use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Prompt type as reported by SSH_ASKPASS_PROMPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptType {
    /// Password / passphrase (SSH_ASKPASS_PROMPT="" or unset)
    Secret,
    /// Host-key confirmation (SSH_ASKPASS_PROMPT="confirm")
    Confirm,
    /// Informational (SSH_ASKPASS_PROMPT="none")
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskpassRequest {
    pub prompt_type: PromptType,
    pub prompt: String,
}

/// `None` means the user cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskpassResponse(pub Option<String>);

/// Write a length-prefixed bincode message to a sync writer.
pub fn write_msg(w: &mut impl Write, msg: &impl Serialize) -> std::io::Result<()> {
    let data = bincode::serialize(msg).map_err(std::io::Error::other)?;
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Read a length-prefixed bincode message from a sync reader.
pub fn read_msg<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    bincode::deserialize(&buf).map_err(std::io::Error::other)
}

/// Async versions for the tokio side.
pub mod tokio {
    use super::*;
    use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub async fn write_msg(
        w: &mut (impl ::tokio::io::AsyncWrite + Unpin),
        msg: &impl Serialize,
    ) -> std::io::Result<()> {
        let data = bincode::serialize(msg).map_err(std::io::Error::other)?;
        w.write_all(&(data.len() as u32).to_be_bytes()).await?;
        w.write_all(&data).await?;
        w.flush().await
    }

    pub async fn read_msg<T: for<'de> Deserialize<'de>>(
        r: &mut (impl ::tokio::io::AsyncRead + Unpin),
    ) -> std::io::Result<T> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        bincode::deserialize(&buf).map_err(std::io::Error::other)
    }
}
