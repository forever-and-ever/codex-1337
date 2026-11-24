//! Desktop links refer to a saved thread on one specific backend, never to a title.
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_protocol::ThreadId;
use codex_utils_path::system_executable;
use sha2::Digest;
use sha2::Sha256;
use tokio::process::Command;
use tokio::sync::watch;
use url::Url;

use crate::AppServerTarget;

#[cfg(target_os = "linux")]
mod windows;

#[derive(Clone)]
pub(crate) struct DesktopRoute {
    destination: Destination,
    namespace: [u8; 32],
}

#[derive(Clone)]
enum Destination {
    Embedded(PathBuf),
    Server(RemoteAppServerEndpoint),
}

impl DesktopRoute {
    pub(crate) fn from_target(target: &AppServerTarget, embedded: bool, codex_home: &Path) -> Self {
        let destination = match target {
            AppServerTarget::Embedded => Destination::Embedded(codex_home.to_owned()),
            AppServerTarget::LocalDaemon { .. } if embedded => {
                Destination::Embedded(codex_home.to_owned())
            }
            AppServerTarget::LocalDaemon { endpoint, .. }
            | AppServerTarget::Remote { endpoint } => Destination::Server(endpoint.clone()),
        };
        let mut digest = Sha256::new();
        match &destination {
            Destination::Embedded(home) => {
                digest.update(b"embedded\0");
                digest.update(home.as_os_str().as_encoded_bytes());
            }
            Destination::Server(RemoteAppServerEndpoint::UnixSocket { socket_path }) => {
                digest.update(b"unix\0");
                digest.update(socket_path.as_path().as_os_str().as_encoded_bytes());
            }
            Destination::Server(RemoteAppServerEndpoint::WebSocket {
                websocket_url,
                auth_token,
            }) => {
                digest.update(b"websocket\0");
                digest.update(websocket_url.as_bytes());
                if let Some(token) = auth_token {
                    digest.update(b"\0authenticated\0");
                    digest.update(token.as_str().as_bytes());
                }
            }
        }
        Self {
            destination,
            namespace: digest.finalize().into(),
        }
    }

    fn thread_key(&self, thread: ThreadId) -> [u8; 32] {
        Sha256::new()
            .chain_update(self.namespace)
            .chain_update(thread.to_string())
            .finalize()
            .into()
    }

