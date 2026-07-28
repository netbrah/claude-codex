use codex_vector_search::tokenize::tokenize;

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
