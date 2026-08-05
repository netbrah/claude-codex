//! BM25 scoring — hand-rolled port of opencode's 15-line implementation.
//!
//! We hand-roll instead of using the `bm25` crate because the merge step
//! requires raw per-document scores, and the crate's `search()` returns
//! ranked results without exposing raw scores.

use std::collections::HashMap;

/// BM25 parameters (standard defaults).
const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Compute BM25 score for a single document against a query.
///
/// # Arguments
/// - `query` — tokenized query terms
/// - `doc_tokens` — tokenized document
/// - `avg_doc_len` — average document length across the corpus
/// - `num_docs` — total number of documents
/// - `df` — document frequency map (term → number of docs containing it)
pub fn bm25_score(
    query: &[String],
    doc_tokens: &[String],
    avg_doc_len: f64,
    num_docs: usize,
    df: &HashMap<String, usize>,
) -> f64 {
    let dl = doc_tokens.len() as f64;

    // Build term frequency map for this document.
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for t in doc_tokens {
        *freq.entry(t.as_str()).or_insert(0) += 1;
    }

    let mut score = 0.0;
    let n = num_docs as f64;

    for term in query {
        let tf = *freq.get(term.as_str()).unwrap_or(&0) as f64;
        if tf == 0.0 {
            continue;
        }
        let doc_freq = *df.get(term.as_str()).unwrap_or(&1) as f64;
        let idf = ((n - doc_freq + 0.5) / (doc_freq + 0.5) + 1.0).ln();
        score += idf * ((tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + (B * dl) / avg_doc_len)));
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            (score - 0.0).abs() < f64::EPSILON,
            "no matching terms should produce zero: {score}"
        );
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
            "rare terms ({rare_score}) should score higher than common ({common_score})"
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
}
