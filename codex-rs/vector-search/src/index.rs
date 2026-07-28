//! ScopeIndex — build, search, invalidate, status.
//!
//! Manages per-component indexes for hybrid BM25 + vector search.
//! Supports in-memory caching with inflight dedup, plus optional
//! disk persistence via bincode.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Notify;

use crate::bm25::bm25_score;
use crate::chunk::Chunk;
use crate::chunk::Chunker;
use crate::cosine::cosine;
use crate::discover;
use crate::embed::CONTENT_LIMIT;
use crate::embed::Embedder;
use crate::embed::{self};
use crate::tokenize::tokenize;

/// Maximum total content bytes to index per scope (50 MB).
const MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024;

// ── Types ────────────────────────────────────────────────────────────

/// Search mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    #[default]
    Hybrid,
    Vector,
    Fulltext,
}

/// Search parameters.
#[derive(Debug, Clone)]
pub struct SearchParams {
    pub query: String,
    pub scope: Option<String>,
    pub limit: usize,
    pub mode: SearchMode,
}

/// A single search result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub file: String,
    pub symbol: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub score: f64,
}

/// The index for a single component scope.
#[derive(Serialize, Deserialize)]
struct ScopeIndex {
    chunks: Vec<Chunk>,
    tokens: Vec<Vec<String>>,
    vectors: Vec<Vec<f32>>,
    df: HashMap<String, usize>,
    avg_doc_len: f64,
    built_at: SystemTime,
}

/// Index status info.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub scope: String,
    pub chunks: usize,
    pub has_vectors: bool,
}

// ── IndexManager ─────────────────────────────────────────────────────

/// Manages scope indexes — build, search, invalidate.
pub struct IndexManager {
    worktree_root: PathBuf,
    cache_dir: Option<PathBuf>,
    indexes: HashMap<String, ScopeIndex>,
    inflight: HashMap<String, Arc<Notify>>,
    embedder: Option<Box<dyn Embedder>>,
    chunker: Arc<dyn Chunker>,
}

impl IndexManager {
    /// Create a new IndexManager.
    ///
    /// - `worktree_root`: path to the worktree
    /// - `cache_dir`: optional disk cache directory for persistent indexes
    /// - `model_cache_dir`: optional directory for embedding model cache
    /// - `chunker`: the chunker to use for extracting chunks from files
    pub fn new(
        worktree_root: PathBuf,
        cache_dir: Option<PathBuf>,
        model_cache_dir: Option<PathBuf>,
        chunker: Box<dyn Chunker>,
    ) -> Self {
        let embedder = embed::create_embedder(model_cache_dir);
        Self {
            worktree_root,
            cache_dir,
            indexes: HashMap::new(),
            inflight: HashMap::new(),
            embedder,
            chunker: Arc::from(chunker),
        }
    }

    /// Create an IndexManager without embedding support (BM25-only).
    pub fn new_bm25_only(
        worktree_root: PathBuf,
        cache_dir: Option<PathBuf>,
        chunker: Box<dyn Chunker>,
    ) -> Self {
        Self {
            worktree_root,
            cache_dir,
            indexes: HashMap::new(),
            inflight: HashMap::new(),
            embedder: None,
            chunker: Arc::from(chunker),
        }
    }

