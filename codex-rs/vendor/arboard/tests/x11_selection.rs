//! Real X11 protocol/handoff coverage. Never run against the live desktop.
#![cfg(target_os = "linux")]

use std::borrow::Cow;
use std::error::Error;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use arboard::{Clipboard, ImageData};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
	Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
	Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_FROM_PARENT, CURRENT_TIME, NONE};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

x11rb::atom_manager! {
	Atoms: AtomsCookie {
		CLIPBOARD, CLIPBOARD_MANAGER, PRIMARY, TARGETS, MULTIPLE, ATOM_PAIR,
		UTF8_STRING, HTML: b"text/html", PNG: b"image/png",
		UTF8_0: b"text/plain;charset=utf-8", UTF8_1: b"text/plain;charset=UTF-8",
		PAIRS, OUTPUT_A, OUTPUT_B, OUTPUT_C, OUTPUT_D, UNSUPPORTED,
	}
}

struct Peer {
	conn: RustConnection,
	window: Window,
	atoms: Atoms,
}

impl Peer {
	fn new() -> Result<Self> {
		let (conn, screen) = x11rb::connect(None)?;
		let window = conn.generate_id()?;
		conn.create_window(
			0,
			window,
			conn.setup().roots[screen].root,
			0,
			0,
			1,
			1,
			0,
			WindowClass::INPUT_ONLY,
			COPY_FROM_PARENT,
			&CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
		)?
		.check()?;
		let atoms = Atoms::new(&conn)?.reply()?;
		Ok(Self { conn, window, atoms })
	}

	fn request(&self, target: Atom, property: Atom) -> Result<SelectionNotifyEvent> {
		// Use a real server timestamp, so notification-time preservation is tested
		// without relying on how the server expands CURRENT_TIME.
		self.conn.get_input_focus()?.reply()?;
		while self.conn.poll_for_event()?.is_some() {}
		self.conn
			.change_property8(
				PropMode::REPLACE,
				self.window,
				self.atoms.OUTPUT_D,
				AtomEnum::STRING,
				b"",
			)?
			.check()?;
		let deadline = Instant::now() + Duration::from_secs(8);
		let request_time = loop {
			if let Some(Event::PropertyNotify(event)) = self.conn.poll_for_event()? {
				break event.time;
			}
			assert!(Instant::now() < deadline, "server timestamp timed out");
			sleep(Duration::from_millis(2));
		};
		self.conn.convert_selection(
			self.window,
			self.atoms.CLIPBOARD,
			target,
			property,
			request_time,
		)?;
		self.conn.flush()?;
		let deadline = Instant::now() + Duration::from_secs(8);
		loop {
			if let Some(Event::SelectionNotify(event)) = self.conn.poll_for_event()? {
				assert_eq!(event.requestor, self.window);
				assert_eq!(event.selection, self.atoms.CLIPBOARD);
				assert_eq!(event.target, target);
				assert_eq!(event.time, request_time);
				// All writes precede the one notification, including MULTIPLE.
				self.conn.get_input_focus()?.reply()?;
				while let Some(extra) = self.conn.poll_for_event()? {
					assert!(!matches!(extra, Event::SelectionNotify(_)), "{extra:?}");
				}
				return Ok(event);
			}
			assert!(Instant::now() < deadline, "selection notification timed out");
			sleep(Duration::from_millis(2));
		}
	}

	fn bytes(&self, target: Atom) -> Result<Vec<u8>> {
		let property = self.atoms.OUTPUT_A;
		assert_eq!(self.request(target, property)?.property, property);
		let reply = self
			.conn
			.get_property(true, self.window, property, AtomEnum::ANY, 0, 1 << 20)?
			.reply()?;
		let incr = self.conn.intern_atom(false, b"INCR")?.reply()?.atom;
		if reply.type_ != incr {
			assert_eq!(reply.type_, target);
			assert_eq!(reply.format, 8);
			assert_eq!(reply.bytes_after, 0);
			return Ok(reply.value);
		}
		let mut bytes = Vec::new();
		let deadline = Instant::now() + Duration::from_secs(8);
		loop {
			if let Some(Event::PropertyNotify(event)) = self.conn.poll_for_event()? {
				if event.atom == property
					&& event.state == x11rb::protocol::xproto::Property::NEW_VALUE
				{
					let chunk = self
						.conn
						.get_property(true, self.window, property, AtomEnum::ANY, 0, 1 << 20)?
						.reply()?;
					assert_eq!(chunk.type_, target);
					assert_eq!(chunk.format, 8);
					assert_eq!(chunk.bytes_after, 0);
					if chunk.value.is_empty() {
						return Ok(bytes);
					}
					bytes.extend(chunk.value);
					assert!(bytes.len() <= 1 << 22, "unbounded test data");
				}
			}
			assert!(Instant::now() < deadline, "incremental selection timed out");
			sleep(Duration::from_millis(2));
		}
	}