    pub(crate) async fn open_or_focus(&self, thread: ThreadId) -> Result<()> {
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("Opening saved threads in i3 requires Linux");

        #[cfg(target_os = "linux")]
        {
            let key = self.thread_key(thread);
            if tokio::task::spawn_blocking(move || windows::focus(&key)).await?? {
                return Ok(());
            }
            let terminal = system_executable("alacritty").context("Alacritty is not installed")?;
            let codex = std::env::current_exe().context("Cannot locate this Codex executable")?;
            let mut child = self
                .terminal_command(&terminal, &codex, thread)?
                .spawn()
                .context("Cannot launch Alacritty")?;
            tokio::spawn(async move {
                if let Err(error) = child.wait().await {
                    tracing::debug!(%error, "could not reap saved-thread terminal");
                }
            });
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn terminal_command(&self, terminal: &Path, codex: &Path, thread: ThreadId) -> Result<Command> {
        let mut command = Command::new(terminal);
        // This is a new TUI host, not a replacement worker for the current terminal.
        for name in [
            "CODEX_UI_HOST",
            "CODEX_UI_PARENT",
            "CODEX_UI_GENERATION",
            "ALACRITTY_WINDOW_ID",
            "ALACRITTY_SOCKET",
            "WINDOWID",
        ] {
            command.env_remove(name);
        }
        command.arg("-e").arg(codex).args(self.resume_args(thread)?);
        if let Destination::Embedded(home) = &self.destination {
            command.env("CODEX_HOME", home);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        Ok(command)
    }

    fn resume_args(&self, thread: ThreadId) -> Result<Vec<OsString>> {
        let mut args: Vec<OsString> = Vec::new();
        match &self.destination {
            Destination::Embedded(_) => args.extend(["--no-daemon".into(), "resume".into()]),
            Destination::Server(endpoint) => {
                args.extend(["resume".into(), "--remote".into()]);
                match endpoint {
                    RemoteAppServerEndpoint::UnixSocket { socket_path } => {
                        let path = socket_path
                            .as_path()
                            .to_str()
                            .context("Cannot reopen a non-UTF-8 server address")?;
                        args.push(format!("unix://{path}").into());
                    }
                    RemoteAppServerEndpoint::WebSocket {
                        websocket_url,
                        auth_token,
                    } => {
                        let url = Url::parse(websocket_url)
                            .context("Invalid saved-thread server address")?;
                        ensure!(
                            matches!(url.scheme(), "ws" | "wss")
                                && url.username().is_empty()
                                && url.password().is_none()
                                && url.query().is_none()
                                && url.fragment().is_none(),
                            "Cannot pass a credential-bearing server address to a new terminal"
                        );
                        args.push(websocket_url.into());
                        if let Some(token) = auth_token {
                            let name = token.source_env_if_current().context("Cannot reopen this authenticated server: its original credential environment variable is unavailable or has changed")?;
                            args.push(format!("--remote-auth-token-env={name}").into());
                        }
                    }
                }
            }
        }
        args.push(thread.to_string().into());
        Ok(args)
    }
}

/// Publish changes only. The X client owns the marker, so exit or reload can't leave a stale seat.
pub(crate) struct DesktopPresence {
    last: Option<([u8; 32], ThreadId)>,
    sender: Option<watch::Sender<Option<[u8; 32]>>>,
}

impl DesktopPresence {
    pub(crate) fn new() -> Self {
        #[cfg(target_os = "linux")]
        let sender = if std::env::var_os("DISPLAY").is_some()
            && std::env::var_os("ALACRITTY_WINDOW_ID").is_some()
        {
            let (tx, mut rx) = watch::channel(None);
            tokio::spawn(async move {
                let mut publisher = None;
                while rx.changed().await.is_ok() {
                    let key = *rx.borrow_and_update();
                    let update = tokio::task::spawn_blocking(move || -> Result<_> {
                        let mut owner = match (publisher, key) {
                            (Some(owner), _) => owner,
                            (None, Some(_)) => windows::Publisher::new()?,
                            (None, None) => return Ok(None),
                        };
                        owner.publish(key.as_ref())?;
                        Ok(Some(owner))
                    })
                    .await;
                    publisher = match update {
                        Ok(Ok(owner)) => owner,
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "saved-thread window registration unavailable");
                            None
                        }
                        Err(_) => break,
                    };
                }
            });
            Some(tx)
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let sender = None;
        Self { sender, last: None }
    }

    pub(crate) fn show(&mut self, route: &DesktopRoute, thread: Option<ThreadId>) {
        let Some(sender) = &self.sender else { return };
        let next = thread.map(|id| (route.namespace, id));
        if self.last != next {
            self.last = next;
            sender.send_replace(thread.map(|id| route.thread_key(id)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_client::RemoteAuthToken;
    use codex_utils_absolute_path::AbsolutePathBuf;

    #[test]
    fn terminal_resume_preserves_endpoint_and_argv_boundaries() -> Result<()> {
        let thread = ThreadId::new();
        let socket = "/run/user/1000/codex test;'dedicated'.sock";
        let home = Path::new("/example-home");
        let local = DesktopRoute::from_target(&AppServerTarget::Embedded, true, home);
        let unix = DesktopRoute::from_target(
            &AppServerTarget::Remote {
                endpoint: RemoteAppServerEndpoint::UnixSocket {
                    socket_path: AbsolutePathBuf::try_from(socket)?,
                },
            },
            false,
            home,
        );
        assert_ne!(local.thread_key(thread), unix.thread_key(thread));
        #[cfg(target_os = "linux")]
        {
            let command = unix.terminal_command(
                Path::new("/bin/alacritty"),
                Path::new("/release/codex"),
                thread,
            )?;
            let command = command.as_std();
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(
                args[..2],
                [
                    std::ffi::OsStr::new("-e"),
                    std::ffi::OsStr::new("/release/codex")
                ]
            );
            assert_eq!(args[2..], unix.resume_args(thread)?);
            for name in ["CODEX_UI_HOST", "CODEX_UI_PARENT", "CODEX_UI_GENERATION"] {
                assert!(
                    command
                        .get_envs()
                        .any(|(key, value)| key == name && value.is_none())
                );
            }
        }
        assert_eq!(
            unix.resume_args(thread)?,
            [
                OsString::from("resume"),
                "--remote".into(),
                format!("unix://{socket}").into(),
                thread.to_string().into()
            ]
        );
        assert_eq!(
            local.resume_args(thread)?,
            [
                OsString::from("--no-daemon"),
                "resume".into(),
                thread.to_string().into()
            ]
        );

        for (address, auth_token) in [
            (
                "wss://example.invalid:123/",
                Some(RemoteAuthToken::from_secret("unexported".to_owned())),
            ),
            ("wss://name:secret@example.invalid:123/", None),
        ] {
            let route = DesktopRoute::from_target(
                &AppServerTarget::Remote {
                    endpoint: RemoteAppServerEndpoint::WebSocket {
                        websocket_url: address.to_string(),
                        auth_token,
                    },
                },
                false,
                home,
            );
            assert!(route.resume_args(thread).is_err());
        }
        Ok(())
    }
}