    /// Search a scope with the given parameters.
    pub async fn search(&mut self, params: SearchParams) -> Result<Vec<SearchResult>, String> {
        let scope = params
            .scope
            .clone()
            .ok_or_else(|| "scope is required".to_string())?;

        self.ensure(&scope).await?;

        let idx = self
            .indexes
            .get(&scope)
            .ok_or_else(|| "index not found after build".to_string())?;

        if idx.chunks.is_empty() {
            return Ok(Vec::new());
        }

        let has_vec = idx.vectors.len() == idx.chunks.len() && !idx.vectors.is_empty();
        let mode = if !has_vec && params.mode != SearchMode::Fulltext {
            // Fall back to fulltext if no vectors available
            if params.mode == SearchMode::Vector {
                return Ok(Vec::new()); // Can't do vector-only without vectors
            }
            SearchMode::Fulltext
        } else {
            params.mode
        };

        let query_tokens = tokenize(&params.query);
        let n = idx.chunks.len();

        // BM25 scores
        let text_scores: Vec<f64> = idx
            .tokens
            .iter()
            .map(|doc| bm25_score(&query_tokens, doc, idx.avg_doc_len, n, &idx.df))
            .collect();

        // Vector scores
        let vec_scores: Vec<f64> =
            if (mode == SearchMode::Hybrid || mode == SearchMode::Vector) && has_vec {
                match &self.embedder {
                    Some(embedder) => match embedder.embed_one(&params.query) {
                        Ok(qvec) => idx
                            .vectors
                            .iter()
                            .map(|v| cosine(&qvec, v).max(0.0) as f64)
                            .collect(),
                        Err(err) => {
                            tracing::debug!(%err, "query embedding failed, text-only fallback");
                            vec![0.0; n]
                        }
                    },
                    None => vec![0.0; n],
                }
            } else {
                vec![0.0; n]
            };

        // Normalize to [0, 1]
        let t_max = text_scores.iter().copied().fold(1e-6_f64, f64::max);
        let v_max = vec_scores.iter().copied().fold(1e-6_f64, f64::max);

        let weights = (0.4, 0.6); // (text, vector)

        let mut results: Vec<SearchResult> = idx
            .chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let tn = text_scores[i] / t_max;
                let vn = vec_scores[i] / v_max;
                let score = match mode {
                    SearchMode::Fulltext => tn,
                    SearchMode::Vector => vn,
                    SearchMode::Hybrid => weights.0 * tn + weights.1 * vn,
                };
                SearchResult {
                    file: c.file.clone(),
                    symbol: c.symbol.clone(),
                    start_line: c.start_line,
                    end_line: c.end_line,
                    snippet: c.content.chars().take(500).collect(),
                    score,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(params.limit);

        Ok(results)
    }

    /// Invalidate the index for the component containing a file.
    pub fn invalidate(&mut self, file: &Path) {
        if let Some(comp) = crate::scope::find_component_root(file, &self.worktree_root) {
            if let Some(scope) = crate::scope::scope_from_component_root(&comp, &self.worktree_root)
            {
                self.indexes.remove(&scope);
                // Also remove disk cache
                if let Some(path) = self.cache_path(&scope) {
                    let _ = std::fs::remove_file(path);
                }
                tracing::debug!(%scope, "vector_search: index invalidated");
            }
        }
    }

    /// Invalidate the index for a scope by name.
    pub fn invalidate_scope(&mut self, scope: &str) {
        self.indexes.remove(scope);
        if let Some(path) = self.cache_path(scope) {
            let _ = std::fs::remove_file(path);
        }
        tracing::debug!(%scope, "vector_search: scope index invalidated");
    }

    /// Get status of all loaded indexes.
    pub fn status(&self) -> Vec<IndexStatus> {
        self.indexes
            .iter()
            .map(|(scope, idx)| IndexStatus {
                scope: scope.clone(),
                chunks: idx.chunks.len(),
                has_vectors: idx.vectors.len() == idx.chunks.len() && !idx.vectors.is_empty(),
            })
            .collect()
    }

    // ── Internal ─────────────────────────────────────────────────────

    /// Check whether the index is stale by sampling file mtimes.
    ///
    /// Collects up to 10 unique file paths from the index's chunks and
    /// checks whether any has been modified after `built_at`. This avoids
    /// a full directory walk while catching most edits.
    async fn is_stale(worktree_root: &Path, index: &ScopeIndex) -> bool {
        use std::collections::HashSet;

        let files: Vec<&str> = index
            .chunks
            .iter()
            .map(|c| c.file.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .take(10)
            .collect();

        for file in files {
            let path = worktree_root.join(file);
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                if let Ok(modified) = meta.modified() {
                    if modified > index.built_at {
                        return true;
                    }
                }
            }
        }
        false
    }

    async fn ensure(&mut self, scope: &str) -> Result<(), String> {
        if let Some(existing) = self.indexes.get(scope) {
            if !Self::is_stale(&self.worktree_root, existing).await {
                return Ok(());
            }
            tracing::info!(%scope, "vector_search: index stale, rebuilding");
            self.indexes.remove(scope);
        }

        // Try loading from disk cache
        if let Some(cached) = self.load_cached(scope).await {
            if !Self::is_stale(&self.worktree_root, &cached).await {
                self.indexes.insert(scope.to_string(), cached);
                return Ok(());
            }
            tracing::info!(%scope, "vector_search: disk cache stale, rebuilding");
            // Remove stale disk cache
            if let Some(path) = self.cache_path(scope) {
                let _ = tokio::fs::remove_file(path).await;
            }
        }

        // Check for inflight build
        if let Some(notify) = self.inflight.get(scope) {
            let notify = notify.clone();
            // Wait for inflight build to complete.
            // Drop &mut self borrow so other ops can proceed.
            notify.notified().await;
            return Ok(());
        }

        // Build fresh
        let notify = Arc::new(Notify::new());
        self.inflight.insert(scope.to_string(), notify.clone());

        let idx = self.build(scope).await?;
        self.save_cache(scope, &idx).await;
        self.indexes.insert(scope.to_string(), idx);

        self.inflight.remove(scope);
        notify.notify_waiters();

        Ok(())
    }

    async fn build(&self, scope: &str) -> Result<ScopeIndex, String> {
        let root = self.worktree_root.clone();
        let scope_owned = scope.to_string();
        let files = discover::discover_async(root.clone(), scope_owned).await;

        tracing::info!(scope, file_count = files.len(), "vector_search: indexing");

        // Read and chunk files with parallel I/O
        let mut chunks = read_and_chunk_parallel(&root, &files, &self.chunker).await;

        // Enforce total content size cap
        let total_bytes: usize = chunks.iter().map(|c| c.content.len()).sum();
        if total_bytes > MAX_TOTAL_BYTES {
            tracing::warn!(
                scope,
                total_mb = total_bytes / (1024 * 1024),
                max_mb = MAX_TOTAL_BYTES / (1024 * 1024),
                "vector_search: total content exceeds cap, truncating index"
            );
            let mut running = 0usize;
            let cutoff = chunks
                .iter()
                .position(|c| {
                    running += c.content.len();
                    running > MAX_TOTAL_BYTES
                })
                .unwrap_or(chunks.len());
            chunks.truncate(cutoff);
        }

        // Tokenize
        let tokens: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(&c.content)).collect();

        // Document frequency
        let mut df: HashMap<String, usize> = HashMap::new();
        for doc in &tokens {
            let mut seen = std::collections::HashSet::new();
            for t in doc {
                if seen.insert(t.as_str()) {
                    *df.entry(t.clone()).or_insert(0) += 1;
                }
            }
        }

        let avg_doc_len = if tokens.is_empty() {
            1.0
        } else {
            tokens.iter().map(|d| d.len()).sum::<usize>() as f64 / tokens.len() as f64
        };

        // Embeddings
        let vectors = if let Some(embedder) = &self.embedder {
            if !chunks.is_empty() {
                let texts: Vec<&str> = chunks
                    .iter()
                    .map(|c| {
                        let end = c.content.len().min(CONTENT_LIMIT);
                        &c.content[..end]
                    })
                    .collect();
                match embedder.embed_batch(&texts) {
                    Ok(vecs) => vecs,
                    Err(err) => {
                        tracing::info!(%err, "vector_search: embedding failed, BM25-only");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        tracing::info!(
            scope,
            chunks = chunks.len(),
            vectors = vectors.len(),
            "vector_search: index ready"
        );

        Ok(ScopeIndex {
            chunks,
            tokens,
            vectors,
            df,
            avg_doc_len,
            built_at: SystemTime::now(),
        })
    }

    fn cache_path(&self, scope: &str) -> Option<PathBuf> {
        self.cache_dir.as_ref().map(|dir| {
            let hash = sha1_hex(scope);
            dir.join(format!("{hash}.bin"))
        })
    }

    async fn load_cached(&self, scope: &str) -> Option<ScopeIndex> {
        let path = self.cache_path(scope)?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        bincode::deserialize(&bytes).ok()
    }

    async fn save_cache(&self, scope: &str, index: &ScopeIndex) {
        let Some(path) = self.cache_path(scope) else {
            return;
        };
        let Ok(bytes) = bincode::serialize(index) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&path, bytes).await;
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn sha1_hex(input: &str) -> String {
    use sha1::Digest;
    use sha1::Sha1;
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Read source files in parallel and extract chunks.
async fn read_and_chunk_parallel(
    root: &Path,
    files: &[String],
    chunker: &Arc<dyn Chunker>,
) -> Vec<Chunk> {
    let sem = Arc::new(tokio::sync::Semaphore::new(16));
    let mut tasks = Vec::with_capacity(files.len());

    for file in files {
        let permit = sem.clone();
        let path = root.join(file);
        let file_rel = file.clone();
        let chunker = Arc::clone(chunker);
        tasks.push(tokio::spawn(async move {
            let _permit = permit.acquire().await;
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                if meta.len() > crate::discover::MAX_FILE_SIZE {
                    tracing::debug!(file = %file_rel, size = meta.len(), "skipping large file");
                    return Vec::new();
                }
            }
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => chunker.chunk(&content, &file_rel),
                Err(_) => Vec::new(),
            }
        }));
    }

    let mut all_chunks = Vec::new();
    for task in tasks {
        if let Ok(chunks) = task.await {
            all_chunks.extend(chunks);
        }
    }
    all_chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp_chunker::CppFunctionChunker;
    use tempfile::TempDir;

    fn make_test_source() -> &'static str {
        r#"
int add(int a, int b) {
    return a + b;
}

int subtract(int a, int b) {
    return a - b;
}

int multiply(int a, int b) {
    int result = a * b;
    return result;
}
"#
    }

    #[tokio::test]
    async fn build_and_search_bm25() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let comp = root.join("mycomp");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        std::fs::write(comp.join("src/math.cc"), make_test_source()).unwrap();

        let mut mgr = IndexManager::new_bm25_only(
            root.to_path_buf(),
            None,
            Box::new(CppFunctionChunker::new()),
        );
        let results = mgr
            .search(SearchParams {
                query: "add two numbers".to_string(),
                scope: Some("mycomp".to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();

        assert!(!results.is_empty(), "should find results");
        // The 'add' function should score well for "add"
        assert!(
            results.iter().any(|r| r.symbol == "add"),
            "should find add function: {results:?}"
        );
    }

    #[tokio::test]
    async fn invalidate_clears_index() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let comp = root.join("mycomp");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        std::fs::write(comp.join("src/test.cc"), make_test_source()).unwrap();

        let mut mgr = IndexManager::new_bm25_only(
            root.to_path_buf(),
            None,
            Box::new(CppFunctionChunker::new()),
        );

        // Build index
        mgr.search(SearchParams {
            query: "add".to_string(),
            scope: Some("mycomp".to_string()),
            limit: 5,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();

        assert_eq!(mgr.status().len(), 1);

        // Invalidate
        mgr.invalidate(&comp.join("src/test.cc"));
        assert_eq!(mgr.status().len(), 0);
    }

    #[tokio::test]
    async fn status_reports_correctly() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let comp = root.join("mycomp");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        std::fs::write(comp.join("src/test.cc"), make_test_source()).unwrap();

        let mut mgr = IndexManager::new_bm25_only(
            root.to_path_buf(),
            None,
            Box::new(CppFunctionChunker::new()),
        );

        mgr.search(SearchParams {
            query: "add".to_string(),
            scope: Some("mycomp".to_string()),
            limit: 5,
            mode: SearchMode::Fulltext,
        })
        .await
        .unwrap();

        let statuses = mgr.status();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].scope, "mycomp");
        assert!(!statuses[0].has_vectors); // BM25-only mode
        assert!(statuses[0].chunks > 0);
    }

    #[tokio::test]
    async fn stale_inmemory_index_is_rebuilt() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let comp = root.join("mycomp");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        std::fs::write(comp.join("src/math.cc"), make_test_source()).unwrap();

        let mut mgr = IndexManager::new_bm25_only(
            root.to_path_buf(),
            None,
            Box::new(CppFunctionChunker::new()),
        );

        // Build index — should find "add"
        let results = mgr
            .search(SearchParams {
                query: "add".to_string(),
                scope: Some("mycomp".to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();
        assert!(results.iter().any(|r| r.symbol == "add"));
        assert_eq!(mgr.status().len(), 1);

        // Modify the file after indexing — replace content
        // Small sleep to ensure mtime is strictly after built_at
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            comp.join("src/math.cc"),
            r#"
int divide(int a, int b) {
    return a / b;
}
"#,
        )
        .unwrap();

        // Next search should detect staleness, rebuild, and find "divide" instead
        let results = mgr
            .search(SearchParams {
                query: "divide".to_string(),
                scope: Some("mycomp".to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.symbol == "divide"),
            "should find 'divide' after rebuild: {results:?}"
        );
        // "add" should no longer be in the index
        assert!(
            !results.iter().any(|r| r.symbol == "add"),
            "should not find 'add' after rebuild: {results:?}"
        );
    }

    #[tokio::test]
    async fn stale_disk_cache_is_rebuilt() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let comp = root.join("mycomp");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(comp.join("Component.py"), "").unwrap();
        std::fs::write(comp.join("src/math.cc"), make_test_source()).unwrap();

        // Build index with disk cache
        {
            let mut mgr = IndexManager::new_bm25_only(
                root.to_path_buf(),
                Some(cache_dir.clone()),
                Box::new(CppFunctionChunker::new()),
            );
            mgr.search(SearchParams {
                query: "add".to_string(),
                scope: Some("mycomp".to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();
            assert_eq!(mgr.status().len(), 1);
        }

        // Modify file after the disk cache was written
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            comp.join("src/math.cc"),
            r#"
int modulo(int a, int b) {
    return a % b;
}
"#,
        )
        .unwrap();

        // New manager — no in-memory state, must load from disk cache
        let mut mgr2 = IndexManager::new_bm25_only(
            root.to_path_buf(),
            Some(cache_dir),
            Box::new(CppFunctionChunker::new()),
        );
        let results = mgr2
            .search(SearchParams {
                query: "modulo".to_string(),
                scope: Some("mycomp".to_string()),
                limit: 5,
                mode: SearchMode::Fulltext,
            })
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.symbol == "modulo"),
            "should find 'modulo' after stale disk cache rebuild: {results:?}"
        );
    }
}
