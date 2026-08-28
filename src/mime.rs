//! MIME decoding: what it takes to turn a raw message into text.
//!
//! Every function here was earned the hard way in kastrup — quoted
//! printable that arrives without its header, base64 bodies that trip
//! the QP sniffer because they end in `==`, multipart trees nested five
//! deep, and header blocks that are not header blocks at all. It lives
//! in its own crate so the phone does not have to learn the same
//! lessons a second time.

/// Where the body starts, given text that may or may not still carry
/// its MIME headers.
///
/// Skipping to the first blank line unconditionally is what threw away
/// the opening paragraph of every header-less mail — and in a short
/// reply the opening paragraph IS the reply. The maildir parser strips
/// headers at ingest, so most stored bodies have none, and their first
/// blank line is just a paragraph break.
pub fn body_after_headers(raw: &str) -> usize {
    let (at, after) = match raw.find("\n\n").map(|p| (p, p + 2))
        .or_else(|| raw.find("\r\n\r\n").map(|p| (p, p + 4)))
    {
        Some(v) => v,
        None => return 0,
    };
    // Every line before the blank one has to look like a header (or a
    // folded continuation of one) for this to be a header block.
    let is_headers = raw[..at].lines().all(|l| {
        l.starts_with(' ') || l.starts_with('\t') || l.is_empty()
            || l.split_once(':').map(|(name, _)| {
                // RFC 5322 allows any printable ASCII but colon in a field
                // name. Letters-digits-hyphen was too strict and threw away
                // the whole header block over one real header: Microsoft
                // Information Protection sends `msip_labels:`, and one
                // underscore meant a 25 KB header chain got read as body,
                // which decoded to nothing at all.
                !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_graphic() && c != ':')
            }).unwrap_or(false)
    });
    if is_headers { after } else { 0 }
}

