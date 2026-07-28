//! Simple tokenizer for BM25 scoring.
//!
//! Splits text into lowercase alphanumeric tokens, filtering out
//! single-character tokens. Matches opencode's `tokenize()`.

/// Tokenize text for BM25 indexing.
///
/// Lowercases, splits on non-alphanumeric/underscore boundaries,
/// and filters tokens with length ≤ 1.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() > 1)
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_non_alphanumeric_boundaries() {
        let tokens = tokenize("hello_world foo-bar baz.qux");
        assert_eq!(tokens, vec!["hello_world", "foo", "bar", "baz", "qux"]);
    }

    #[test]
    fn lowercases() {
        let tokens = tokenize("Hello WORLD FooBar");
        assert_eq!(tokens, vec!["hello", "world", "foobar"]);
    }

    #[test]
    fn filters_single_char_tokens() {
        let tokens = tokenize("a bb c dd e");
        assert_eq!(tokens, vec!["bb", "dd"]);
    }

    #[test]
    fn handles_empty_input() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }
}
