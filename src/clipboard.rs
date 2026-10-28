//! System clipboard via arboard, falling back to the OSC 52 terminal escape
//! (which also works over SSH and in tmux with `set-clipboard on`).

use std::io::Write;

#[derive(Default)]
pub struct Clipboard {
    // Kept alive: on X11 the owning process must live for the selection to persist.
    inner: Option<arboard::Clipboard>,
}

impl Clipboard {
    pub fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        let remote =
            std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
        if !remote {
            if self.inner.is_none() {
                self.inner = arboard::Clipboard::new().ok();
            }
            if let Some(cb) = self.inner.as_mut()
                && cb.set_text(text).is_ok()
            {
                return Ok(());
            }
        }
        osc52(text)
    }
}

fn osc52(text: &str) -> anyhow::Result<()> {
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()?;
    Ok(())
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_encodes() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"src/a.rs:42"), "c3JjL2EucnM6NDI=");
    }
}