/// Decode quoted-printable encoding: =XX hex escapes, =\n soft line breaks
pub fn decode_quoted_printable(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let input = s.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'=' {
            if i + 1 < input.len() && (input[i + 1] == b'\r' || input[i + 1] == b'\n') {
                // Soft line break
                i += 1;
                if i < input.len() && input[i] == b'\r' { i += 1; }
                if i < input.len() && input[i] == b'\n' { i += 1; }
            } else if i + 2 < input.len() {
                let b1 = input[i + 1];
                let b2 = input[i + 2];
                if b1.is_ascii_hexdigit() && b2.is_ascii_hexdigit() {
                    let hex = [b1, b2];
                    // SAFETY: both bytes are ASCII hex digits -> valid UTF-8
                    let hex_str = std::str::from_utf8(&hex).unwrap();
                    if let Ok(byte) = u8::from_str_radix(hex_str, 16) {
                        bytes.push(byte);
                        i += 3;
                    } else {
                        bytes.push(b'=');
                        i += 1;
                    }
                } else {
                    // Bare `=` not followed by ASCII hex (e.g. preceding a
                    // UTF-8 multi-byte char) — emit literally.
                    bytes.push(b'=');
                    i += 1;
                }
            } else {
                bytes.push(b'=');
                i += 1;
            }
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    // Try UTF-8 first. When that fails the bytes are almost always
    // latin1 / Windows-1252 (the Nordics see plenty of these — bank
    // mailers, .no government letters, etc.). The previous fallback
    // was `from_utf8_lossy`, which substitutes U+FFFD and showed the
    // user `?`-in-a-box wherever the original had `å` / `ø` / `æ`.
    // Latin1 maps one byte → one codepoint with no possible failure
    // and produces sensible text for any 8-bit-encoded payload; the
    // small mismatch between latin1 and Cp1252 (a handful of
    // punctuation glyphs in 0x80..0x9F) is barely visible compared
    // to losing every Norwegian vowel.
    String::from_utf8(bytes).unwrap_or_else(|e| latin1_to_utf8(e.as_bytes()))
}

/// Heuristic: does this look like a quoted-printable payload? Used by
/// the render and yank paths to decide whether to QP-decode a body
/// when the `Content-Transfer-Encoding` header has been stripped by
/// the ingestion stage (kastrup's maildir parser does that). The soft
/// line break `=\n` / `=\r\n` is the cleanest signal and rarely
/// shows up in plain ASCII text; failing that we count `=XX` hex
/// escapes and require a couple before declaring the body QP.
pub fn looks_quoted_printable(s: &str) -> bool {
    if s.contains("=\n") || s.contains("=\r\n") { return true; }
    let bytes = s.as_bytes();
    let mut hits = 0u32;
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'=' && bytes[i+1].is_ascii_hexdigit() && bytes[i+2].is_ascii_hexdigit() {
            hits += 1;
            if hits >= 3 { return true; }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Check if content looks like raw base64 (no MIME headers, just base64 lines).
pub fn looks_base64(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.len() < 20 { return false; }
    // Check first few lines: should be long lines of base64 chars only
    let mut b64_lines = 0;
    for line in trimmed.lines().take(5) {
        let l = line.trim();
        if l.is_empty() { continue; }
        if l.len() < 20 { return false; }
        if l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=') {
            b64_lines += 1;
        } else {
            return false;
        }
    }
    b64_lines >= 2
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let table: [u8; 128] = {
        let mut t = [255u8; 128];
        for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
            t[c as usize] = i as u8;
        }
        t
    };
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for &b in s.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' || b == b' ' { continue; }
        if b >= 128 || table[b as usize] == 255 { continue; }
        buf = (buf << 6) | table[b as usize] as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Convert ISO-8859-1 / Windows-1252 bytes to UTF-8 string.
pub fn latin1_to_utf8(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Decode RFC 2047 encoded-words: =?charset?encoding?text?=
pub fn decode_rfc2047(s: &str) -> String {
    if !s.contains("=?") { return s.to_string(); }
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("=?") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        // Format: charset?encoding?encoded_text?=
        // Find first ? (end of charset), second ? (end of encoding), then ?= (terminator)
        let mut qmarks = Vec::new();
        for (i, b) in after.bytes().enumerate() {
            if b == b'?' { qmarks.push(i); }
            if qmarks.len() >= 3 { break; }
        }
        // Need at least 2 '?' for charset?encoding?, then find ?= after the encoded text
        if qmarks.len() >= 2 {
            let charset_end = qmarks[0];
            let enc_end = qmarks[1];
            let _charset = &after[..charset_end];
            let encoding = &after[charset_end + 1..enc_end];
            let text_start = enc_end + 1;
            // Find ?= after the encoded text
            if let Some(term) = after[text_start..].find("?=") {
                let encoded = &after[text_start..text_start + term];
                let decoded_bytes = match encoding.to_lowercase().as_str() {
                    "b" => base64_decode(encoded),
                    "q" => Some(decode_qp_bytes(encoded)),
                    _ => None,
                };
                if let Some(bytes) = decoded_bytes {
                    let text = String::from_utf8(bytes.clone())
                        .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect());
                    result.push_str(&text);
                } else {
                    result.push_str(&rest[start..start + 2 + text_start + term + 2]);
                }
                rest = &after[text_start + term + 2..];
                // RFC 2047 6.2: whitespace between two adjacent encoded
                // words is part of neither and is dropped. Between an
                // encoded word and ordinary text it is real text and stays.
                //
                // The run can be anything the folding used: a space, a tab,
                // or a CRLF and one of those. Matching only " ", "\n " and
                // "\r\n " left a tab-folded header with a gap in the middle
                // of a word.
                let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
                if trimmed.len() != rest.len() && trimmed.starts_with("=?") {
                    rest = trimmed;
                }
            } else {
                result.push_str("=?");
                rest = after;
            }
        } else {
            result.push_str("=?");
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

/// Extract readable text from raw MIME multipart content.
/// CRLF is a wire convention, not something a reader wants to see.
pub fn normalize_line_endings(s: String) -> String {
    if !s.contains('\r') { return s; }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// The readable text of a MIME message.
///
/// A `text/calendar` part is handed back as its raw iCalendar text. If
/// you would rather show an invite as something a human reads, use
/// [`extract_mime_text_with`] and pass a renderer — the shape of that
/// rendering is a matter of taste and of what the display can do, so it
/// does not belong in here.
/// A message that has headers but no boundary: one part, its encoding
/// declared in its own headers.
///
/// This is the shape a full RFC822 message arrives in when nobody has
/// split it first — an IMAP client handing over what the server sent.
/// The multipart walk finds no boundary in it and rightly gives up, so
/// something has to decode the single part, and this is it.
///
/// Returns `None` when there is no header block to speak of, leaving the
/// caller's own heuristics in charge.
pub fn decode_single_part(raw: &str) -> Option<String> {
    let at = body_after_headers(raw);
    if at == 0 { return None; }
    let headers = raw[..at].to_lowercase();
    let body = &raw[at..];

    let is_latin1 = headers.contains("iso-8859") || headers.contains("windows-1252");
    let decoded = if headers.contains("content-transfer-encoding: base64") {
        decode_body_bytes(&base64_decode(body.trim()).unwrap_or_default(), is_latin1)
    } else if headers.contains("content-transfer-encoding: quoted-printable") {
        decode_body_bytes(&decode_qp_bytes_body(body), is_latin1)
    } else if is_latin1 {
        latin1_to_utf8(body.as_bytes())
    } else {
        body.to_string()
    };

    // An HTML-only message is still one part; hand back readable text.
    if headers.contains("content-type: text/html") {
        return Some(crate::html::html_to_text(&decoded));
    }
    Some(decoded)
}

pub fn extract_mime_text(raw: &str) -> Option<String> {
    extract_mime_text_depth(raw, 0, &|ical| ical.to_string())
}

/// As [`extract_mime_text`], with your own renderer for `text/calendar`
/// parts.
pub fn extract_mime_text_with(raw: &str, ical: &dyn Fn(&str) -> String) -> Option<String> {
    extract_mime_text_depth(raw, 0, ical)
}

fn extract_mime_text_depth(raw: &str, depth: usize, ical: &dyn Fn(&str) -> String) -> Option<String> {
    if depth > 5 { return None; }
    // Detect MIME boundary: prefer first "--" line if content starts with one,
    // otherwise use boundary= attribute, fallback to first "--" line anywhere.
    let first_line = raw.lines().find(|l| !l.trim().is_empty());
    let boundary = if first_line.map(|l| l.starts_with("--") && l.len() > 5).unwrap_or(false) {
        // Content starts with a boundary line: use it as the primary boundary
        first_line.unwrap()[2..].trim_end_matches("--").trim().to_string()
    } else if let Some(pos) = raw.find("boundary=") {
        // RFC 2045: the value is either quoted, and ends at the closing
        // quote, or a bare token, and ends at a semicolon or whitespace.
        //
        // Reading an unquoted one as if it were quoted took everything up
        // to the next `"` anywhere in the message — thousands of
        // characters, matching no line, so the walk found no parts at all
        // and the reader got the raw MIME. Some senders quote the value,
        // some do not, and one that does not is not malformed.
        let rest = &raw[pos + 9..];
        let b = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or(""),
            None => rest
                .split(|c: char| c == ';' || c.is_whitespace())
                .next()
                .unwrap_or(""),
        };
        if b.is_empty() { return None; }
        b.to_string()
    } else {
        raw.lines()
            .find(|l| l.starts_with("--") && l.len() > 5)
            .map(|l| l[2..].trim_end_matches(':').trim().to_string())?
    };

    let delimiter = format!("--{}", boundary);
    let parts: Vec<&str> = raw.split(&delimiter).collect();

    // Find text/plain part first, fall back to text/html, then text/calendar
    let mut text_part = None;
    let mut html_part = None;
    let mut cal_part = None;
    for (i, part) in parts.iter().enumerate() {
        if let Some(header_end) = part.find("\n\n").or_else(|| part.find("\r\n\r\n")) {
            let headers = &part[..header_end];
            let body_start = if part[header_end..].starts_with("\r\n\r\n") { header_end + 4 } else { header_end + 2 };
            let body = &part[body_start..];
            let headers_lower = headers.to_lowercase();
            let is_qp = headers_lower.contains("quoted-printable");
            let is_b64 = headers_lower.contains("base64");

            // Detect charset for proper decoding
            let is_latin1 = headers_lower.contains("iso-8859") || headers_lower.contains("windows-1252");

            // Recurse into nested multipart parts — but never into the
            // first, which is whatever came before the first delimiter:
            // the message's own headers. It says `multipart/` because
            // this message is, so recursing on it re-derived the same
            // boundary and walked the same bytes again, five deep, on
            // every multipart message opened.
            if i > 0 && headers_lower.contains("multipart/") {
                if let Some(result) = extract_mime_text_depth(part, depth + 1, ical) {
                    if text_part.is_none() { text_part = Some(result); }
                }
                continue;
            }

            // A forwarded message travels as a whole message, headers and
            // all. It is an attachment, not this message's body — and its
            // own Content-Type lines name text/plain, so anything looking
            // for a type across the part rather than in its headers
            // adopted the whole thing and printed a page of Received:.
            if headers_lower.contains("message/rfc822") {
                continue;
            }

            if headers_lower.contains("text/plain") {
                let decoded = if is_qp {
                    let bytes = decode_qp_bytes_body(body);
                    decode_body_bytes(&bytes, is_latin1)
                } else if is_b64 {
                    let bytes = base64_decode(body.trim()).unwrap_or_default();
                    decode_body_bytes(&bytes, is_latin1)
                } else { body.to_string() };
                if !decoded.trim().is_empty() { text_part = Some(decoded); }
            } else if headers_lower.contains("text/html") {
                let decoded = if is_qp {
                    let bytes = decode_qp_bytes_body(body);
                    decode_body_bytes(&bytes, is_latin1)
                } else if is_b64 {
                    let bytes = base64_decode(body.trim()).unwrap_or_default();
                    decode_body_bytes(&bytes, is_latin1)
                } else { body.to_string() };
                html_part = Some(decoded);
            } else if headers_lower.contains("text/calendar") && cal_part.is_none() {
                let decoded = if is_b64 {
                    base64_decode(body.trim())
                        .and_then(|b| String::from_utf8(b).ok())
                        .unwrap_or_default()
                } else {
                    body.to_string()
                };
                if !decoded.is_empty() { cal_part = Some(ical(&decoded)); }
            }
        }
    }

    // Skip text/plain if it's just a "your client doesn't support HTML" fallback
    let text_is_fallback = text_part.as_ref().map(|t| {
        let lower = t.to_lowercase();
        lower.contains("html-e-poster") || lower.contains("html e-post")
            || lower.contains("doesn't support html") || lower.contains("does not support html")
            || lower.contains("not displayed") || lower.contains("html messages are not support")
            || (t.trim().lines().count() <= 3 && html_part.is_some())
    }).unwrap_or(false);

    let effective_text = if text_is_fallback { None } else { text_part };

    // If text_part contains HTML entities, decode them
    let body = effective_text
        .map(|t| {
            let has_entities = regex::Regex::new(r"&[a-zA-Z]+;|&#\d+;|&#x[0-9a-fA-F]+;")
                .map(|re| re.is_match(&t)).unwrap_or(false);
            if has_entities {
                crate::html::html_to_text(&t)
            } else { t }
        })
        .or_else(|| html_part.map(|h| crate::html::html_to_text(&h)).filter(|t| !t.trim().is_empty()));

    // Normalise line endings on the extracted body. Legacy clients (and
    // some receipt-system mailers, e.g. Mitt Dekkhotell) emit CR-only
    // line terminators inside a base64 text/plain part. After base64
    // decode the bare `\r` bytes survive into the string, and Rust's
    // `str::lines()` only splits on `\n` or `\r\n` — so the whole body
    // is treated as ONE logical line. When that line is later printed
    // via the right pane's positioning, each embedded `\r` makes the
    // terminal cursor jump to column 1 of the row, overwriting the
    // adjacent left pane with body text. Convert CRLF → LF first, then
    // any remaining bare CR → LF.
    let body = body.map(normalize_line_endings);
    // When this is a calendar invite (text/calendar part present), put the
    // structured summary on top followed by the plain-text body. That way
    // the user sees "Title / When / Where / Organizer" first and the
    // Teams/Zoom join block below.
    match (cal_part, body) {
        (Some(cal), Some(text)) => Some(format!("{}\n\n---\n\n{}", cal, text)),
        (Some(cal), None)       => Some(cal),
        (None,      Some(text)) => Some(text),
        (None,      None)       => None,
    }
}


fn decode_qp_bytes(s: &str) -> Vec<u8> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' {
            result.push(b' ');
            i += 1;
        } else if bytes[i] == b'=' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i+1..i+3]).unwrap_or(""), 16) {
                result.push(b);
                i += 3;
            } else {
                result.push(bytes[i]);
                i += 1;
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    result
}

