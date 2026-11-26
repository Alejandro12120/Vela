//! Framing acotado y seguro frente a la cancelación de una lectura en select.

use std::io;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const MAX_FRAME: usize = 16 * 1024 * 1024;

pub(crate) async fn read<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    frame: &mut Vec<u8>,
) -> io::Result<bool> {
    loop {
        let received = reader.fill_buf().await?;
        if received.is_empty() {
            return Ok(!frame.is_empty());
        }
        let newline = received.iter().position(|&byte| byte == b'\n');
        let length = newline.unwrap_or(received.len());
        if frame.len() + length > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Mensaje demasiado grande",
            ));
        }
        frame.extend_from_slice(&received[..length]);
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(true);
        }
    }
}

pub(crate) async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &impl Serialize,
) -> io::Result<()> {
    let mut frame = serde_json::to_vec(value)?;
    if frame.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Respuesta demasiado grande",
        ));
    }
    frame.push(b'\n');
    writer.write_all(&frame).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_overlong_frames_before_growing_the_buffer() {
        let message = vec![b'x'; MAX_FRAME + 1];
        let mut frame = Vec::new();
        let error = read(&mut message.as_slice(), &mut frame).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(frame.is_empty());
    }
}
