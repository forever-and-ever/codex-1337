//! Legacy trusted bridge wire framing; no proxy listener is created here.
use std::io::Write;
use std::io::{self};
pub const PROXY_ATTRIBUTION_TOKEN_ENV_KEY: &str = "CODEX_NETWORK_PROXY_ATTRIBUTION";
const ATTRIBUTION_FRAME_MAGIC: &[u8; 8] = b"\0CDXPXY1";
const MAX_ATTRIBUTION_TOKEN_LEN: usize = 128;
pub fn write_attribution_frame(writer: &mut impl Write, token: &str) -> io::Result<()> {
    if token.is_empty() || token.len() > MAX_ATTRIBUTION_TOKEN_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid network proxy attribution token length",
        ));
    }
    let token_len = u16::try_from(token.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "network proxy attribution token is too long",
        )
    })?;
    writer.write_all(ATTRIBUTION_FRAME_MAGIC)?;
    writer.write_all(&token_len.to_be_bytes())?;
    writer.write_all(token.as_bytes())
}