	fn pairs(&self, values: &[u32], type_: Atom) -> Result<()> {
		self.conn
			.change_property32(PropMode::REPLACE, self.window, self.atoms.PAIRS, type_, values)?
			.check()?;
		Ok(())
	}

	fn owner(&self, selection: Atom) -> Result<Window> {
		Ok(self.conn.get_selection_owner(selection)?.reply()?.owner)
	}
}

struct Keeper(Child);

impl Keeper {
	fn start() -> Result<Self> {
		let runner = std::env::var("CODEX_CLIPBOARD_KEEPER")?;
		Ok(Self(
			Command::new("/usr/bin/python3")
				.arg(runner)
				.env("HOME", std::env::var("CODEX_CLIPBOARD_HOME")?)
				.env("TMPDIR", "/tmp")
				.stdin(Stdio::null())
				.process_group(0)
				.spawn()?,
		))
	}

	fn stop(&mut self) -> Result<()> {
		// Kill/reap the process group, including toolkit decoder helpers.
		let status = Command::new("/usr/bin/kill")
			.args(["-TERM", "--", &format!("-{}", self.0.id())])
			.status()?;
		assert!(status.success());
		assert!(self.0.wait()?.success());
		Ok(())
	}
}

impl Drop for Keeper {
	fn drop(&mut self) {
		if matches!(self.0.try_wait(), Ok(None)) {
			let _ = Command::new("/usr/bin/kill")
				.args(["-KILL", "--", &format!("-{}", self.0.id())])
				.status();
			let _ = self.0.wait();
		}
	}
}

fn wait_for(mut predicate: impl FnMut() -> Result<bool>) -> Result<()> {
	let deadline = Instant::now() + Duration::from_secs(8);
	while !predicate()? {
		assert!(Instant::now() < deadline, "clipboard ownership timed out");
		sleep(Duration::from_millis(5));
	}
	Ok(())
}

