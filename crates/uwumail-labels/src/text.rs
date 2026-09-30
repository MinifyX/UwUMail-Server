//! Small helpers for comparing text: folding, words and word boundaries.

/// Lower case (Unicode), every run of white space one space, trimmed.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A word boundary: the start or end of the text, or a character that is neither a letter nor a
/// digit.
pub fn boundary(c: Option<char>) -> bool {
    c.is_none_or(|c| !c.is_alphanumeric())
}

/// Whether `needle` occurs in `hay` starting and ending at word boundaries. Both are folded.
pub fn has_word(hay: &str, needle: &str) -> bool {
    find_word(hay, needle).is_some()
}

/// Where `needle` first occurs in `hay` as a whole word (a byte offset).
pub fn find_word(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(found) = hay[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        if boundary(hay[..start].chars().next_back()) && boundary(hay[end..].chars().next()) {
            return Some(start);
        }
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The runs of letters and digits of `text`, with their byte offsets.
pub fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = text.char_indices().peekable();
    std::iter::from_fn(move || {
        while let Some(&(_, c)) = rest.peek() {
            if c.is_alphanumeric() {
                break;
            }
            rest.next();
        }
        let (start, _) = *rest.peek()?;
        let mut end = start;
        while let Some(&(index, c)) = rest.peek() {
            if !c.is_alphanumeric() {
                break;
            }
            end = index + c.len_utf8();
            rest.next();
        }
        Some((start, &text[start..end]))
    })
}

/// The whole word of `text` around the byte offset `at`.
pub fn word_at(text: &str, at: usize) -> &str {
    let start =
        text[..at].char_indices().rev().find(|(_, c)| !c.is_alphanumeric()).map_or(0, |(i, c)| i + c.len_utf8());
    let end = text[at..].char_indices().find(|(_, c)| !c.is_alphanumeric()).map_or(text.len(), |(i, _)| at + i);
    &text[start..end]
}

/// The first of `stems` that occurs in `folded` (a substring), and where.
pub fn find_any<'a>(folded: &str, stems: &[&'a str]) -> Option<(&'a str, usize)> {
    stems.iter().filter_map(|stem| folded.find(stem).map(|at| (*stem, at))).min_by_key(|(_, at)| *at)
}

/// The word of the original `text` that holds the first stem of `stems`, as written: folding keeps
/// the words and their order, so the n-th word of the folded text is the n-th of the original.
pub fn original_word(text: &str, stems: &[&str]) -> Option<String> {
    let folded = fold(text);
    let (_, at) = find_any(&folded, stems)?;
    let index = words(&folded).position(|(start, word)| start <= at && at < start + word.len())?;
    words(text).nth(index).map(|(_, word)| word.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_and_words() {
        assert_eq!(fold("  Ihre   RECHNUNG\n\tSeptember "), "ihre rechnung september");
        assert!(has_word("ship with gls today", "gls"));
        assert!(!has_word("goglsx", "gls"));
        assert_eq!(words("a-b, Größe!").map(|(_, w)| w).collect::<Vec<_>>(), ["a", "b", "Größe"]);
        assert_eq!(word_at("ihre mobilfunkrechnung ist da", 14), "mobilfunkrechnung");
        assert_eq!(
            original_word("Ihre Mobilfunkrechnung  September", &["rechnung"]).as_deref(),
            Some("Mobilfunkrechnung")
        );
    }
}
