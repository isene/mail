//! Email plumbing for the Fe₂O₃ suite.
//!
//! The parts of reading mail that are pure logic, so the desktop
//! ([kastrup](https://github.com/isene/kastrup)) and the phone
//! ([nomad](https://github.com/isene/nomad)) can share one
//! implementation instead of earning the same bugs twice:
//!
//! * [`mime`] — quoted-printable, base64, RFC 2047 headers, latin-1
//!   rescue, and the multipart walk that finds the text among the
//!   alternatives and attachments.
//! * [`html`] — HTML mail reduced to readable text.
//! * [`attach`] — the files hanging off a message, listed cheaply and
//!   fetched one at a time.
//! * [`read_state`] — which messages have been read, merged across
//!   devices under a rule that keeps the laptop authoritative.
//!
//! No I/O and no platform APIs: give it bytes, get back text.

pub mod attach;
pub mod html;
pub mod mime;
pub mod read_state;

pub use html::html_to_text;
pub use mime::{
    base64_decode, body_after_headers, decode_quoted_printable, decode_rfc2047,
    extract_mime_text, latin1_to_utf8, looks_base64, looks_quoted_printable,
};

/// The readable text of a message body, whatever it arrived as.
///
/// Order matters and is not obvious. MIME first, because a multipart
/// body's own encoding headers live inside its parts. Then base64
/// BEFORE quoted-printable, because a base64 payload ends in `==\n`
/// and trips the QP sniffer. Finally HTML, if that is all there is.
///
/// `html_fallback` is the `html_content` a store may keep alongside the
/// text part; it is used only when the text part turns out to be empty
/// or one of those "this message is in HTML" stubs.
pub fn body_text(raw: &str, html_fallback: Option<&str>) -> String {
    let looks_mime = raw.contains("Content-Type:")
        || raw.lines().any(|l| l.starts_with("--") && l.len() > 5);

    let text = if looks_mime {
        // The walk first. When it finds no boundary this is not multipart
        // at all — a whole RFC822 message with one part, which is what an
        // IMAP client hands over. Decode that part by its own headers
        // rather than showing nothing, which is what a caller passing
        // full messages used to get.
        mime::extract_mime_text(raw)
            .filter(|t| !t.trim().is_empty())
            .or_else(|| mime::decode_single_part(raw))
            // An attachment-only mail yields nothing; an empty body beats
            // dumping raw MIME at the reader.
            .unwrap_or_default()
    } else if mime::looks_base64(raw) {
        match mime::base64_decode(raw.trim()) {
            Some(bytes) => String::from_utf8(bytes.clone())
                .unwrap_or_else(|_| mime::latin1_to_utf8(&bytes)),
            None => raw.to_string(),
        }
    } else if raw.contains("Content-Transfer-Encoding: quoted-printable")
        || mime::looks_quoted_printable(raw)
    {
        mime::decode_quoted_printable(&raw[mime::body_after_headers(raw)..])
    } else {
        raw.to_string()
    };

    if let Some(html) = html_fallback {
        let lc = text.to_lowercase();
        let is_stub = text.trim().is_empty()
            || text.trim().len() < 20
            || lc.contains("html messages are not support")
            || lc.contains("not displayed")
            || lc.contains("html-e-post")
            || lc.contains("støtter ikke html")
            || lc.contains("does not support html");
        if is_stub {
            return html::html_to_text(html);
        }
    }
    if text.contains("<br") || text.contains("<p>") || text.contains("<p ")
        || (text.trim_start().starts_with('<') && (text.contains("<html") || text.contains("<body")))
    {
        return html::html_to_text(&text);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_body_survives_untouched() {
        assert_eq!(body_text("Hei\n\nMvh\n", None), "Hei\n\nMvh\n");
    }

    #[test]
    fn a_headerless_qp_body_keeps_its_first_paragraph() {
        // The bug that ate the substance of a short reply: skipping to
        // the first blank line as if it were a header block.
        let raw = "Hei Geir\nDet ser greit ut. Mitt kontonr. 1234 =\n56 78901\n\nMvh\n";
        let out = body_text(raw, None);
        assert!(out.starts_with("Hei Geir"), "got {:?}", out);
        assert!(out.contains("1234 56 78901"), "soft break rejoined: {:?}", out);
    }

    #[test]
    fn an_html_only_mail_falls_back_to_the_html() {
        let out = body_text("", Some("<p>Hello</p><p>There</p>"));
        assert!(out.contains("Hello"));
        assert!(out.contains("There"));
    }

    #[test]
    fn a_whole_message_with_one_base64_part_still_reads() {
        // The shape an IMAP client hands over: headers and body together,
        // no multipart boundary anywhere. The Content-Type header made
        // this look like MIME, the walk found no boundary, and the reader
        // got a blank screen.
        let body = "SGVpIMOlIGRlcgpNdmg=";   // "Hei å der\nMvh"
        let raw = format!(
            "From: A <a@example.com>\r\nSubject: Hi\r\n\
             Content-Type: text/plain; charset=\"utf-8\"\r\n\
             Content-Transfer-Encoding: base64\r\nMIME-Version: 1.0\r\n\r\n{}",
            body,
        );
        let out = body_text(&raw, None);
        assert!(out.contains("Hei å der"), "got {:?}", out);
    }

    #[test]
    fn a_whole_message_with_one_qp_part_still_reads() {
        let raw = "Subject: Hi\r\nContent-Type: text/plain\r\n\
                   Content-Transfer-Encoding: quoted-printable\r\n\r\n\
                   Hei =C3=A5 der";
        assert!(body_text(raw, None).contains("Hei å der"));
    }

    #[test]
    fn a_multipart_message_is_still_walked() {
        // The single-part fallback must not steal multipart's job. Four
        // lines, because three or fewer beside an HTML alternative is
        // taken for a stub — see the module docs.
        let raw = "Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n\
                   --b\r\nContent-Type: text/plain\r\n\r\n\
                   The plain part.\r\nWith a second line.\r\n\
                   And a third.\r\nAnd a fourth, so it is no stub.\r\n\
                   --b\r\nContent-Type: text/html\r\n\r\n<p>ignored</p>\r\n--b--\r\n";
        let out = body_text(raw, None);
        assert!(out.contains("The plain part"), "got {:?}", out);
        assert!(!out.contains("ignored"));
    }

    #[test]
    fn a_real_text_part_beats_the_html_alternative() {
        let out = body_text("A proper plain text body, long enough to count.", Some("<p>ignored</p>"));
        assert!(out.starts_with("A proper plain text"));
        assert!(!out.contains("ignored"));
    }
}
