//! Thread marks are X11 resources owned by the displaying TUI. No mapped window, registry, or
//! cleanup command is needed: X destroys the mark when either the connection or terminal dies.

use crate::tui::desktop_attention::read_i3_payload;
use crate::tui::desktop_attention::send_i3_message;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use rustix::event::PollFd;
use rustix::event::PollFlags;
use rustix::event::poll;
use rustix::net::AddressFamily;
use rustix::net::SocketAddrUnix;
use rustix::net::SocketFlags;
use rustix::net::SocketType;
use serde::Deserialize;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tokio::net::UnixStream;
use tokio::runtime::Handle;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::xproto::Atom;
use x11rb::protocol::xproto::AtomEnum;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xproto::CreateWindowAux;
use x11rb::protocol::xproto::PropMode;
use x11rb::protocol::xproto::Window;
use x11rb::protocol::xproto::WindowClass;
use x11rb::reexports::x11rb_protocol::parse_display::parse_display;
use x11rb::reexports::x11rb_protocol::xauth::get_auth;
use x11rb::rust_connection::DefaultStream;
use x11rb::rust_connection::PollMode;
use x11rb::rust_connection::RustConnection;
use x11rb::rust_connection::Stream;
use x11rb::utils::RawFdContainer;
use x11rb::wrapper::ConnectionExt as _;

const MARK: &[u8] = b"_CODEX_1337_THREAD_V1";
const DESKTOP_TIMEOUT: Duration = Duration::from_secs(2);
const I3_COMMAND: u32 = 0;
const I3_GET_TREE: u32 = 4;

type XConnection = RustConnection<DesktopStream>;

pub(super) struct Publisher {
    marker: Option<Marker>,
}

struct Marker {
    connection: XConnection,
    window: Window,
    atom: Atom,
    key: Option<[u8; 32]>,
}

impl Publisher {
    pub(super) fn new() -> Result<Self> {
        let parent: Window = std::env::var("ALACRITTY_WINDOW_ID")
            .context("the current terminal has no Alacritty X11 window")?
            .parse()
            .context("invalid Alacritty X11 window ID")?;
        let (connection, _) = connect_x(Instant::now() + DESKTOP_TIMEOUT)?;
        let atom = connection.intern_atom(false, MARK)?.reply()?.atom;
        let window = connection.generate_id()?;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                parent,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )?
            .check()
            .context("create the terminal's thread mark")?;
        Ok(Self {
            marker: Some(Marker {
                connection,
                window,
                atom,
                key: None,
            }),
        })
    }

    pub(super) fn publish(&mut self, key: Option<&[u8; 32]>) -> Result<()> {
        let marker = self.marker.as_mut().context("thread mark has been lost")?;
        let result = marker.publish(key);
        if result.is_err() {
            // An ambiguous update must not keep advertising the previous thread.
            self.marker = None;
        }
        result
    }
}

impl Marker {
    fn publish(&mut self, key: Option<&[u8; 32]>) -> Result<()> {
        if self.key.as_ref() == key {
            return Ok(());
        }
        self.connection
            .stream()
            .deadline
            .set(Instant::now() + DESKTOP_TIMEOUT);
        match key {
            Some(key) => self
                .connection
                .change_property8(
                    PropMode::REPLACE,
                    self.window,
                    self.atom,
                    AtomEnum::STRING,
                    key,
                )?
                .check()?,
            None => self
                .connection
                .delete_property(self.window, self.atom)?
                .check()?,
        }
        self.key = key.copied();
        Ok(())
    }
}

