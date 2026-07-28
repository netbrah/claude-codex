use codex_vector_search::cosine::cosine;

#[test]
fn identical_vectors_return_1() {
    let v = vec![1.0, 2.0, 3.0];
    let sim = cosine(&v, &v);
    assert!((sim - 1.0).abs() < 1e-5, "identical: {sim}");
}

#[test]
fn orthogonal_vectors_return_0() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![0.0, 1.0, 0.0];
    let sim = cosine(&a, &b);
    assert!(sim.abs() < 1e-5, "orthogonal: {sim}");
}

#[test]
fn opposite_vectors_return_neg1() {
    let a = vec![1.0, 0.0];
    let b = vec![-1.0, 0.0];
    let sim = cosine(&a, &b);
    assert!((sim + 1.0).abs() < 1e-5, "opposite: {sim}");
}

#[test]
fn handles_zero_vectors() {
    let a = vec![0.0, 0.0, 0.0];
    let b = vec![1.0, 2.0, 3.0];
    assert_eq!(cosine(&a, &b), 0.0);
    assert_eq!(cosine(&a, &a), 0.0);
}

#[test]
fn similar_vectors_have_high_score() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![1.1, 2.1, 3.1];
    let sim = cosine(&a, &b);
    assert!(sim > 0.99, "similar: {sim}");
}
