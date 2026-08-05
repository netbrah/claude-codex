use codex_vector_search::bm25::bm25_score;
use std::collections::HashMap;

fn make_df(terms: &[(&str, usize)]) -> HashMap<String, usize> {
    terms.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn matching_terms_produce_positive_score() {
    let query = vec!["hello".to_string(), "world".to_string()];
    let doc = vec!["hello".to_string(), "world".to_string(), "foo".to_string()];
    let df = make_df(&[("hello", 1), ("world", 1), ("foo", 2)]);
    let score = bm25_score(&query, &doc, 3.0, 5, &df);
    assert!(
        score > 0.0,
        "matching terms should produce positive score: {score}"
    );
}

#[test]
fn no_matching_terms_produce_zero() {
    let query = vec!["hello".to_string()];
    let doc = vec!["world".to_string(), "foo".to_string()];
    let df = make_df(&[("hello", 1), ("world", 2), ("foo", 2)]);
    let score = bm25_score(&query, &doc, 2.0, 5, &df);
    assert!((score - 0.0).abs() < f64::EPSILON);
}

#[test]
fn rare_terms_score_higher_than_common() {
    let query_rare = vec!["rare".to_string()];
    let query_common = vec!["common".to_string()];
    let doc = vec!["rare".to_string(), "common".to_string()];
    let df = make_df(&[("rare", 1), ("common", 50)]);
    let rare_score = bm25_score(&query_rare, &doc, 2.0, 100, &df);
    let common_score = bm25_score(&query_common, &doc, 2.0, 100, &df);
    assert!(
        rare_score > common_score,
        "rare ({rare_score}) > common ({common_score})"
    );
}

#[test]
fn handles_empty_query() {
    let query: Vec<String> = vec![];
    let doc = vec!["hello".to_string()];
    let df = make_df(&[("hello", 1)]);
    let score = bm25_score(&query, &doc, 1.0, 5, &df);
    assert!((score - 0.0).abs() < f64::EPSILON);
}

#[test]
fn handles_empty_document() {
    let query = vec!["hello".to_string()];
    let doc: Vec<String> = vec![];
    let df = make_df(&[("hello", 1)]);
    let score = bm25_score(&query, &doc, 1.0, 5, &df);
    assert!((score - 0.0).abs() < f64::EPSILON);
}
