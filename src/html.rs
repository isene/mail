//! HTML mail, flattened to something a terminal or a phone can show.
//!
//! Not a renderer: a reduction. Tags that carry meaning for reading
//! prose (paragraphs, breaks, list items, links) become layout, and
//! everything else goes away.

pub fn html_to_text(html: &str) -> String {
    // Strip elements that a browser would render as invisible (CSS
    // display:none, opacity:0, max-height:0, visibility:hidden). Substack
    // and other newsletters stuff these with preview-padding chars
    // (soft-hyphen, combining grapheme joiner, NBSP) which otherwise leak
    // into the pane as "-?" grids. Regex is deliberately narrow — matches
    // a single <div|span|…> with the offending style and no nested
    // element of the same type inside it (`[^<]` forbids `<` in the body).
    use std::sync::OnceLock;
    static HIDDEN_RE: OnceLock<regex::Regex> = OnceLock::new();
    let hidden_re = HIDDEN_RE.get_or_init(|| {
        // Rust's regex crate doesn't support backreferences, so we match
        // any open/close pair from the same set of element names instead
        // of pinning the close tag to the open tag. The body is
        // constrained to `[^<]*` so nested HTML can't sneak in.
        regex::Regex::new(
            r#"(?is)<(?:div|span|p|td|tr|table|section)\b[^>]*\bstyle\s*=\s*"[^"]*\b(?:display\s*:\s*none|visibility\s*:\s*hidden|opacity\s*:\s*0(?:\.0+)?|max-height\s*:\s*0(?:px)?|max-width\s*:\s*0(?:px)?)[^"]*"[^>]*>[^<]*</\s*(?:div|span|p|td|tr|table|section)\s*>"#
        ).expect("hidden-element regex should compile")
    });
    let stripped = hidden_re.replace_all(html, "").into_owned();

    // Convert `<table>` blocks to Markdown BEFORE the generic tag strip so
    // structure survives. format_markdown_tables will lay them out.
    // Guard against recursion — cell_html_to_text re-enters html_to_text on
    // already-stripped cell HTML; skip the table pass when there's nothing
    // to do.
    let html_owned;
    let html: &str = if stripped.contains("<table") || stripped.contains("<TABLE") {
        html_owned = html_tables_to_markdown(&stripped);
        &html_owned
    } else {
        html_owned = stripped;
        &html_owned
    };
    let mut result = String::new();
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_block = false;

    // ASCII folding, not Unicode: `to_lowercase` can change a string's
    // length (İ becomes two chars), and every offset found in this copy
    // is then used to index the original. Tag names are ASCII, which is
    // all these searches look for, and ASCII folding is byte-for-byte
    // aligned with the source.
    let lower = html.to_ascii_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if in_tag {
            if chars[i] == '>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }

        if chars[i] == '<' {
            let rest: String = lower_chars[i..].iter().take(20).collect();
            if rest.starts_with("<script") { in_script = true; }
            if rest.starts_with("</script") { in_script = false; }
            if rest.starts_with("<style") { in_style = true; }
            if rest.starts_with("</style") { in_style = false; }

            if rest.starts_with("<br") || rest.starts_with("<p")
                || rest.starts_with("</p") || rest.starts_with("<div")
                || rest.starts_with("</div") || rest.starts_with("<li")
                || rest.starts_with("<tr") || rest.starts_with("<h1")
                || rest.starts_with("<h2") || rest.starts_with("<h3")
                || rest.starts_with("<h4") || rest.starts_with("<h5")
                || rest.starts_with("<h6")
            {
                if !last_was_block {
                    result.push('\n');
                    last_was_block = true;
                }
            }

            in_tag = true;
            i += 1;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        // HTML entity decoding
        if chars[i] == '&' {
            // Find the entity (up to ';')
            let entity_end = chars[i..].iter().take(12).position(|&c| c == ';');
            if let Some(end) = entity_end {
                let entity: String = chars[i..i + end + 1].iter().collect();
                let decoded = decode_html_named_entity(entity.as_str());
                if let Some(c) = decoded {
                    if !is_invisible_format_char(c) { result.push(c); }
                    i += end + 1;
                    continue;
                }
            }
            // Numeric entities: &#NNN; or &#xHHH;
            if let Some(end) = entity_end {
                let entity: String = chars[i..i + end + 1].iter().collect();
                if entity.starts_with("&#") {
                    let num_str = &entity[2..entity.len() - 1];
                    let code = if num_str.starts_with('x') || num_str.starts_with('X') {
                        u32::from_str_radix(&num_str[1..], 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(c) = code.and_then(char::from_u32) {
                        if !is_invisible_format_char(c) { result.push(c); }
                        i += end + 1;
                        continue;
                    }
                }
            }
        }

        last_was_block = false;
        if !is_invisible_format_char(chars[i]) {
            result.push(chars[i]);
        }
        i += 1;
    }

    // Clean up: collapse multiple blank lines, trim
    let mut cleaned = String::new();
    let mut blank_count = 0;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_count += 1;
            if blank_count <= 2 { cleaned.push('\n'); }
        } else {
            blank_count = 0;
            cleaned.push_str(trimmed);
            cleaned.push('\n');
        }
    }
    cleaned
}
/// Find every `<table>…</table>` in `html` and replace it with an equivalent
/// Markdown table. The downstream html_to_text strips what's left; the
/// downstream format_markdown_tables then lays our Markdown out as a
/// Unicode-box block.
fn html_tables_to_markdown(html: &str) -> String {
    // ASCII folding: offsets from this copy index `html`. See html_to_text.
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    while let Some(rel_start) = lower[cursor..].find("<table") {
        let start = cursor + rel_start;
        // Find the matching </table> allowing nested tables (rare but possible).
        let mut depth = 1usize;
        let mut scan = start + 6;
        let end = loop {
            let next_open = lower[scan..].find("<table").map(|p| scan + p);
            let next_close = lower[scan..].find("</table>").map(|p| scan + p);
            match (next_open, next_close) {
                (Some(o), Some(c)) if o < c => { depth += 1; scan = o + 6; }
                (_, Some(c)) => {
                    depth -= 1;
                    if depth == 0 { break c + 8; } // include "</table>"
                    scan = c + 8;
                }
                _ => return out + &html[cursor..],
            }
        };
        out.push_str(&html[cursor..start]);
        let block = &html[start..end];
        out.push('\n');
        out.push_str(&table_block_to_markdown(block));
        out.push('\n');
        cursor = end;
    }
    out.push_str(&html[cursor..]);
    out
}

/// Decode a named HTML entity like "&micro;" into its character. Returns
/// `Some('\u{200C}')` for zero-width entities so the caller can skip them
/// without treating them as "unknown". Returns `None` for unrecognised
/// entities — caller can leave them verbatim or try numeric fallback.
fn decode_html_named_entity(entity: &str) -> Option<char> {
    match entity {
        // Structural / basic
        "&amp;" => Some('&'), "&lt;" => Some('<'), "&gt;" => Some('>'),
        "&quot;" => Some('"'), "&apos;" => Some('\''), "&nbsp;" => Some(' '),
        "&zwnj;" | "&zwj;" => Some('\u{200C}'),

        // Punctuation / dashes / quotes
        "&ndash;" => Some('\u{2013}'), "&mdash;" => Some('\u{2014}'),
        "&lsquo;" => Some('\u{2018}'), "&rsquo;" => Some('\u{2019}'),
        "&sbquo;" => Some('\u{201A}'), "&bdquo;" => Some('\u{201E}'),
        "&ldquo;" => Some('\u{201C}'), "&rdquo;" => Some('\u{201D}'),
        "&lsaquo;" => Some('\u{2039}'), "&rsaquo;" => Some('\u{203A}'),
        "&laquo;" => Some('\u{00AB}'), "&raquo;" => Some('\u{00BB}'),
        "&bull;" => Some('\u{2022}'), "&hellip;" => Some('\u{2026}'),
        "&prime;" => Some('\u{2032}'), "&Prime;" => Some('\u{2033}'),
        "&oline;" => Some('\u{203E}'), "&middot;" => Some('\u{00B7}'),
        "&para;" => Some('\u{00B6}'), "&sect;" => Some('\u{00A7}'),
        "&iexcl;" => Some('\u{00A1}'), "&iquest;" => Some('\u{00BF}'),

        // Currency / symbols
        "&cent;" => Some('\u{00A2}'), "&pound;" => Some('\u{00A3}'),
        "&curren;" => Some('\u{00A4}'), "&yen;" => Some('\u{00A5}'),
        "&euro;" => Some('\u{20AC}'),

        // Trademarks / copyright / misc
        "&trade;" => Some('\u{2122}'), "&copy;" => Some('\u{00A9}'),
        "&reg;" => Some('\u{00AE}'),
        "&deg;" => Some('\u{00B0}'), "&micro;" => Some('\u{00B5}'),
        "&not;" => Some('\u{00AC}'), "&shy;" => Some('\u{00AD}'),
        "&macr;" => Some('\u{00AF}'), "&acute;" => Some('\u{00B4}'),
        "&cedil;" => Some('\u{00B8}'), "&brvbar;" => Some('\u{00A6}'),
        "&uml;" => Some('\u{00A8}'), "&ordf;" => Some('\u{00AA}'),
        "&ordm;" => Some('\u{00BA}'),

        // Superscripts / fractions
        "&sup1;" => Some('\u{00B9}'), "&sup2;" => Some('\u{00B2}'),
        "&sup3;" => Some('\u{00B3}'),
        "&frac14;" => Some('\u{00BC}'), "&frac12;" => Some('\u{00BD}'),
        "&frac34;" => Some('\u{00BE}'),

        // Math operators
        "&times;" => Some('\u{00D7}'), "&divide;" => Some('\u{00F7}'),
        "&plusmn;" => Some('\u{00B1}'), "&minus;" => Some('\u{2212}'),
        "&ne;" => Some('\u{2260}'), "&le;" => Some('\u{2264}'), "&ge;" => Some('\u{2265}'),
        "&infin;" => Some('\u{221E}'), "&sum;" => Some('\u{2211}'), "&prod;" => Some('\u{220F}'),
        "&radic;" => Some('\u{221A}'), "&part;" => Some('\u{2202}'),
        "&int;" => Some('\u{222B}'), "&asymp;" => Some('\u{2248}'),
        "&equiv;" => Some('\u{2261}'), "&empty;" => Some('\u{2205}'),
        "&isin;" => Some('\u{2208}'), "&notin;" => Some('\u{2209}'),
        "&sub;" => Some('\u{2282}'), "&sup;" => Some('\u{2283}'),
        "&cap;" => Some('\u{2229}'), "&cup;" => Some('\u{222A}'),
        "&and;" => Some('\u{2227}'), "&or;" => Some('\u{2228}'),
        "&forall;" => Some('\u{2200}'), "&exist;" => Some('\u{2203}'),
        "&nabla;" => Some('\u{2207}'), "&prop;" => Some('\u{221D}'),
        "&lang;" => Some('\u{2329}'), "&rang;" => Some('\u{232A}'),

        // Arrows
        "&larr;" => Some('\u{2190}'), "&uarr;" => Some('\u{2191}'),
        "&rarr;" => Some('\u{2192}'), "&darr;" => Some('\u{2193}'),
        "&harr;" => Some('\u{2194}'),
        "&lArr;" => Some('\u{21D0}'), "&uArr;" => Some('\u{21D1}'),
        "&rArr;" => Some('\u{21D2}'), "&dArr;" => Some('\u{21D3}'),
        "&hArr;" => Some('\u{21D4}'),

        // Latin-1 supplement accented letters (upper)
        "&Agrave;" => Some('À'), "&Aacute;" => Some('Á'), "&Acirc;" => Some('Â'),
        "&Atilde;" => Some('Ã'), "&Auml;" => Some('Ä'),   "&Aring;" => Some('Å'),
        "&AElig;" => Some('Æ'),  "&Ccedil;" => Some('Ç'),
        "&Egrave;" => Some('È'), "&Eacute;" => Some('É'), "&Ecirc;" => Some('Ê'),
        "&Euml;" => Some('Ë'),
        "&Igrave;" => Some('Ì'), "&Iacute;" => Some('Í'), "&Icirc;" => Some('Î'),
        "&Iuml;" => Some('Ï'),
        "&ETH;" => Some('Ð'),    "&Ntilde;" => Some('Ñ'),
        "&Ograve;" => Some('Ò'), "&Oacute;" => Some('Ó'), "&Ocirc;" => Some('Ô'),
        "&Otilde;" => Some('Õ'), "&Ouml;" => Some('Ö'),   "&Oslash;" => Some('Ø'),
        "&Ugrave;" => Some('Ù'), "&Uacute;" => Some('Ú'), "&Ucirc;" => Some('Û'),
        "&Uuml;" => Some('Ü'),
        "&Yacute;" => Some('Ý'), "&THORN;" => Some('Þ'), "&szlig;" => Some('ß'),

        // Latin-1 supplement (lower)
        "&agrave;" => Some('à'), "&aacute;" => Some('á'), "&acirc;" => Some('â'),
        "&atilde;" => Some('ã'), "&auml;" => Some('ä'),   "&aring;" => Some('å'),
        "&aelig;" => Some('æ'),  "&ccedil;" => Some('ç'),
        "&egrave;" => Some('è'), "&eacute;" => Some('é'), "&ecirc;" => Some('ê'),
        "&euml;" => Some('ë'),
        "&igrave;" => Some('ì'), "&iacute;" => Some('í'), "&icirc;" => Some('î'),
        "&iuml;" => Some('ï'),
        "&eth;" => Some('ð'),    "&ntilde;" => Some('ñ'),
        "&ograve;" => Some('ò'), "&oacute;" => Some('ó'), "&ocirc;" => Some('ô'),
        "&otilde;" => Some('õ'), "&ouml;" => Some('ö'),   "&oslash;" => Some('ø'),
        "&ugrave;" => Some('ù'), "&uacute;" => Some('ú'), "&ucirc;" => Some('û'),
        "&uuml;" => Some('ü'),
        "&yacute;" => Some('ý'), "&thorn;" => Some('þ'),  "&yuml;" => Some('ÿ'),

        _ => None,
    }
}

/// Characters that browsers render as zero-width or purely-formatting.
/// Newsletters abuse these to pad email preview text; in a plain-text
/// pane they leak as "?" / "-" replacement glyphs and junk up the view.
fn is_invisible_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}'          // SOFT HYPHEN
        | '\u{034F}'        // COMBINING GRAPHEME JOINER
        | '\u{180E}'        // MONGOLIAN VOWEL SEPARATOR
        | '\u{200B}'        // ZERO WIDTH SPACE
        | '\u{200C}'        // ZERO WIDTH NON-JOINER
        | '\u{200D}'        // ZERO WIDTH JOINER
        | '\u{2060}'        // WORD JOINER
        | '\u{FEFF}'        // ZERO WIDTH NO-BREAK SPACE / BOM
    )
}


