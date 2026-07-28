//! Markdown chunker — wraps `text_splitter::MarkdownSplitter`.
//!
//! Splits at markdown semantic boundaries (headings, paragraphs, code blocks).
//! Symbol name = heading text (if present).

use crate::chunk::Chunk;
use crate::chunk::ChunkKind;
use crate::chunk::Chunker;
use text_splitter::MarkdownSplitter;

const DEFAULT_MAX_CHARS: usize = 1500;

/// Markdown-aware chunker using `text-splitter`'s markdown parser.
pub struct MarkdownChunker {
    max_chars: usize,
}

impl MarkdownChunker {
    pub fn new() -> Self {
        Self {
            max_chars: DEFAULT_MAX_CHARS,
        }
    }

    pub fn with_max_chars(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Default for MarkdownChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker for MarkdownChunker {
    fn chunk(&self, content: &str, file_path: &str) -> Vec<Chunk> {
        if content.is_empty() {
            return Vec::new();
        }

        let splitter = MarkdownSplitter::new(self.max_chars);
        splitter
            .chunks(content)
            .enumerate()
            .map(|(i, chunk_text)| {
                // Extract heading as symbol name.
                let symbol = chunk_text
                    .lines()
                    .find(|l| l.starts_with('#'))
                    .map(|l| l.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| format!("section_{}", i + 1));

                let byte_start = chunk_text.as_ptr() as usize - content.as_ptr() as usize;
                let start_line = content[..byte_start].matches('\n').count() + 1;
                let end_line = start_line + chunk_text.matches('\n').count();

                Chunk {
                    file: file_path.to_string(),
                    symbol,
                    start_line,
                    end_line,
                    content: chunk_text.to_string(),
                    kind: ChunkKind::Section,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_empty_markdown() {
        let chunker = MarkdownChunker::new();
        let chunks = chunker.chunk("", "test.md");
        assert!(chunks.is_empty());
    }

    #[test]
    fn extracts_heading_as_symbol() {
        let chunker = MarkdownChunker::new();
        let content = "# Introduction

This is the intro paragraph.

## Details

More details here.";
        let chunks = chunker.chunk(content, "test.md");
        assert!(!chunks.is_empty());
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            symbols.iter().any(|s| s.contains("Introduction")),
            "should extract heading: {symbols:?}"
        );
    }

    #[test]
    fn all_chunks_are_section_kind() {
        let chunker = MarkdownChunker::new();
        let chunks = chunker.chunk(
            "# Title

Content",
            "test.md",
        );
        for chunk in &chunks {
            assert_eq!(chunk.kind, ChunkKind::Section);
        }
    }
}