#[test]
#[ignore = "requires private isolated-X11 launcher; see DOWNSTREAM.md"]
fn multiple_protocol_and_current_clipboard_handoff() -> Result<()> {
	// Explicit opt-in is required in addition to the ignored-test switch.
	assert_eq!(std::env::var("CODEX_PRIVATE_X11").as_deref(), Ok("1"));
	let peer = Peer::new()?;
	let a = &peer.atoms;
	assert_eq!(peer.owner(a.CLIPBOARD_MANAGER)?, NONE, "private display required");
	peer.conn.set_selection_owner(peer.window, a.PRIMARY, CURRENT_TIME)?.check()?;

	let mut source = Clipboard::new()?;
	source.set_html("<b>protocol fixture</b>", Some("protocol fixture"))?;
	assert_eq!(peer.bytes(a.UTF8_0)?, b"protocol fixture");
	assert_eq!(peer.bytes(a.UTF8_1)?, b"protocol fixture");
	assert_eq!(peer.request(a.UTF8_STRING, NONE)?.property, a.UTF8_STRING);
	assert_eq!(peer.request(a.UNSUPPORTED, a.OUTPUT_D)?.property, NONE);

	assert_eq!(peer.request(a.TARGETS, a.OUTPUT_A)?.property, a.OUTPUT_A);
	let targets =
		peer.conn.get_property(true, peer.window, a.OUTPUT_A, AtomEnum::ATOM, 0, 64)?.reply()?;
	assert!(targets.value32().ok_or("TARGETS format")?.any(|x| x == a.MULTIPLE));

	// Mixed success must not turn the whole request into failure, recurse,
	// emit per-pair notifications, or overwrite the controlling pair list.
	let invalid_property = 0x7fff_ffff;
	peer.pairs(
		&[
			a.UTF8_STRING,
			a.OUTPUT_A,
			a.HTML,
			a.OUTPUT_B,
			a.UNSUPPORTED,
			a.OUTPUT_C,
			a.MULTIPLE,
			a.OUTPUT_D,
			a.UTF8_STRING,
			NONE,
			a.UTF8_STRING,
			a.PAIRS,
			a.UTF8_STRING,
			invalid_property,
		],
		a.ATOM_PAIR,
	)?;
	assert_eq!(peer.request(a.MULTIPLE, a.PAIRS)?.property, a.PAIRS);
	let result =
		peer.conn.get_property(false, peer.window, a.PAIRS, a.ATOM_PAIR, 0, 64)?.reply()?;
	assert_eq!(
		result.value32().ok_or("pair format")?.collect::<Vec<_>>(),
		[
			a.UTF8_STRING,
			a.OUTPUT_A,
			a.HTML,
			a.OUTPUT_B,
			a.UNSUPPORTED,
			NONE,
			a.MULTIPLE,
			NONE,
			a.UTF8_STRING,
			NONE,
			a.UTF8_STRING,
			NONE,
			a.UTF8_STRING,
			NONE,
		]
	);
	let text =
		peer.conn.get_property(true, peer.window, a.OUTPUT_A, a.UTF8_STRING, 0, 64)?.reply()?;
	assert_eq!(text.value, b"protocol fixture");
	let html = peer.conn.get_property(true, peer.window, a.OUTPUT_B, a.HTML, 0, 64)?.reply()?;
	assert_eq!(html.value, b"<b>protocol fixture</b>");

	assert_eq!(peer.request(a.MULTIPLE, NONE)?.property, NONE);
	peer.conn.delete_property(peer.window, a.PAIRS)?.check()?;
	assert_eq!(peer.request(a.MULTIPLE, a.PAIRS)?.property, NONE);
	// Rejection happens before converting a valid prefix.
	peer.conn
		.change_property8(PropMode::REPLACE, peer.window, a.OUTPUT_A, a.UTF8_STRING, b"sentinel")?
		.check()?;
	for (values, type_) in [
		(vec![a.UTF8_STRING, a.OUTPUT_A], a.UTF8_STRING),
		(vec![a.UTF8_STRING, a.OUTPUT_A, a.HTML], a.ATOM_PAIR),
		([a.UTF8_STRING, a.OUTPUT_A].repeat(1025), a.ATOM_PAIR),
	] {
		peer.pairs(&values, type_)?;
		assert_eq!(peer.request(a.MULTIPLE, a.PAIRS)?.property, NONE);
		let unchanged = peer
			.conn
			.get_property(false, peer.window, a.OUTPUT_A, a.UTF8_STRING, 0, 64)?
			.reply()?;
		assert_eq!(unchanged.value, b"sentinel");
	}
	peer.conn
		.change_property8(PropMode::REPLACE, peer.window, a.PAIRS, a.ATOM_PAIR, b"bad format")?
		.check()?;
	assert_eq!(peer.request(a.MULTIPLE, a.PAIRS)?.property, NONE);
	peer.pairs(&[], a.ATOM_PAIR)?;
	assert_eq!(peer.request(a.MULTIPLE, a.PAIRS)?.property, a.PAIRS);
	drop(source);
	assert_eq!(peer.owner(a.CLIPBOARD)?, NONE);

	let mut keeper = Keeper::start()?;
	wait_for(|| Ok(peer.owner(a.CLIPBOARD_MANAGER)? != NONE))?;
	let manager = peer.owner(a.CLIPBOARD_MANAGER)?;
	let mut duplicate = Keeper::start()?;
	wait_for(|| Ok(duplicate.0.try_wait()?.is_some()))?;
	assert!(!duplicate.0.wait()?.success());
	assert_eq!(peer.owner(a.CLIPBOARD_MANAGER)?, manager);

	let text = "x".repeat(262144);
	let html = format!("<b>{text}</b>");
	let mut source = Clipboard::new()?;
	source.set_html(html.clone(), Some(text.clone()))?;
	drop(source);
	assert_eq!(peer.bytes(a.UTF8_STRING)?, text.as_bytes());
	assert_eq!(peer.bytes(a.HTML)?, html.as_bytes());
	assert_eq!(peer.owner(a.PRIMARY)?, peer.window);

	let mut source = Clipboard::new()?;
	source.set_image(ImageData {
		width: 2,
		height: 2,
		bytes: Cow::Owned(vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]),
	})?;
	let image_owner = peer.owner(a.CLIPBOARD)?;
	let original = peer.bytes(a.PNG)?;
	assert!(original.starts_with(b"\x89PNG\r\n\x1a\n"));
	wait_for(|| {
		let owner = peer.owner(a.CLIPBOARD)?;
		Ok(owner != NONE && owner != image_owner)
	})?;
	drop(source);
	assert_eq!(peer.bytes(a.PNG)?, original);
	assert_eq!(peer.owner(a.PRIMARY)?, peer.window);
	keeper.stop()?;
	assert_eq!(peer.owner(a.CLIPBOARD_MANAGER)?, NONE);
	assert_eq!(peer.owner(a.CLIPBOARD)?, NONE);
	assert_eq!(peer.owner(a.PRIMARY)?, peer.window);
	Ok(())
}