fn table_block_to_markdown(block: &str) -> String {
    let rows = extract_tr_cells(block);
    if rows.is_empty() { return String::new(); }
    let n_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if n_cols == 0 { return String::new(); }
    let mut out = String::new();
    // Header row (first <tr>, even if it only has <td>s).
    let header = &rows[0];
    out.push('|');
    for c in 0..n_cols {
        out.push(' ');
        out.push_str(header.get(c).map(|s| s.as_str()).unwrap_or(""));
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    for _ in 0..n_cols { out.push_str(" --- |"); }
    for row in &rows[1..] {
        out.push('\n');
        out.push('|');
        for c in 0..n_cols {
            out.push(' ');
            out.push_str(row.get(c).map(|s| s.as_str()).unwrap_or(""));
            out.push_str(" |");
        }
    }
    out
}

/// Walk a `<table>…</table>` block, returning each `<tr>` as a Vec of cell
/// text (both `<td>` and `<th>`). Inner HTML inside each cell is stripped
/// to plain text; pipe characters are escaped so they don't break the
/// Markdown we emit.
fn extract_tr_cells(block: &str) -> Vec<Vec<String>> {
    // ASCII folding: offsets from this copy index `block`. See html_to_text.
    let lower = block.to_ascii_lowercase();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find("<tr") {
        let tr_open = cursor + rel;
        let tr_body = match lower[tr_open..].find('>') {
            Some(p) => tr_open + p + 1,
            None => break,
        };
        let tr_end_rel = lower[tr_body..].find("</tr>");
        let tr_end = match tr_end_rel {
            Some(p) => tr_body + p,
            None => lower.len(),
        };
        let tr_slice = &block[tr_body..tr_end];
        let cells = extract_cells_in_tr(tr_slice);
        if !cells.is_empty() { rows.push(cells); }
        // A row with no `</tr>` ends at the end of the block, and
        // stepping over a terminator that is not there walked off the
        // string.
        cursor = (tr_end + 5).min(lower.len());
    }
    rows
}

fn extract_cells_in_tr(tr: &str) -> Vec<String> {
    // ASCII folding: offsets from this copy index `tr`. See html_to_text.
    let lower = tr.to_ascii_lowercase();
    let mut cells: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    loop {
        let next_td = lower[cursor..].find("<td").map(|p| cursor + p);
        let next_th = lower[cursor..].find("<th").map(|p| cursor + p);
        let cell_open = match (next_td, next_th) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            _ => None,
        };
        let Some(open) = cell_open else { break; };
        let Some(tag_end) = lower[open..].find('>').map(|p| open + p + 1) else { break; };
        // Look for matching </td> / </th>.
        let close_td = lower[tag_end..].find("</td>").map(|p| tag_end + p);
        let close_th = lower[tag_end..].find("</th>").map(|p| tag_end + p);
        let (close, close_len) = match (close_td, close_th) {
            (Some(a), Some(b)) if a < b => (a, 5),
            (_, Some(b)) => (b, 5),
            (Some(a), _) => (a, 5),
            _ => break,
        };
        let inner = &tr[tag_end..close];
        cells.push(cell_html_to_text(inner));
        cursor = close + close_len;
    }
    cells
}

