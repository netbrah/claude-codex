//! Cosine similarity between two vectors.

/// Compute cosine similarity between two vectors.
///
/// Returns 0.0 if either vector has zero magnitude.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for i in 0..a.len().min(b.len()) {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na > 0.0 && nb > 0.0 {
        dot / (na.sqrt() * nb.sqrt())
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_return_1() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "identical vectors: {sim}");
    }

    #[test]
    fn orthogonal_vectors_return_0() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine(&a, &b);
        assert!(sim.abs() < 1e-5, "orthogonal vectors: {sim}");
    }

    #[test]
    fn opposite_vectors_return_neg1() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine(&a, &b);
        assert!((sim + 1.0).abs() < 1e-5, "opposite vectors: {sim}");
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
        assert!(sim > 0.99, "similar vectors should have high score: {sim}");
    }
}
