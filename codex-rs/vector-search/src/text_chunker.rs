//! Text chunker — wraps `text_splitter::TextSplitter` for prose content.
//!
//! Splits at semantic boundaries (sentence, paragraph, newline sequences)
//! and adds file/line metadata to each chunk.

use crate::chunk::Chunk;
use crate::chunk::ChunkKind;
use crate::chunk::Chunker;
use text_splitter::TextSplitter;

/// Default max characters per chunk (matches embedding context window).
const DEFAULT_MAX_CHARS: usize = 1500;

/// Prose chunker using `text-splitter` for semantic boundary detection.
pub struct TextSplitterChunker {
    max_chars: usize,
}

impl TextSplitterChunker {
    pub fn new() -> Self {
        Self {
            max_chars: DEFAULT_MAX_CHARS,
        }
    }

    pub fn with_max_chars(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Default for TextSplitterChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker for TextSplitterChunker {
    fn chunk(&self, content: &str, file_path: &str) -> Vec<Chunk> {
        if content.is_empty() {
            return Vec::new();
        }

        let splitter = TextSplitter::new(self.max_chars);
        splitter
            .chunks(content)
            .enumerate()
            .map(|(i, chunk_text)| {
                // Map byte offset back to line numbers.
                // text-splitter returns &str slices into the original content.
                let byte_start = chunk_text.as_ptr() as usize - content.as_ptr() as usize;
                let start_line = content[..byte_start].matches('\n').count() + 1;
                let end_line = start_line + chunk_text.matches('\n').count();

                Chunk {
                    file: file_path.to_string(),
                    symbol: format!("chunk_{}", i + 1),
                    start_line,
                    end_line,
                    content: chunk_text.to_string(),
                    kind: ChunkKind::Generic,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_empty_content() {
        let chunker = TextSplitterChunker::new();
        let chunks = chunker.chunk("", "test.txt");
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunks_short_content() {
        let chunker = TextSplitterChunker::new();
        let chunks = chunker.chunk("Hello world.", "test.txt");
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].file, "test.txt");
        assert_eq!(chunks[0].kind, ChunkKind::Generic);
    }

    #[test]
    fn chunks_long_content() {
        let chunker = TextSplitterChunker::with_max_chars(50);
        let content = "First sentence here. Second sentence here.

Another paragraph with more text that goes on and on to fill up space.

Third paragraph.";
        let chunks = chunker.chunk(content, "test.txt");
        assert!(
            chunks.len() > 1,
            "should split into multiple chunks: got {}",
            chunks.len()
        );
        for chunk in &chunks {
            assert!(
                chunk.content.len() <= 100,
                "chunk too large: {}",
                chunk.content.len()
            );
        }
    }

    #[test]
    fn line_numbers_are_correct() {
        let chunker = TextSplitterChunker::with_max_chars(20);
        let content = "line one
line two
line three";
        let chunks = chunker.chunk(content, "test.txt");
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].start_line, 1);
    }
}
