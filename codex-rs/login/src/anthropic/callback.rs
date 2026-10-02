use std::io;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use url::Url;

use crate::oauth::CallbackError;
use crate::oauth::CallbackParameters;

pub(super) async fn receive(listener: TcpListener, state: &str) -> io::Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        // A stalled or malformed local probe must not consume the authorization attempt.
        if let Ok(Ok(Some(code))) = tokio::time::timeout(
            Duration::from_secs(/*secs*/ 2),
            callback(&mut stream, state),
        )
        .await
        {
            return code;
        }
    }
}

async fn callback(stream: &mut TcpStream, state: &str) -> io::Result<Option<io::Result<String>>> {
    let mut header = Vec::new();
    let mut buffer = [0u8; 1024];
    while !header.windows(/*size*/ 4).any(|bytes| bytes == b"\r\n\r\n") {
        let count = stream.read(&mut buffer).await?;
        if count == 0 || header.len() + count > 8192 {
            return Ok(None);
        }
        header.extend_from_slice(&buffer[..count]);
    }
    let result = parse(&header, state);
    let status = if result.is_some() {
        "200 OK"
    } else {
        "400 Bad Request"
    };
    // Completion is reported by the caller only after the account has been persisted.
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
    );
    // A browser disconnect after sending a valid code does not invalidate authorization.
    let _ = stream.write_all(response.as_bytes()).await;
    Ok(result)
}

fn parse(header: &[u8], state: &str) -> Option<io::Result<String>> {
    let request = std::str::from_utf8(header).ok()?.lines().next()?;
    let mut parts = request.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if !target.starts_with("/callback?") || parts.next()? != "HTTP/1.1" || parts.next().is_some() {
        return None;
    }
    let url = Url::parse(&format!("http://localhost{target}")).ok()?;
    if url.path() != "/callback" || url.fragment().is_some() {
        return None;
    }
    for key in ["code", "state", "error", "error_description"] {
        if url.query_pairs().filter(|(name, _)| name == key).count() > 1 {
            return None;
        }
    }
    let parameters = CallbackParameters::from_url(&url);
    match parameters.validate(state) {
        Ok(code) => {
            // Older callbacks put the same state after a '#' inside the encoded code.
            let code = match code.split_once('#') {
                Some((code, embedded_state)) if embedded_state == state && !code.is_empty() => code,
                Some(_) => return None,
                None => code,
            };
            Some(Ok(code.to_owned()))
        }
        Err(CallbackError::Provider { .. }) => Some(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Anthropic authorization was declined",
        ))),
        Err(CallbackError::StateMismatch | CallbackError::MissingCode) => None,
    }
}
