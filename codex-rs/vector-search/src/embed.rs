//! Embedding pipeline — wraps `fastembed` for MiniLM-L6-v2 embeddings.
//!
//! Always attempts to create a FastEmbedder. Falls back to BM25-only mode
//! on failure for graceful degradation.

/// Content truncation limit — MiniLM context is 256 tokens (~300 words).
/// 1500 chars fills the window without exceeding it.
pub const CONTENT_LIMIT: usize = 1500;

/// Embedding dimensionality for all-MiniLM-L6-v2.
pub const DIMS: usize = 384;

/// Batch size for embedding multiple texts.
pub const BATCH_SIZE: usize = 32;

/// Trait for embedding text into vectors.
///
/// This abstraction allows the index to work with any embedding backend,
/// or with no embedder at all (BM25-only mode).
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts into vectors.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String>;

    /// Embed a single text.
    fn embed_one(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut results = self.embed_batch(&[text])?;
        if results.is_empty() {
            return Err("empty embedding result".to_string());
        }
        Ok(results.remove(0))
    }
}

/// Create the default embedder (fastembed-based).
///
/// Returns `None` if initialization fails. The caller should fall back
/// to BM25-only mode.
pub fn create_embedder(cache_dir: Option<std::path::PathBuf>) -> Option<Box<dyn Embedder>> {
    match FastEmbedder::new(cache_dir) {
        Ok(e) => {
            tracing::info!("vector_search: embedder ready (all-MiniLM-L6-v2)");
            Some(Box::new(e))
        }
        Err(err) => {
            tracing::info!(%err, "vector_search: embedder unavailable, BM25-only mode");
            None
        }
    }
}

// ── fastembed implementation ─────────────────────────────────────────

struct FastEmbedder {
    model: fastembed::TextEmbedding,
}

impl FastEmbedder {
    fn new(cache_dir: Option<std::path::PathBuf>) -> Result<Self, String> {
        use fastembed::EmbeddingModel;
        use fastembed::InitOptions;
        use fastembed::TextEmbedding;

        let mut opts =
            InitOptions::new(EmbeddingModel::AllMiniLML6V2Q).with_show_download_progress(false);

        if let Some(dir) = cache_dir {
            opts = opts.with_cache_dir(dir);
        }

        let model = TextEmbedding::try_new(opts).map_err(|e| format!("fastembed init: {e}"))?;

        Ok(Self { model })
    }
}

impl Embedder for FastEmbedder {
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();

        let mut all_results = Vec::with_capacity(texts.len());
        for batch_start in (0..refs.len()).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(refs.len());
            let batch = &refs[batch_start..batch_end];
            let embeddings = self
                .model
                .embed(batch.to_vec(), None)
                .map_err(|e| format!("fastembed embed: {e}"))?;
            all_results.extend(embeddings);
        }

        Ok(all_results)
    }
}
