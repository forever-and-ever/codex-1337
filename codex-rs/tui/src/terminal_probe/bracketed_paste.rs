use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static CONFIRMED: AtomicBool = AtomicBool::new(false);

pub(super) const QUERY: &[u8] = b"\x1b[?2004$p";
pub(super) const REPLY_LEN: usize = 11;

pub(crate) fn disable_paste_burst(configured: bool) -> bool {
    configured || CONFIRMED.load(Ordering::Relaxed)
}

pub(super) fn observe(enabled: Option<bool>) {
    CONFIRMED.store(enabled == Some(true), Ordering::Relaxed);
}

pub(super) fn parse(input: &[u8]) -> Option<bool> {
    input.windows(REPLY_LEN).find_map(reply)
}

pub(super) fn reply(input: &[u8]) -> Option<bool> {
    let [
        b'\x1b',
        b'[',
        b'?',
        b'2',
        b'0',
        b'0',
        b'4',
        b';',
        state,
        b'$',
        b'y',
        ..,
    ] = input
    else {
        return None;
    };
    match state {
        b'1' | b'3' => Some(true),
        b'0' | b'2' | b'4' => Some(false),
        _ => None,
    }
}
