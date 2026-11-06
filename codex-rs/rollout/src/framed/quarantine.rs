//! Keeps what sealing cuts: the bytes past the cut move to a durable sidecar before truncation.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use super::FRAME_MAGIC;
use super::TRAILER_BODY_LEN;
use super::TRAILER_LEN;
use super::TRAILER_MAGIC;
use super::read_exact_at;
use super::to_index;
use super::u32_le;

const COPY_CHUNK: u64 = 1 << 20;

/// Copies `file`'s bytes from `cut` to its end into `<path>.torn-<unix ms>` and makes the copy
/// durable; the caller truncates only afterwards. A crash tears only the frame or trailer being
/// written, so dropped bytes holding anything more are logged at ERROR as corruption.
pub(super) fn quarantine(path: &Path, file: &File, cut: u64) -> io::Result<()> {
    let len = file.metadata()?.len();
    if cut >= len {
        return Ok(());
    }
    let (sidecar_path, mut sidecar) = create_sidecar(path)?;
    let mut scan = StructureScan::default();
    let mut chunk = Vec::new();
    let mut at = cut;
    while at < len {
        let count = (len - at).min(COPY_CHUNK);
        chunk.resize(to_index(count)?, 0);
        read_exact_at(file, &mut chunk, at)?;
        scan.feed(&chunk);
        sidecar.write_all(&chunk)?;
        at += count;
    }
    sidecar.sync_all()?;
    #[cfg(unix)]
    if let Some(directory) = sidecar_path.parent() {
        File::open(directory)?.sync_all()?;
    }
    let (rollout, sidecar) = (path.display(), sidecar_path.display());
    let dropped = len - cut;
    if scan.found() {
        tracing::error!(
            %rollout,
            %sidecar,
            kept = cut,
            dropped,
            "sealing a framed rollout cut bytes holding a later frame or trailer: corruption, not a torn tail"
        );
    } else {
        tracing::warn!(
            %rollout,
            %sidecar,
            kept = cut,
            dropped,
            "sealing a framed rollout cut its torn tail"
        );
    }
    Ok(())
}

fn create_sidecar(path: &Path) -> io::Result<(PathBuf, File)> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        )
    })?;
    let mut stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    loop {
        let mut sidecar_name = name.to_os_string();
        sidecar_name.push(format!(".torn-{stamp}"));
        let sidecar_path = path.with_file_name(sidecar_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&sidecar_path) {
            Ok(sidecar) => return Ok((sidecar_path, sidecar)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => stamp += 1,
            Err(err) => return Err(err),
        }
    }
}

/// Finds what a torn tail cannot hold: a frame start past its first byte, or a complete trailer.
#[derive(Default)]
pub(super) struct StructureScan {
    /// The last bytes of the previous chunk, which may begin a trailer this chunk completes.
    carry: Vec<u8>,
    /// Offset, within the dropped bytes, of `carry`'s first byte.
    carry_offset: u64,
    found: bool,
}

impl StructureScan {
    pub(super) fn feed(&mut self, chunk: &[u8]) {
        if self.found {
            return;
        }
        let mut window = std::mem::take(&mut self.carry);
        window.extend_from_slice(chunk);
        self.found = (0..window.len().saturating_sub(3)).any(|index| {
            let magic = u32_le(&window[index..]);
            let offset = self.carry_offset + index as u64;
            (magic == FRAME_MAGIC && offset > 0)
                || (magic == TRAILER_MAGIC
                    && index + TRAILER_LEN <= window.len()
                    && u32_le(&window[index + 4..]) == TRAILER_BODY_LEN)
        });
        let keep = window.len().min(TRAILER_LEN - 1);
        self.carry_offset += (window.len() - keep) as u64;
        self.carry = window.split_off(window.len() - keep);
    }

    pub(super) fn found(&self) -> bool {
        self.found
    }
}