/// Strip all tags from a cell's inner HTML, decode entities via the main
/// html_to_text, collapse whitespace to a single space, and escape `|` and
/// newlines so the resulting string plays nicely in a Markdown table.
fn cell_html_to_text(inner: &str) -> String {
    // Turn `<br>` into spaces so multi-line cells fit on one Markdown row.
    let pre = inner
        .replace("<br>", " ").replace("<BR>", " ")
        .replace("<br/>", " ").replace("<br />", " ").replace("<BR/>", " ").replace("<BR />", " ");
    let stripped = html_to_text(&pre);
    // Collapse consecutive whitespace, trim.
    let mut out = String::with_capacity(stripped.len());
    let mut prev_ws = false;
    for ch in stripped.chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' {
            if !prev_ws { out.push(' '); prev_ws = true; }
        } else if ch == ' ' {
            if !prev_ws { out.push(' '); prev_ws = true; }
        } else if ch == '|' {
            out.push('\\'); out.push('|'); prev_ws = false;
        } else {
            out.push(ch); prev_ws = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `to_lowercase` is not length-preserving — `İ` becomes two chars —
    /// and every offset found in the lowercased copy is used to index the
    /// original. A table holding one panicked the whole renderer.
    #[test]
    fn a_table_with_a_turkish_dotted_i_does_not_panic() {
        let html = "<table><tr><td>\u{130}stanbul</td><td>x</td></tr></table>";
        assert!(html_to_text(html).contains("stanbul"));
    }

    /// A row that never closes ends at the end of the block, and stepping
    /// over a `</tr>` that is not there walked off the string.
    #[test]
    fn an_unclosed_row_does_not_panic() {
        assert!(html_to_text("<table><tr><td>one</td><td>two</td></table>").contains("one"));
    }
}
