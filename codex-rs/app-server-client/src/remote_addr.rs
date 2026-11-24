//! Parse CLI remote app-server endpoints and determine whether they can carry bearer tokens.

use crate::RemoteAppServerEndpoint;
use crate::RemoteAuthToken;
use crate::remote::websocket_url_supports_auth_token;
use codex_core::config::find_codex_home;
use codex_utils_absolute_path::AbsolutePathBuf;
use color_eyre::eyre::WrapErr;
use url::Url;

fn remote_addr_has_explicit_port(addr: &str, parsed: &Url) -> bool {
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if parsed.port().is_some() {
        return true;
    }

    let Some((_, rest)) = addr.split_once("://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_and_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_and_port)| host_and_port);
    let explicit_default_port = match parsed.scheme() {
        "ws" => 80,
        "wss" => 443,
        _ => return false,
    };
    let expected_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    host_and_port == format!("{expected_host}:{explicit_default_port}")
}

pub fn resolve_remote_addr(addr: &str) -> color_eyre::Result<RemoteAppServerEndpoint> {
    if let Some(socket_path) = addr.strip_prefix("unix://") {
        let socket_path = if socket_path.is_empty() {
            let codex_home = find_codex_home().wrap_err("failed to resolve CODEX_HOME")?;
            crate::app_server_control_socket_path(&codex_home).map_err(color_eyre::Report::new)?
        } else {
            AbsolutePathBuf::relative_to_current_dir(socket_path)
                .map_err(color_eyre::Report::new)?
        };
        return Ok(RemoteAppServerEndpoint::UnixSocket { socket_path });
    }

    let parsed = match Url::parse(addr) {
        Ok(parsed) => parsed,
        Err(_) => {
            color_eyre::eyre::bail!(
                "invalid remote address `{addr}`; expected `ws://host:port`, `wss://host:port`, `unix://`, or `unix://PATH`"
            );
        }
    };
    if matches!(parsed.scheme(), "ws" | "wss")
        && parsed.host_str().is_some()
        && remote_addr_has_explicit_port(addr, &parsed)
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
    {
        return Ok(RemoteAppServerEndpoint::WebSocket {
            websocket_url: parsed.to_string(),
            auth_token: None,
        });
    }

    color_eyre::eyre::bail!(
        "invalid remote address `{addr}`; expected `ws://host:port`, `wss://host:port`, `unix://`, or `unix://PATH`"
    );
}

pub fn resolve_remote_endpoint(
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
) -> std::io::Result<Option<RemoteAppServerEndpoint>> {
    let mut remote_endpoint = remote
        .as_deref()
        .map(resolve_remote_addr)
        .transpose()
        .map_err(std::io::Error::other)?;
    if let Some(remote_auth_token_env) = remote_auth_token_env {
        let Some(endpoint) = remote_endpoint.as_mut() else {
            return Err(std::io::Error::other(
                "`--remote-auth-token-env` requires `--remote`.",
            ));
        };
        let slot = match endpoint {
            RemoteAppServerEndpoint::WebSocket {
                websocket_url,
                auth_token,
            } if Url::parse(websocket_url)
                .is_ok_and(|parsed| websocket_url_supports_auth_token(&parsed)) =>
            {
                auth_token
            }
            RemoteAppServerEndpoint::WebSocket { .. }
            | RemoteAppServerEndpoint::UnixSocket { .. } => {
                return Err(std::io::Error::other(
                    "`--remote-auth-token-env` requires a `wss://` or loopback `ws://` remote.",
                ));
            }
        };
        *slot = Some(RemoteAuthToken::from_env(remote_auth_token_env)?);
    }
    Ok(remote_endpoint)
}

#[cfg(test)]
#[path = "remote_addr_tests.rs"]
mod tests;
