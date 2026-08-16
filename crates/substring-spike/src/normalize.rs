use unicode_normalization::UnicodeNormalization;

/// Produces the canonical experimental search form.
///
/// The function is shared by indexing, planning, verification, and ranking so
/// the benchmark cannot measure a different normalization path than tests.
#[must_use]
pub fn normalize_search_text(value: &str) -> String {
    let folded: String = value.nfkc().flat_map(char::to_lowercase).collect();
    let mut normalized = String::with_capacity(folded.len());
    let mut previous_separator = false;

    for character in folded.chars() {
        let separator = character == '\\' || character == '/';
        if separator {
            if !previous_separator {
                normalized.push('/');
            }
        } else {
            normalized.push(character);
        }
        previous_separator = separator;
    }

    normalized
}

/// Tokenizes normalized filename text at non-alphanumeric boundaries.
#[must_use]
pub fn tokenize(normalized: &str) -> Vec<&str> {
    normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::normalize_search_text;

    #[test]
    fn normalization_is_unicode_aware_and_separator_stable() {
        assert_eq!(normalize_search_text("CAFÉ\\\\ПУТЬ/File"), "café/путь/file");
        assert_eq!(normalize_search_text("Cafe\u{301}"), "café");
    }
}