pub(super) fn focus(key: &[u8; 32]) -> Result<bool> {
    let handle = Handle::try_current().context("focus must run in a Tokio blocking worker")?;
    let deadline = Instant::now() + DESKTOP_TIMEOUT;
    let (connection, root) = connect_x(deadline)?;
    let mut i3 = I3::connect(&connection, root, handle, deadline)?;
    let mark = connection.intern_atom(true, MARK)?.reply()?.atom;
    // A mark may be changed or destroyed between the two desktop protocols. Re-resolve once
    // rather than launching a duplicate from a stale scan or reporting a different window as focused.
    for _ in 0..2 {
        let tree = i3.tree()?;
        let Some((terminal, marker)) = locate(&connection, &tree, mark, key)? else {
            return Ok(false);
        };
        let accepted = i3.focus_window(terminal)?;
        let tree = i3.tree()?;
        if !still_marked(&connection, terminal, marker, mark, key)? {
            continue;
        }
        ensure!(accepted, "i3 rejected focus for the terminal");
        ensure!(
            tree.windows()
                .any(|(window, focused)| window == terminal && focused),
            "i3 did not focus the marked terminal"
        );
        return Ok(true);
    }
    anyhow::bail!("the marked terminal changed during focus")
}

fn locate(
    connection: &XConnection,
    tree: &I3Tree,
    mark: Atom,
    key: &[u8; 32],
) -> Result<Option<(Window, Window)>> {
    if mark == u32::from(AtomEnum::NONE) {
        return Ok(None);
    }

    let mut windows: Vec<_> = tree.windows().collect();
    windows.sort_unstable_by_key(|(window, focused)| (!*focused, *window));
    let children = windows
        .into_iter()
        .map(|(window, _)| Ok((window, connection.query_tree(window)?)))
        .collect::<Result<Vec<_>>>()?;
    for (terminal, children) in children {
        let Some(children) = live_window(children.reply())? else {
            continue;
        };
        let marks = children
            .children
            .into_iter()
            .map(|child| {
                Ok((
                    child,
                    connection.get_property(false, child, mark, AtomEnum::STRING, 0, 8)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        for (marker, reply) in marks {
            if marked(live_window(reply.reply())?, key)
                && still_marked(connection, terminal, marker, mark, key)?
            {
                return Ok(Some((terminal, marker)));
            }
        }
    }
    Ok(None)
}

fn still_marked(
    connection: &XConnection,
    terminal: Window,
    marker: Window,
    atom: Atom,
    key: &[u8; 32],
) -> Result<bool> {
    let parent = live_window(connection.query_tree(marker)?.reply())?;
    if parent.is_none_or(|node| node.parent != terminal) {
        return Ok(false);
    }
    let attributes = live_window(connection.get_window_attributes(marker)?.reply())?;
    if attributes.is_none_or(|attributes| attributes.class != WindowClass::INPUT_ONLY) {
        return Ok(false);
    }
    let property = live_window(
        connection
            .get_property(false, marker, atom, AtomEnum::STRING, 0, 8)?
            .reply(),
    )?;
    Ok(marked(property, key))
}

fn marked(property: Option<x11rb::protocol::xproto::GetPropertyReply>, key: &[u8; 32]) -> bool {
    property.is_some_and(|property| {
        property.type_ == u32::from(AtomEnum::STRING)
            && property.format == 8
            && property.bytes_after == 0
            && property.value == key
    })
}

fn live_window<T>(result: std::result::Result<T, ReplyError>) -> Result<Option<T>> {
    match result {
        Ok(reply) => Ok(Some(reply)),
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Deserialize)]
struct I3Tree {
    window: Option<Window>,
    focused: bool,
    nodes: Vec<Self>,
    floating_nodes: Vec<Self>,
}

impl I3Tree {
    fn windows(&self) -> impl Iterator<Item = (Window, bool)> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            while let Some(node) = stack.pop() {
                stack.extend(node.nodes.iter().chain(&node.floating_nodes));
                if let Some(window) = node.window {
                    return Some((window, node.focused));
                }
            }
            None
        })
    }
}

#[derive(Deserialize)]
struct CommandReply {
    success: bool,
}

struct I3 {
    socket: UnixStream,
    handle: Handle,
    deadline: Instant,
}

impl I3 {
    fn connect(
        connection: &XConnection,
        root: Window,
        handle: Handle,
        deadline: Instant,
    ) -> Result<Self> {
        // Use i3's own advertised socket, not an environment pointing at an unrelated i3 instance.
        let atom = connection
            .intern_atom(true, b"I3_SOCKET_PATH")?
            .reply()?
            .atom;
        ensure!(
            atom != u32::from(AtomEnum::NONE),
            "i3 is not available on this X11 display"
        );
        // i3 4.25 publishes UTF8_STRING; also accept the legacy STRING property.
        let property = connection
            .get_property(false, root, atom, AtomEnum::ANY, 0, 32)?
            .reply()?;
        let path_type = property.type_ == u32::from(AtomEnum::STRING)
            || (property.type_ != u32::from(AtomEnum::NONE)
                && property.type_ == connection.intern_atom(true, b"UTF8_STRING")?.reply()?.atom);
        ensure!(
            path_type && property.format == 8 && property.bytes_after == 0,
            "i3 did not publish a usable IPC socket"
        );
        let bytes = property.value.strip_suffix(&[0]).unwrap_or(&property.value);
        let path = Path::new(OsStr::from_bytes(bytes));
        ensure!(
            path.is_absolute() && !bytes.contains(&0),
            "i3 did not publish an absolute IPC socket"
        );
        let socket = connect_unix(&SocketAddrUnix::new(path)?).context("connect to i3 IPC")?;
        Ok(Self {
            socket: UnixStream::from_std(socket)?,
            handle,
            deadline,
        })
    }

    fn focus_window(&mut self, terminal: Window) -> Result<bool> {
        // i3 parses a command language. Only a protocol-typed XID enters this fixed command.
        let replies: Vec<CommandReply> =
            self.request(I3_COMMAND, format!("[id={terminal}] focus").as_bytes())?;
        Ok(!replies.is_empty() && replies.iter().all(|item| item.success))
    }

    fn tree(&mut self) -> Result<I3Tree> {
        self.request(I3_GET_TREE, &[])
    }

    fn request<T: serde::de::DeserializeOwned>(&mut self, kind: u32, payload: &[u8]) -> Result<T> {
        self.handle.block_on(async {
            tokio::time::timeout_at(self.deadline.into(), async {
                send_i3_message(&mut self.socket, kind, payload).await?;
                let (reply_kind, reply) = read_i3_payload(&mut self.socket).await?;
                ensure!(reply_kind == kind, "unexpected i3 IPC response type");
                serde_json::from_slice(&reply).context("invalid i3 response payload")
            })
            .await
            .context("i3 IPC timed out")?
        })
    }
}

fn connect_x(deadline: Instant) -> Result<(XConnection, Window)> {
    let display = parse_display(None).context("no usable X11 display")?;
    ensure!(
        matches!(display.host.as_str(), "" | "unix")
            && matches!(display.protocol.as_deref(), None | Some("unix")),
        "the thread desktop requires a local X11 display"
    );
    let path = format!("/tmp/.X11-unix/X{}", display.display);
    let socket = connect_unix(&SocketAddrUnix::new_abstract_name(path.as_bytes())?)
        .or_else(|_| connect_unix(&SocketAddrUnix::new(Path::new(&path))?))
        .context("connect to the local X11 display")?;
    let (stream, (family, address)) = DefaultStream::from_unix_stream(socket)?;
    let (auth_name, auth_data) = get_auth(family, &address, display.display)
        .ok()
        .flatten()
        .unwrap_or_default();
    let screen = usize::from(display.screen);
    let connection = RustConnection::connect_to_stream_with_auth_info(
        DesktopStream {
            stream,
            deadline: Cell::new(deadline),
        },
        screen,
        auth_name,
        auth_data,
    )?;
    let root = connection.setup().roots[screen].root;
    Ok((connection, root))
}

fn connect_unix(address: &SocketAddrUnix) -> io::Result<StdUnixStream> {
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    rustix::net::connect(&socket, address)?;
    Ok(StdUnixStream::from(socket))
}

fn wait_ready(fd: &impl AsFd, events: PollFlags, deadline: Instant) -> io::Result<()> {
    let mut fds = [PollFd::new(fd, events)];
    loop {
        let timeout = time_left(deadline)?
            .try_into()
            .map_err(|_| io::Error::other("invalid desktop IPC deadline"))?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn time_left(deadline: Instant) -> io::Result<Duration> {
    match deadline.saturating_duration_since(Instant::now()) {
        remaining if remaining.is_zero() => Err(io::ErrorKind::TimedOut.into()),
        remaining => Ok(remaining),
    }
}

struct DesktopStream {
    stream: DefaultStream,
    deadline: Cell<Instant>,
}

impl Stream for DesktopStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        let mut events = PollFlags::empty();
        events.set(PollFlags::IN, mode.readable());
        events.set(PollFlags::OUT, mode.writable());
        wait_ready(&self.stream, events, self.deadline.get())
    }

    fn read(&self, bytes: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        time_left(self.deadline.get())?;
        self.stream.read(bytes, fds)
    }

    fn write(&self, bytes: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        time_left(self.deadline.get())?;
        self.stream.write(bytes, fds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires Alacritty windows on private Xvfb and i3"]
    async fn thread_mark_tracks_the_focused_terminal() -> Result<()> {
        tokio::task::spawn_blocking(check_thread_mark).await?
    }

    fn check_thread_mark() -> Result<()> {
        let expected_socket = std::path::PathBuf::from(
            std::env::var_os("CODEX_THREAD_LINK_TEST_I3SOCK")
                .context("test requires an explicit isolated i3 socket")?,
        );
        let runtime_dir = std::path::PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("test requires an isolated runtime dir")?,
        );
        ensure!(
            runtime_dir.is_absolute() && expected_socket.starts_with(&runtime_dir),
            "test i3 must be under the isolated XDG_RUNTIME_DIR"
        );
        let terminal: Window = std::env::var("ALACRITTY_WINDOW_ID")?.parse()?;
        let other: Window = std::env::var("CODEX_THREAD_LINK_TEST_OTHER_XID")?.parse()?;
        ensure!(
            terminal != other,
            "the focus witness must be another window"
        );

        let deadline = Instant::now() + DESKTOP_TIMEOUT;
        let (connection, root) = connect_x(deadline)?;
        let mut i3 = I3::connect(&connection, root, Handle::current(), deadline)?;
        assert_eq!(
            i3.socket.peer_addr()?.as_pathname(),
            Some(expected_socket.as_path()),
            "refusing to focus any window on a different i3 socket"
        );
        assert!(i3.focus_window(other)?);
        assert!(i3.tree()?.windows().any(|window| window == (other, true)));

        let first = [0x56; 32];
        let second = [0x57; 32];
        let mut publisher = Publisher::new()?;
        publisher.publish(Some(&first))?;
        assert!(focus(&first)?);

        i3.deadline = Instant::now() + DESKTOP_TIMEOUT;
        assert!(
            i3.tree()?
                .windows()
                .any(|window| window == (terminal, true))
        );

        publisher.publish(Some(&second))?;
        assert!(!focus(&first)?);
        assert!(focus(&second)?);

        publisher.publish(None)?;
        assert!(!focus(&second)?);
        publisher.publish(Some(&first))?;
        let marker = publisher
            .marker
            .as_ref()
            .context("publisher lost its mark")?
            .window;
        connection
            .stream()
            .deadline
            .set(Instant::now() + DESKTOP_TIMEOUT);
        connection
            .change_window_attributes(
                marker,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .event_mask(x11rb::protocol::xproto::EventMask::STRUCTURE_NOTIFY),
            )?
            .check()?;
        drop(publisher);
        assert!(matches!(
            connection.wait_for_event()?,
            x11rb::protocol::Event::DestroyNotify(event) if event.window == marker
        ));
        assert!(!focus(&first)?);
        Ok(())
    }
}
