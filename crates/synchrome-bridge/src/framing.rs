//! Native messaging framing: 4-byte native-endian length prefix + UTF-8 JSON.
//!
//! Limits: Chromium caps host -> extension messages at 1 MB; we cap extension -> host at
//! 64 KiB because the protocol never needs more (Chromium itself would allow 64 MiB). An
//! oversized length is rejected **before** allocating.

use crate::protocol::{AppToExt, ExtToApp};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_INBOUND: usize = 64 * 1024;
pub const MAX_OUTBOUND: usize = 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    /// Clean end of stream before a new frame (the extension disconnected).
    #[error("end of stream")]
    Eof,
    #[error("frame too large: {len} bytes (max {max})")]
    TooLarge { len: usize, max: usize },
    #[error("truncated frame: expected {expected} bytes, got {got}")]
    Truncated { expected: usize, got: usize },
    #[error("timed out waiting for a frame")]
    Timeout,
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid message: {0}")]
    InvalidMessage(String),
}

/// Read one frame body (bounded, with a timeout).
pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    timeout: Duration,
) -> Result<Vec<u8>, FrameError> {
    let fut = async {
        let mut len_buf = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            let n = r
                .read(&mut len_buf[got..])
                .await
                .map_err(|e| FrameError::Io(e.to_string()))?;
            if n == 0 {
                return if got == 0 {
                    Err(FrameError::Eof)
                } else {
                    Err(FrameError::Truncated { expected: 4, got })
                };
            }
            got += n;
        }
        let len = u32::from_ne_bytes(len_buf) as usize;
        if len > MAX_INBOUND {
            return Err(FrameError::TooLarge {
                len,
                max: MAX_INBOUND,
            });
        }
        let mut body = vec![0u8; len];
        let mut got = 0;
        while got < len {
            let n = r
                .read(&mut body[got..])
                .await
                .map_err(|e| FrameError::Io(e.to_string()))?;
            if n == 0 {
                return Err(FrameError::Truncated { expected: len, got });
            }
            got += n;
        }
        Ok(body)
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| FrameError::Timeout)?
}

/// Parse a frame body into a message. Unknown types and malformed JSON are typed errors.
pub fn decode(body: &[u8]) -> Result<ExtToApp, FrameError> {
    serde_json::from_slice(body).map_err(|e| FrameError::InvalidMessage(e.to_string()))
}

/// Serialize + frame an outbound message, enforcing the 1 MB limit.
pub fn encode(msg: &AppToExt) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(msg).map_err(|e| FrameError::InvalidMessage(e.to_string()))?;
    if json.len() > MAX_OUTBOUND {
        return Err(FrameError::TooLarge {
            len: json.len(),
            max: MAX_OUTBOUND,
        });
    }
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&(json.len() as u32).to_ne_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &AppToExt,
) -> Result<(), FrameError> {
    let bytes = encode(msg)?;
    w.write_all(&bytes)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    w.flush().await.map_err(|e| FrameError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HandshakeReply, HandshakeStatus};

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_ne_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }
    const T: Duration = Duration::from_secs(2);

    #[tokio::test]
    async fn roundtrip_and_multiple_frames() {
        let mut data = frame(br#"{"type":"ping"}"#);
        data.extend(frame(br#"{"type":"hello","ext_version":"1"}"#));
        let mut r = &data[..];
        assert!(matches!(
            decode(&read_frame(&mut r, T).await.unwrap()).unwrap(),
            ExtToApp::Ping
        ));
        assert!(matches!(
            decode(&read_frame(&mut r, T).await.unwrap()).unwrap(),
            ExtToApp::Hello { .. }
        ));
        assert_eq!(read_frame(&mut r, T).await.unwrap_err(), FrameError::Eof);
    }

    #[tokio::test]
    async fn oversized_length_rejected_without_reading_body() {
        let data = ((MAX_INBOUND as u32) + 1).to_ne_bytes().to_vec(); // no body at all
        let mut r = &data[..];
        assert_eq!(
            read_frame(&mut r, T).await.unwrap_err(),
            FrameError::TooLarge {
                len: MAX_INBOUND + 1,
                max: MAX_INBOUND
            }
        );
        let huge = u32::MAX.to_ne_bytes().to_vec();
        assert!(matches!(
            read_frame(&mut &huge[..], T).await.unwrap_err(),
            FrameError::TooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn truncated_frames_are_typed() {
        let data = [1u8, 0];
        assert!(matches!(
            read_frame(&mut &data[..], T).await.unwrap_err(),
            FrameError::Truncated { expected: 4, .. }
        ));
        let mut data = 10u32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"abc");
        assert_eq!(
            read_frame(&mut &data[..], T).await.unwrap_err(),
            FrameError::Truncated {
                expected: 10,
                got: 3
            }
        );
    }

    #[test]
    fn invalid_json_and_unknown_type_are_typed() {
        assert!(matches!(
            decode(b"{not json"),
            Err(FrameError::InvalidMessage(_))
        ));
        assert!(matches!(
            decode(br#"{"type":"steal_tokens"}"#),
            Err(FrameError::InvalidMessage(_))
        ));
        assert!(matches!(
            decode(br#"{"type":"hello"}"#),
            Err(FrameError::InvalidMessage(_))
        )); // missing ext_version
    }

    #[tokio::test]
    async fn timeout_when_nothing_arrives() {
        let (mut client, _server) = tokio::io::duplex(64);
        assert_eq!(
            read_frame(&mut client, Duration::from_millis(50))
                .await
                .unwrap_err(),
            FrameError::Timeout
        );
    }

    #[test]
    fn encode_enforces_outbound_limit() {
        let ok = encode(&AppToExt::Pong).unwrap();
        assert_eq!(
            u32::from_ne_bytes(ok[..4].try_into().unwrap()) as usize,
            ok.len() - 4
        );
        let big = HandshakeReply::status(HandshakeStatus::CloudUnavailable)
            .with_detail("x".repeat(MAX_OUTBOUND));
        assert!(matches!(
            encode(&AppToExt::HelloResult(big)),
            Err(FrameError::TooLarge { .. })
        ));
    }
}
