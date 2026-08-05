//! `vscode-jsonrpc` stdio framing: `Content-Length: N\r\n\r\n<N bytes JSON>`.
//!
//! This matches the encoding used by `StreamMessageReader`/`StreamMessageWriter`
//! in the `@github/copilot-sdk` extension client and by LSP. We only support
//! the `Content-Length` header because that is what the SDK emits; any other
//! headers that appear in the stream are tolerated and ignored.

use std::io;

use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

#[derive(Debug, Error)]
pub enum FramingError {
    #[error("stream closed before a complete message was read")]
    Eof,
    #[error("malformed header line: {0:?}")]
    MalformedHeader(String),
    #[error("missing Content-Length header")]
    MissingContentLength,
    #[error("content length too large: {0}")]
    ContentLengthTooLarge(u64),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Hard cap on a single frame. Copilot SDK observably stays well under this;
/// the ceiling protects the host from a buggy or malicious extension.
pub const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;

/// Read one framed message from `reader`, returning the raw JSON payload.
///
/// Returns [`FramingError::Eof`] if the stream closes between frames (clean
/// shutdown). IO errors or malformed headers bubble up.
pub async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Vec<u8>, FramingError>
where
    R: AsyncRead + Unpin,
{
    let mut content_length: Option<u64> = None;
    let mut header_line = String::new();

    loop {
        header_line.clear();
        let bytes = reader.read_line(&mut header_line).await?;
        if bytes == 0 {
            return Err(FramingError::Eof);
        }

        // Normalize CRLF → LF, then trim. The SDK emits CRLF; be tolerant.
        let trimmed = header_line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Blank line terminates the header block.
            break;
        }

        if let Some(rest) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            let v = rest.trim();
            content_length = Some(
                v.parse::<u64>()
                    .map_err(|_| FramingError::MalformedHeader(trimmed.into()))?,
            );
        }
        // Any other header (e.g. Content-Type) is accepted and ignored.
    }

    let len = content_length.ok_or(FramingError::MissingContentLength)?;
    if len > MAX_FRAME_BYTES {
        return Err(FramingError::ContentLengthTooLarge(len));
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write one framed message to `writer`.
pub async fn write_message<W>(writer: &mut W, payload: &[u8]) -> Result<(), FramingError>
where
    W: AsyncWrite + Unpin,
{
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn round_trip() {
        let payload = br#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
        let mut buf = Vec::new();
        write_message(&mut buf, payload).await.unwrap();

        let mut reader = BufReader::new(&buf[..]);
        let got = read_message(&mut reader).await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn tolerates_extra_headers() {
        let framed = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
                       Content-Length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(&framed[..]);
        let got = read_message(&mut reader).await.unwrap();
        assert_eq!(got, b"{}");
    }

    #[tokio::test]
    async fn eof_between_frames() {
        let mut reader = BufReader::new(&b""[..]);
        let err = read_message(&mut reader).await.unwrap_err();
        assert!(matches!(err, FramingError::Eof));
    }

    #[tokio::test]
    async fn rejects_missing_length() {
        let framed = b"X-Foo: bar\r\n\r\n";
        let mut reader = BufReader::new(&framed[..]);
        let err = read_message(&mut reader).await.unwrap_err();
        assert!(matches!(err, FramingError::MissingContentLength));
    }
}
