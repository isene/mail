//! The files hanging off a message.
//!
//! Same multipart walk [`crate::mime`] uses to find the text, reading the
//! other way: every part that names a file is one of these.
//!
//! Listing and fetching are separate on purpose. A phone wants the names
//! and sizes to draw a row each, and the bytes only for the one that gets
//! tapped — handing every megabyte across an FFI boundary to render a
//! list would be a poor trade.

use crate::mime::{base64_decode, decode_rfc2047, decode_qp_bytes_body};

/// One attachment, without its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    /// Decoded size in bytes.
    pub size: u64,
}

/// Every part of a message, flattened. `(headers, body)`, both still raw.
fn parts(raw: &str, depth: usize, out: &mut Vec<(String, String)>) {
    if depth > 5 { return; }
    let boundary = match find_boundary(raw) { Some(b) => b, None => return };
    let delimiter = format!("--{}", boundary);
    for part in raw.split(&delimiter).skip(1) {
        let at = match part.find("\n\n").map(|p| p + 2)
            .or_else(|| part.find("\r\n\r\n").map(|p| p + 4))
        { Some(v) => v, None => continue };
        let (headers, body) = (&part[..at], &part[at..]);
        if headers.to_lowercase().contains("multipart/") {
            parts(part, depth + 1, out);
        } else {
            out.push((headers.to_string(), body.to_string()));
        }
    }
}

fn find_boundary(raw: &str) -> Option<String> {
    let first = raw.lines().find(|l| !l.trim().is_empty());
    if first.map(|l| l.starts_with("--") && l.len() > 5).unwrap_or(false) {
        return Some(first?[2..].trim_end_matches("--").trim().to_string());
    }
    let pos = raw.find("boundary=")?;
    let rest = &raw[pos + 9..];
    let b = rest.trim_start_matches('"').split('"').next()
        .or_else(|| rest.split_whitespace().next())
        .unwrap_or("");
    // An unquoted boundary runs to the end of the parameter.
    let b = b.split(|c| c == ';' || c == '\r' || c == '\n').next().unwrap_or("").trim();
    if b.is_empty() { None } else { Some(b.to_string()) }
}

/// The filename a part declares, if any. `Content-Disposition: …
/// filename=` first, then `Content-Type: … name=` — some senders only
/// set the latter.
fn filename_of(headers: &str) -> Option<String> {
    let unfolded = headers.replace("\r\n ", " ").replace("\r\n\t", " ")
        .replace("\n ", " ").replace("\n\t", " ");
    let lower = unfolded.to_lowercase();
    for key in ["filename*=", "filename=", "name*=", "name="] {
        let Some(at) = lower.find(key) else { continue };
        let rest = &unfolded[at + key.len()..];
        let raw = if rest.starts_with('"') {
            rest[1..].split('"').next().unwrap_or("")
        } else {
            rest.split(|c| c == ';' || c == '\r' || c == '\n').next().unwrap_or("").trim()
        };
        // RFC 2231: `filename*=UTF-8''name%20here`. Only the common
        // single-segment form; a split parameter falls back to raw.
        let raw = match raw.rsplit_once("''") { Some((_, tail)) => &percent_decode(tail), None => raw };
        let name = decode_rfc2047(raw.trim()).trim().to_string();
        if !name.is_empty() { return Some(name); }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn content_type_of(headers: &str) -> String {
    let lower = headers.to_lowercase();
    let Some(at) = lower.find("content-type:") else { return "application/octet-stream".into() };
    headers[at + 13..]
        .split(|c| c == ';' || c == '\r' || c == '\n')
        .next().unwrap_or("").trim().to_string()
}

fn decode(headers: &str, body: &str) -> Vec<u8> {
    let lower = headers.to_lowercase();
    if lower.contains("base64") {
        base64_decode(body.trim()).unwrap_or_default()
    } else if lower.contains("quoted-printable") {
        decode_qp_bytes_body(body)
    } else {
        body.as_bytes().to_vec()
    }
}

/// Every part that names a file, in the order they appear. The index of
/// each is what [`bytes`] takes.
pub fn list(raw: &str) -> Vec<Attachment> {
    let mut found = Vec::new();
    parts(raw, 0, &mut found);
    found.iter().filter_map(|(headers, body)| {
        let filename = filename_of(headers)?;
        Some(Attachment {
            filename,
            mime_type: content_type_of(headers),
            size: decode(headers, body).len() as u64,
        })
    }).collect()
}

/// The contents of one, by its index in [`list`].
pub fn bytes(raw: &str, index: usize) -> Option<Vec<u8>> {
    let mut found = Vec::new();
    parts(raw, 0, &mut found);
    found.iter()
        .filter(|(headers, _)| filename_of(headers).is_some())
        .nth(index)
        .map(|(headers, body)| decode(headers, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
        --b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n\
        --b\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\n\
        Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
        Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8gUERG\r\n\
        --b--\r\n";

    #[test]
    fn a_named_part_is_an_attachment_and_the_text_is_not() {
        let list = list(RAW);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].filename, "report.pdf");
        assert_eq!(list[0].mime_type, "application/pdf");
        assert_eq!(list[0].size, 9);
    }

    #[test]
    fn the_bytes_come_back_decoded() {
        assert_eq!(bytes(RAW, 0).as_deref(), Some(&b"Hello PDF"[..]));
        assert_eq!(bytes(RAW, 1), None);
    }

    #[test]
    fn a_message_with_no_attachments_has_none() {
        assert!(list("Subject: Hi\r\n\r\nJust text.").is_empty());
    }

    #[test]
    fn an_encoded_word_filename_is_decoded() {
        let raw = "Content-Type: multipart/mixed; boundary=b\r\n\r\n\
            --b\r\nContent-Disposition: attachment; \
            filename=\"=?UTF-8?Q?=C3=A5rsrapport.pdf?=\"\r\n\r\ndata\r\n--b--\r\n";
        assert_eq!(list(raw)[0].filename, "årsrapport.pdf");
    }

    #[test]
    fn an_rfc_2231_filename_is_decoded() {
        let raw = "Content-Type: multipart/mixed; boundary=b\r\n\r\n\
            --b\r\nContent-Disposition: attachment; \
            filename*=UTF-8''%C3%A5rsrapport.pdf\r\n\r\ndata\r\n--b--\r\n";
        assert_eq!(list(raw)[0].filename, "årsrapport.pdf");
    }

    #[test]
    fn an_inline_image_still_counts() {
        // Signature logos arrive as inline parts with a name. Showing
        // them beats silently dropping something the sender attached.
        let raw = "Content-Type: multipart/related; boundary=b\r\n\r\n\
            --b\r\nContent-Type: image/png; name=logo.png\r\n\
            Content-Disposition: inline; filename=logo.png\r\n\r\ndata\r\n--b--\r\n";
        assert_eq!(list(raw).len(), 1);
    }
}