/// Decode quoted-printable to raw bytes (for charset-aware conversion).
pub(crate) fn decode_qp_bytes_body(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(s.len());
    let input = s.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'=' {
            if i + 1 < input.len() && (input[i + 1] == b'\r' || input[i + 1] == b'\n') {
                i += 1;
                if i < input.len() && input[i] == b'\r' { i += 1; }
                if i < input.len() && input[i] == b'\n' { i += 1; }
            } else if i + 2 < input.len() {
                let b1 = input[i + 1];
                let b2 = input[i + 2];
                if b1.is_ascii_hexdigit() && b2.is_ascii_hexdigit() {
                    let hex = [b1, b2];
                    // SAFETY: both bytes are ASCII hex digits -> valid UTF-8
                    let hex_str = std::str::from_utf8(&hex).unwrap();
                    if let Ok(byte) = u8::from_str_radix(hex_str, 16) {
                        bytes.push(byte);
                        i += 3;
                    } else {
                        bytes.push(b'=');
                        i += 1;
                    }
                } else {
                    // Bare `=` not followed by ASCII hex (e.g. preceding a
                    // UTF-8 multi-byte char) — emit literally.
                    bytes.push(b'=');
                    i += 1;
                }
            } else {
                bytes.push(b'=');
                i += 1;
            }
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    bytes
}

/// Decode a body byte buffer using the declared MIME charset, but
/// don't blindly trust the declaration. Many senders mark UTF-8
/// content as `charset=iso-8859-1` or `windows-1252` (this is
/// rampant on transactional mail). Strategy:
///
/// 1. If `declared_latin1` is false (charset says UTF-8 or wasn't
///    set), interpret as UTF-8, lossy-decode on error.
/// 2. If `declared_latin1` is true, FIRST try strict UTF-8. If
///    the bytes happen to be valid UTF-8 that's almost certainly
///    what they really are — Norwegian "påminnelse" (UTF-8
///    `0xC3 0xA5`) would otherwise come through as the mojibake
///    `pÃ¥minnelse` after a literal latin1→utf-8 lift.
/// 3. Only fall through to `latin1_to_utf8` when strict UTF-8
///    fails, i.e. the bytes are genuinely 8-bit Latin-1.
fn decode_body_bytes(bytes: &[u8], declared_latin1: bool) -> String {
    if declared_latin1 {
        if let Ok(s) = std::str::from_utf8(bytes) {
            return s.to_string();
        }
        return latin1_to_utf8(bytes);
    }
    String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {

    /// A boundary the sender did not quote. Read as if it were quoted it
    /// swallowed the rest of the message, matched no line, and the walk
    /// came back with nothing — so the reader was shown the raw MIME,
    /// part headers and `=0A` and all.
    #[test]
    fn an_unquoted_boundary_is_still_a_boundary() {
        let raw = "Content-Type: multipart/alternative;\r\n\
                   \x20boundary=--b_7496591_500c2b51\r\n\
                   \r\n\
                   ----b_7496591_500c2b51\r\n\
                   Content-Type: text/plain; charset=utf-8\r\n\
                   Content-Transfer-Encoding: quoted-printable\r\n\
                   \r\n\
                   Aktiver kortet ditt=0A\r\n\
                   ----b_7496591_500c2b51\r\n\
                   Content-Type: text/html; charset=utf-8\r\n\
                   \r\n\
                   <p>Aktiver kortet ditt</p>\r\n\
                   ----b_7496591_500c2b51--\r\n";
        let got = extract_mime_text(raw).unwrap_or_default();
        assert!(got.contains("Aktiver kortet ditt"), "got: {}", got);
        // None of the machine format, and the quoted-printable decoded.
        assert!(!got.contains("Content-Type"), "got: {}", got);
        assert!(!got.contains("=0A"), "got: {}", got);
    }

    /// The quoted form keeps working, and the value stops at the quote
    /// rather than running on into the next parameter.
    #[test]
    fn a_quoted_boundary_ends_at_the_quote() {
        let raw = "Content-Type: multipart/alternative; boundary=\"b1\"; charset=utf-8\r\n\
                   \r\n\
                   --b1\r\n\
                   Content-Type: text/plain\r\n\
                   \r\n\
                   Hello there\r\n\
                   --b1--\r\n";
        assert_eq!(extract_mime_text(raw).unwrap_or_default().trim(), "Hello there");
    }

    /// A forward carries the original as a `message/rfc822` part —
    /// headers and all, and its own Content-Type lines say text/plain.
    /// Reading the type off the whole part instead of its headers meant
    /// the walk adopted it, and the body came out as a page of
    /// `Received:` lines.
    #[test]
    fn an_embedded_message_is_not_the_body() {
        let raw = "Content-Type: multipart/mixed; boundary=\"b\"\n\
                   \n\
                   --b\n\
                   Content-Type: text/plain; charset=us-ascii\n\
                   \n\
                   Here is the brief you asked for.\n\
                   \n\
                   --b\n\
                   Content-Type: message/rfc822\n\
                   Content-Disposition: attachment\n\
                   \n\
                   Received: from mail.example.com (10.0.0.1)\n\
                   From: someone@example.com\n\
                   Content-Type: text/plain; charset=utf-8\n\
                   \n\
                   The forwarded text.\n\
                   --b--\n";
        let got = extract_mime_text(raw).unwrap_or_default();
        assert!(got.contains("Here is the brief"), "got: {}", got);
        assert!(!got.contains("Received:"), "got: {}", got);
    }

    #[test]
    fn an_underscore_in_a_header_name_is_still_a_header() {
        // `msip_labels` is a real header (Microsoft Information
        // Protection). Rejecting it made the whole block look like body.
        let raw = "Subject: Hi\r\nmsip_labels:\r\nContent-Type: text/plain\r\n\r\nThe body.";
        assert!(body_after_headers(raw) > 0);
        assert_eq!(&raw[body_after_headers(raw)..], "The body.");
    }

    #[test]
    fn prose_with_a_colon_is_not_mistaken_for_headers() {
        let raw = "Hei Geir, se her: noe\n\nMvh";
        assert_eq!(body_after_headers(raw), 0);
    }
    use super::*;

    #[test]
    fn a_header_block_is_skipped_but_a_paragraph_is_not() {
        let mail = "Content-Type: text/plain\nX-Folded: one\n\ttwo\n\nHei\n";
        assert_eq!(&mail[body_after_headers(mail)..], "Hei\n");
        let bare = "Hei Geir\nDet ser greit ut.\n\nMvh\n";
        assert_eq!(body_after_headers(bare), 0, "prose is not a header block");
    }

    #[test]
    fn soft_breaks_and_hex_escapes_decode() {
        let out = decode_quoted_printable("s=C3=A5 da skal=20\ndet v=C3=A6re =\nher\n");
        assert!(out.contains("så da skal"));
        assert!(out.contains("være here") || out.contains("være her"), "got {:?}", out);
    }

    #[test]
    fn base64_is_recognised_before_quoted_printable() {
        // A base64 payload ends in "==", which the QP sniffer would
        // otherwise claim as a hex escape. The sniffer wants two long
        // lines of base64 alphabet, which is what a real body looks
        // like — a short one-liner is more likely to be prose.
        let b64 = "SGVpIEdlaXIsIGRldCBnw6VyIGJyYS4gSGVpIEdlaXIsIGRldCBnw6VyIGJyYS4gSGVpIEdlaXIs\nIGRldCBnw6VyIGJyYS4gSGVpIEdlaXIsIGRldCBnw6VyIGJyYS4gSGVpIEdlaXIsIGRldCBnw6Vy\nIGJyYS4gSGVpIEdlaXIsIGRldCBnw6VyIGJyYS4g";
        assert!(looks_base64(b64));
        let bytes = base64_decode(b64).unwrap();
        assert!(String::from_utf8(bytes).unwrap().starts_with("Hei Geir, det går bra."));
        assert!(!looks_base64("SGVpIEdlaXI="), "too short to be a body");
    }

    #[test]
    fn encoded_words_in_headers_come_back_as_text() {
        assert_eq!(decode_rfc2047("=?UTF-8?B?QXJ2ZW9wcGdqw7hy?="), "Arveoppgjør");
        assert_eq!(decode_rfc2047("=?iso-8859-1?Q?Fw=3A_M=F8te?="), "Fw: Møte");
        assert_eq!(decode_rfc2047("plain subject"), "plain subject");
    }

    #[test]
    fn multipart_alternative_prefers_the_text_part() {
        let raw = "Content-Type: multipart/alternative; boundary=\"BB\"\n\
                   \n\
                   --BB\n\
                   Content-Type: text/plain; charset=utf-8\n\
                   \n\
                   The plain one.\n\
                   It runs to several lines,\n\
                   because three or fewer\n\
                   would be read as a stub.\n\
                   --BB\n\
                   Content-Type: text/html; charset=utf-8\n\
                   \n\
                   <p>The HTML one.</p>\n\
                   --BB--\n";
        let out = extract_mime_text(raw).unwrap();
        assert!(out.contains("The plain one"), "got {:?}", out);
        assert!(!out.contains("The HTML one"));
    }

    /// A surprise worth pinning: alongside an HTML alternative, a text
    /// part of three lines or fewer is taken for a stub — the "this
    /// message is in HTML" placeholder — and the HTML wins. It is the
    /// right call for the mail that motivated it, and it does mean a
    /// genuinely terse plain part gets passed over.
    #[test]
    fn a_stub_text_part_yields_to_the_html() {
        let raw = "Content-Type: multipart/alternative; boundary=\"BB\"\n\
                   \n\
                   --BB\n\
                   Content-Type: text/plain; charset=utf-8\n\
                   \n\
                   See HTML.\n\
                   --BB\n\
                   Content-Type: text/html; charset=utf-8\n\
                   \n\
                   <p>The real message.</p>\n\
                   --BB--\n";
        let out = extract_mime_text(raw).unwrap();
        assert!(out.contains("The real message"), "got {:?}", out);
    }

    #[test]
    fn a_calendar_part_is_the_callers_business() {
        let raw = "Content-Type: multipart/mixed; boundary=\"BB\"\n\
                   \n\
                   --BB\n\
                   Content-Type: text/calendar; method=REQUEST\n\
                   \n\
                   BEGIN:VCALENDAR\nSUMMARY:Standup\nEND:VCALENDAR\n\
                   --BB--\n";
        let plain = extract_mime_text(raw).unwrap_or_default();
        assert!(plain.contains("SUMMARY:Standup"), "raw ical by default: {:?}", plain);
        let rendered = extract_mime_text_with(raw, &|_| "[Invite]".to_string()).unwrap_or_default();
        assert!(rendered.contains("[Invite]"), "the caller decides: {:?}", rendered);
    }
}

#[cfg(test)]
mod adjacent_word_tests {
    use super::decode_rfc2047;

    const WANT: &str = "Fw: Dualogの脆弱性対応に関するアンケート回答のお願い";
    const W1: &str = "=?utf-8?B?Rnc6IER1YWxvZ+OBruiEhuW8seaAp+WvvuW/nOOBq+mWouOBmeOCi+OCog==?=";
    const W2: &str = "=?utf-8?B?44Oz44Kx44O844OI5Zue562U44Gu44GK6aGY44GE?=";

    #[test]
    fn adjacent_encoded_words_join_with_nothing_between() {
        // However the header was unfolded, the two words are adjacent, so
        // RFC 2047 6.2 says the whitespace between them is not part of
        // either and has to go.
        for (label, sep) in [
            ("space", " "),
            ("newline + space", "\n "),
            ("crlf + space", "\r\n "),
            ("tab", "\t"),
            ("newline + tab", "\n\t"),
            ("two spaces", "  "),
            ("nothing", ""),
        ] {
            let got = decode_rfc2047(&format!("{}{}{}", W1, sep, W2));
            assert_eq!(got, WANT, "separator: {}", label);
        }
    }

    #[test]
    fn a_gap_before_ordinary_text_is_kept() {
        // The other half of the rule: whitespace between an encoded word
        // and a plain word IS part of the text.
        assert_eq!(decode_rfc2047("=?utf-8?B?SGVsbG8=?= world"), "Hello world");
        assert_eq!(decode_rfc2047("plain =?utf-8?B?SGVsbG8=?="), "plain Hello");
    }
}
