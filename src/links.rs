//! Links in plain text: the URL under a column of a line, for Ctrl+click.
//! Real hyperlinks (OSC 8) come from the cell itself; this finds the rest.

const SCHEMES: &[&str] = &["https://", "http://", "file://", "ftp://", "mailto:", "ssh://", "git://"];

/// The URL that covers `col` in `line` (columns = characters), if any.
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    for scheme in SCHEMES {
        let mut from = 0;
        while let Some(pos) = text[from..].find(scheme) {
            let start_byte = from + pos;
            let start = text[..start_byte].chars().count();
            // a URL runs to whitespace or a character URLs cannot contain
            let mut end = start;
            while end < chars.len() && !chars[end].is_whitespace() && !matches!(chars[end], '<' | '>' | '"' | '\'' | '`' | '|') {
                end += 1;
            }
            // trailing punctuation belongs to the sentence, not the link
            while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?') {
                end -= 1;
            }
            // a closing bracket only belongs to the URL if it opened one
            while end > start && matches!(chars[end - 1], ')' | ']') {
                let open = if chars[end - 1] == ')' { '(' } else { '[' };
                let close = chars[end - 1];
                let opens = chars[start..end].iter().filter(|c| **c == open).count();
                let closes = chars[start..end].iter().filter(|c| **c == close).count();
                if closes > opens { end -= 1 } else { break }
            }
            if col >= start && col < end && end - start > scheme.len() {
                return Some(chars[start..end].iter().collect());
            }
            from = start_byte + scheme.len();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::url_at;

    #[test]
    fn finds_the_url_under_the_column() {
        let line = "see https://example.com/a?b=1 for details";
        assert_eq!(url_at(line, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 20).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 2), None);
        assert_eq!(url_at(line, 35), None);
    }

    #[test]
    fn trims_sentence_punctuation_and_unbalanced_brackets() {
        assert_eq!(url_at("go to https://a.org/x.", 8).as_deref(), Some("https://a.org/x"));
        assert_eq!(url_at("(see https://a.org/x)", 6).as_deref(), Some("https://a.org/x"));
        assert_eq!(url_at("https://en.wikipedia.org/wiki/Rust_(language)", 3).as_deref(), Some("https://en.wikipedia.org/wiki/Rust_(language)"));
    }

    #[test]
    fn a_bare_scheme_is_not_a_link() {
        assert_eq!(url_at("https://", 2), None);
    }
}
