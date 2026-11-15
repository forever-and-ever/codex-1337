mod bel;
mod command;
mod osc;

use std::io;

use bel::BelBackend;
use codex_config::types::NotificationMethod;
use codex_config::types::TuiNotificationSettings;
use codex_terminal_detection::TerminalInfo;
use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use command::CommandBackend;
use osc::OscBackend;
use osc::OscProtocol;

#[derive(Debug)]
pub enum DesktopNotificationBackend {
    Osc(OscBackend),
    Bel(BelBackend),
    Command {
        backend: CommandBackend,
        fallback: NotificationMethod,
    },
}

impl DesktopNotificationBackend {
    pub fn for_settings(settings: &TuiNotificationSettings) -> Self {
        match &settings.command {
            Some(command) => Self::Command {
                backend: CommandBackend(command.clone()),
                fallback: settings.method,
            },
            None => Self::for_method(settings.method),
        }
    }

    pub fn for_method(method: NotificationMethod) -> Self {
        match method {
            NotificationMethod::Auto => {
                if supports_osc9(&terminal_info()) {
                    Self::Osc(OscBackend::new(OscProtocol::Osc9))
                } else {
                    Self::Bel(BelBackend)
                }
            }
            NotificationMethod::Osc9 => Self::Osc(OscBackend::new(OscProtocol::Osc9)),
            NotificationMethod::Osc777 => Self::Osc(OscBackend::new(OscProtocol::Osc777)),
            NotificationMethod::Bel => Self::Bel(BelBackend),
        }
    }

    pub fn method(&self) -> &'static str {
        match self {
            DesktopNotificationBackend::Osc(backend) => match backend.protocol {
                OscProtocol::Osc9 => "osc9",
                OscProtocol::Osc777 => "osc777",
            },
            DesktopNotificationBackend::Bel(_) => "bel",
            DesktopNotificationBackend::Command { .. } => "command",
        }
    }

    pub fn notify(&mut self, message: &str) -> io::Result<()> {
        match self {
            DesktopNotificationBackend::Osc(backend) => backend.notify(message),
            DesktopNotificationBackend::Bel(backend) => backend.notify(message),
            DesktopNotificationBackend::Command { backend, fallback } => {
                match backend.notify(message) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        tracing::warn!(%error, "Notification command unavailable; using terminal notifications");
                        *self = Self::for_method(*fallback);
                        self.notify(message)
                    }
                }
            }
        }
    }
}

pub fn detect_backend(method: NotificationMethod) -> DesktopNotificationBackend {
    DesktopNotificationBackend::for_method(method)
}

fn supports_osc9(terminal: &TerminalInfo) -> bool {
    matches!(
        terminal.name,
        TerminalName::Ghostty
            | TerminalName::Iterm2
            | TerminalName::Kitty
            | TerminalName::WarpTerminal
            | TerminalName::WezTerm
    )
}

#[cfg(test)]
mod tests {
    use super::detect_backend;
    use super::supports_osc9;
    use codex_config::types::NotificationMethod;
    use codex_terminal_detection::TerminalInfo;
    use codex_terminal_detection::TerminalName;
    use pretty_assertions::assert_eq;

    fn test_terminal(name: TerminalName) -> TerminalInfo {
        TerminalInfo {
            backend: None,
            name,
            term_program: None,
            version: None,
            term: None,
            multiplexer: None,
        }
    }

    #[test]
    fn selects_explicit_notification_method() {
        for method in [
            NotificationMethod::Osc9,
            NotificationMethod::Osc777,
            NotificationMethod::Bel,
        ] {
            assert_eq!(detect_backend(method).method(), method.to_string());
        }
    }

    #[test]
    fn supports_osc9_for_supported_terminals() {
        for name in [
            TerminalName::Ghostty,
            TerminalName::Iterm2,
            TerminalName::Kitty,
            TerminalName::WarpTerminal,
            TerminalName::WezTerm,
        ] {
            assert!(
                supports_osc9(&test_terminal(name)),
                "{name:?} should support OSC 9"
            );
        }
    }

    #[test]
    fn supports_osc9_for_unsupported_terminals() {
        for name in [
            TerminalName::AppleTerminal,
            TerminalName::Alacritty,
            TerminalName::Dumb,
            TerminalName::GnomeTerminal,
            TerminalName::Konsole,
            TerminalName::Unknown,
            TerminalName::VsCode,
            TerminalName::Vte,
            TerminalName::WindowsTerminal,
        ] {
            assert_eq!(
                supports_osc9(&test_terminal(name)),
                false,
                "{name:?} should not support OSC 9"
            );
        }
    }
}
