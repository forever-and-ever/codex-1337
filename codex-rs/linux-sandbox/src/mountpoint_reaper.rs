//! Reaps the empty mountpoints that bubblewrap leaves in writable roots.
//!
//! A read-only path missing from a writable root is masked by a mount, so a
//! mountpoint must exist on the host while the sandbox runs. The wrapper removes
//! it when the last registrant exits, but a SIGKILLed wrapper or a cgroup kill
//! skips that cleanup, and a leftover `.git` makes its parent a project root.
//!
//! - The wrapper creates each missing mountpoint itself, without following
//!   symlinks, and brands it with the user xattr `BRAND`.
//! - Its registry marker records the mountpoint's path, kind, device and inode,
//!   and the registrant's pid, start time and pid namespace.
//! - An existing empty target counts as synthetic, so its last registrant
//!   removes it, only if it carries the brand or a marker, live or dead, records
//!   that path with the same device and inode.
//! - Each registration sweeps the registry: where every marker of a path is
//!   dead, the path is removed if it is still the recorded empty mountpoint,
//!   reached without symlinks.
//!
//! The registry lives in the per-boot runtime directory, which sandboxes see
//! read-only, so every removal rests on trusted evidence. A reboot erases the
//! registry; a branded leftover then stays until a run reuses its root, which
//! claims it through the brand. Without user xattrs that leftover passes for a
//! pre-existing path.
//!
//! Unlinking a mountpoint silently detaches the mount from every namespace that
//! still uses it, so liveness errs toward keeping. A marker from another pid
//! namespace cannot be judged and counts as live, and the wrapper enlists its
//! bubblewrap child, whose lifetime bounds the sandbox.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use rustix::fs::AtFlags;
use rustix::fs::CWD;
use rustix::fs::FileType;
use rustix::fs::Mode;
use rustix::fs::OFlags;
use rustix::fs::ResolveFlags;
use rustix::fs::StatxFlags;
use rustix::fs::XattrFlags;
use rustix::io::Errno;

use super::PROTECTED_CREATE_MARKER;
use super::SYNTHETIC_MOUNT_MARKER_EXISTING;
use super::SYNTHETIC_MOUNT_MARKER_SYNTHETIC;
use super::SyntheticMountTargetRegistration;
use super::process_is_active;
use super::synthetic_mount_marker_contents;
use crate::bwrap::SyntheticMountTarget;
use crate::bwrap::SyntheticMountTargetKind;

/// Name of the registry inside the runtime directory.
pub(super) const REGISTRY: &str = "sandbox-mounts";
/// User xattr on every mountpoint the wrapper creates.
const BRAND: &str = "user.codex.sandbox_mountpoint";
/// Bounds on one sweep.
const SWEPT_PATHS_MAX: usize = 512;
const MARKERS_PER_PATH_MAX: usize = 64;
const MARKER_MAX_BYTES: u64 = 8192;

/// The process-lifetime directory that holds the registry. Each unit test
/// process gets a private one, removed when it exits.
pub(super) fn runtime_directory() -> std::io::Result<PathBuf> {
    #[cfg(test)]
    {
        static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
        extern "C" fn remove_directory() {
            if let Some(directory) = DIRECTORY.get() {
                let _ = fs::remove_dir_all(directory);
            }
        }
        if let Some(directory) = DIRECTORY.get() {
            return Ok(directory.clone());
        }
        let directory = tempfile::Builder::new()
            .prefix("codex-sandbox-runtime-")
            .tempdir()?
            .keep();
        if DIRECTORY.set(directory).is_ok() {
            unsafe { libc::atexit(remove_directory) };
        }
        Ok(DIRECTORY.get().cloned().unwrap_or_default())
    }
    #[cfg(not(test))]
    codex_uds::prepare_user_runtime_directory()
}

/// What a registry marker asserts about its registrant's hold on a path; its
/// first line, in the upstream marker format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stake {
    Synthetic,
    Existing,
    ProtectedCreate,
}

impl Stake {
    fn parse(line: &[u8]) -> Option<Self> {
        [
            (Self::Synthetic, SYNTHETIC_MOUNT_MARKER_SYNTHETIC),
            (Self::Existing, SYNTHETIC_MOUNT_MARKER_EXISTING),
            (Self::ProtectedCreate, PROTECTED_CREATE_MARKER),
        ]
        .into_iter()
        .find_map(|(stake, encoded)| (encoded.strip_suffix(b"\n") == Some(line)).then_some(stake))
    }
}

/// What tells a registrant apart from a later process that reuses its pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Incarnation {
    /// Field 22 of `/proc/<pid>/stat`.
    start_time: u64,
    /// Inode of the pid namespace in which the marker's pid is meaningful.
    pid_namespace: u64,
}

impl Incarnation {
    /// `process` is a `/proc` entry: `self` or a pid.
    fn of(process: &str) -> Option<Self> {
        let stat = fs::read_to_string(format!("/proc/{process}/stat")).ok()?;
        Some(Self {
            start_time: parse_stat(&stat)?.1,
            pid_namespace: fs::metadata(format!("/proc/{process}/ns/pid")).ok()?.ino(),
        })
    }
}

/// What tells an entry apart from a later one at the same place. The birth
/// time, where the filesystem keeps one, catches a recycled inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    birth: Option<(i64, u32)>,
}

impl Identity {
    fn is(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && match (self.birth, other.birth) {
                (Some(birth), Some(other)) => birth == other,
                _ => true,
            }
    }
}

/// An entry as `statx` reports it, never through a symlink.
struct Status {
    file_type: FileType,
    size: u64,
    identity: Identity,
}

impl Status {
    fn of<Fd: AsFd, P: rustix::path::Arg>(
        dirfd: Fd,
        path: P,
        flags: AtFlags,
    ) -> rustix::io::Result<Self> {
        let statx = rustix::fs::statx(
            dirfd,
            path,
            flags | AtFlags::SYMLINK_NOFOLLOW,
            StatxFlags::BASIC_STATS | StatxFlags::BTIME,
        )?;
        let birth = StatxFlags::from_bits_retain(statx.stx_mask)
            .contains(StatxFlags::BTIME)
            .then_some((statx.stx_btime.tv_sec, statx.stx_btime.tv_nsec));
        Ok(Self {
            file_type: FileType::from_raw_mode(statx.stx_mode.into()),
            size: statx.stx_size,
            identity: Identity {
                device: rustix::fs::makedev(statx.stx_dev_major, statx.stx_dev_minor),
                inode: statx.stx_ino,
                birth,
            },
        })
    }
}

/// A mountpoint the wrapper created, as it stood when registered. The path
/// has no symlink components.
#[derive(Clone, Debug)]
pub(super) struct Mountpoint {
    path: PathBuf,
    kind: SyntheticMountTargetKind,
    identity: Identity,
}

impl Mountpoint {
    fn tag(&self) -> char {
        match self.kind {
            SyntheticMountTargetKind::EmptyDirectory => 'd',
            SyntheticMountTargetKind::EmptyFile => 'f',
        }
    }

    fn matches(&self, status: &Status) -> bool {
        self.identity.is(&status.identity) && is_empty_mountpoint(self.kind, status)
    }
}

/// A registry marker. The file name is the registrant's pid; the contents are
/// the stake line, then `<start time> <pid namespace>`, then
/// `<d|f> <device> <inode> <birth|-> <path>` when the marker records a
/// mountpoint, with the birth time as `<seconds>.<nanoseconds>`.
/// Markers of older wrappers hold only the stake line.
struct Marker {
    pid: libc::pid_t,
    stake: Option<Stake>,
    incarnation: Option<Incarnation>,
    mountpoint: Option<Mountpoint>,
}

impl Marker {
    fn read(pid: libc::pid_t, path: &Path) -> Self {
        let mut bytes = Vec::new();
        if let Ok(file) = File::open(path) {
            let _ = file.take(MARKER_MAX_BYTES).read_to_end(&mut bytes);
        }
        let mut lines = bytes.splitn(3, |byte| *byte == b'\n');
        let stake = lines.next().and_then(Stake::parse);
        let incarnation = lines
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|line| {
                let (start_time, pid_namespace) = line.split_once(' ')?;
                Some(Incarnation {
                    start_time: start_time.parse().ok()?,
                    pid_namespace: pid_namespace.parse().ok()?,
                })
            });
        let mountpoint = lines.next().and_then(parse_mountpoint);
        Self {
            pid,
            stake,
            incarnation,
            mountpoint,
        }
    }

    fn is_live(&self) -> bool {
        match self.incarnation {
            None => process_is_active(self.pid),
            Some(incarnation) => {
                own_pid_namespace() != Some(incarnation.pid_namespace)
                    || process_is_live(self.pid, incarnation.start_time)
            }
        }
    }

    /// The mountpoint this marker proves synthetic.
    fn synthetic(&self) -> Option<&Mountpoint> {
        self.mountpoint
            .as_ref()
            .filter(|_| self.stake == Some(Stake::Synthetic))
    }
}

fn parse_mountpoint(record: &[u8]) -> Option<Mountpoint> {
    let mut fields = record.splitn(5, |byte| *byte == b' ');
    let kind = match fields.next()? {
        b"d" => SyntheticMountTargetKind::EmptyDirectory,
        b"f" => SyntheticMountTargetKind::EmptyFile,
        _ => return None,
    };
    let mut word = || std::str::from_utf8(fields.next()?).ok();
    let device = word()?.parse().ok()?;
    let inode = word()?.parse().ok()?;
    let birth = match word()? {
        "-" => None,
        birth => {
            let (seconds, nanoseconds) = birth.split_once('.')?;
            Some((seconds.parse().ok()?, nanoseconds.parse().ok()?))
        }
    };
    let path = PathBuf::from(OsStr::from_bytes(fields.next()?));
    path.is_absolute().then_some(Mountpoint {
        path,
        kind,
        identity: Identity {
            device,
            inode,
            birth,
        },
    })
}

/// This process's marker: the upstream stake line, extended with its
/// incarnation and the mountpoint it registered.
pub(super) fn marker(stake_line: &[u8], mountpoint: Option<Mountpoint>) -> Vec<u8> {
    encode(stake_line, Incarnation::of("self"), mountpoint.as_ref())
}

fn encode(
    stake_line: &[u8],
    incarnation: Option<Incarnation>,
    mountpoint: Option<&Mountpoint>,
) -> Vec<u8> {
    let mut bytes = stake_line.to_vec();
    if let Some(Incarnation {
        start_time,
        pid_namespace,
    }) = incarnation
    {
        bytes.extend_from_slice(format!("{start_time} {pid_namespace}").as_bytes());
    }
    bytes.push(b'\n');
    if let Some(mountpoint) = mountpoint {
        let Identity {
            device,
            inode,
            birth,
        } = mountpoint.identity;
        let birth = birth.map_or_else(
            || "-".to_string(),
            |(seconds, nanoseconds)| format!("{seconds}.{nanoseconds}"),
        );
        let tag = mountpoint.tag();
        bytes.extend_from_slice(format!("{tag} {device} {inode} {birth} ").as_bytes());
        bytes.extend_from_slice(mountpoint.path.as_os_str().as_bytes());
    }
    bytes
}

/// Every marker under `marker_dir`, or `None` when they cannot all be read.
fn census(marker_dir: &Path) -> Option<Vec<Marker>> {
    let mut markers = Vec::new();
    for entry in fs::read_dir(marker_dir).ok()? {
        let path = entry.ok()?.path();
        let pid = path.file_name()?.to_str()?.parse().ok()?;
        if markers.len() == MARKERS_PER_PATH_MAX {
            return None;
        }
        markers.push(Marker::read(pid, &path));
    }
    Some(markers)
}

/// Whether the registrant behind the marker at `path` may still run.
pub(super) fn registrant_is_live(pid: libc::pid_t, path: &Path) -> bool {
    Marker::read(pid, path).is_live()
}

fn own_pid_namespace() -> Option<u64> {
    static OWN: OnceLock<Option<u64>> = OnceLock::new();
    *OWN.get_or_init(|| Incarnation::of("self").map(|own| own.pid_namespace))
}

/// A zombie has already released its namespaces, so it holds no mount.
fn process_is_live(pid: libc::pid_t, start_time: u64) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => parse_stat(&stat)
            .is_none_or(|(state, started)| !matches!(state, "Z" | "X") && started == start_time),
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

/// The state and start time: fields 3 and 22 of `/proc/<pid>/stat`.
fn parse_stat(stat: &str) -> Option<(&str, u64)> {
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let state = fields.next()?;
    let start_time = fields.nth(18)?.parse().ok()?;
    Some((state, start_time))
}

/// Reaps the mountpoints of every path whose registrants are all dead. Runs
/// under the registry lock.
pub(super) fn sweep(registry: &Path) {
    let Ok(entries) = fs::read_dir(registry) else {
        return;
    };
    for entry in entries.take(SWEPT_PATHS_MAX) {
        let Ok(entry) = entry else {
            return;
        };
        if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let marker_dir = entry.path();
        let Some(markers) = census(&marker_dir) else {
            continue;
        };
        if markers.iter().any(Marker::is_live) {
            continue;
        }
        markers
            .iter()
            .filter_map(Marker::synthetic)
            .for_each(remove);
        let _ = fs::remove_dir_all(&marker_dir);
    }
}

/// Sweeps for a run that registers no target, which has never needed the
/// registry. It skips the sweep rather than wait for the lock or fail, as in a
/// nested sandbox that sees the runtime directory read-only.
pub(super) fn sweep_unregistered() {
    let Ok(runtime) = runtime_directory() else {
        return;
    };
    let registry = runtime.join(REGISTRY);
    match fs::symlink_metadata(&registry) {
        Ok(metadata) if metadata.is_dir() => {}
        _ => return,
    }
    let Ok(lock) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(registry.join("lock"))
    else {
        return;
    };
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        sweep(&registry);
    }
}

/// Whether the existing empty `target` is a mountpoint some run created: a
/// live registrant calls it synthetic, it carries the brand, or a marker in
/// `marker_dir` records it. A target swept since bubblewrap arguments were
/// built is absent, and this run creates it.
pub(super) fn vouches(target: &SyntheticMountTarget, marker_dir: &Path) -> bool {
    let markers = census(marker_dir).unwrap_or_default();
    if markers
        .iter()
        .any(|marker| marker.stake == Some(Stake::Synthetic) && marker.is_live())
    {
        return true;
    }
    let Some(site) = Site::resolve(target.path()) else {
        return false;
    };
    match site.status() {
        Err(Errno::NOENT) => true,
        Err(_) => false,
        Ok(status) => {
            is_empty_mountpoint(target.kind(), &status)
                && (site.is_branded(&status)
                    || markers
                        .iter()
                        .filter_map(Marker::synthetic)
                        .any(|mountpoint| {
                            mountpoint.path == site.path
                                && mountpoint.kind == target.kind()
                                && mountpoint.matches(&status)
                        }))
        }
    }
}

/// Creates a synthetic target's missing mountpoint, branded, so bubblewrap
/// mounts over it instead of creating it; returns the mountpoint now at the
/// path, if it is one. A pre-existing target is not touched.
pub(super) fn forge(target: &SyntheticMountTarget) -> Option<Mountpoint> {
    if target.preserves_pre_existing_path() {
        return None;
    }
    let site = Site::resolve(target.path())?;
    site.forge(target.kind());
    let status = site.status().ok()?;
    is_empty_mountpoint(target.kind(), &status).then(|| Mountpoint {
        kind: target.kind(),
        identity: status.identity,
        path: site.path,
    })
}

/// Registers the bubblewrap child beside this wrapper, so the hold outlives a
/// SIGKILLed wrapper for as long as the sandbox does.
pub(super) fn enlist(registrations: &[SyntheticMountTargetRegistration], child: libc::pid_t) {
    if registrations.is_empty() {
        return;
    }
    let incarnation = Incarnation::of(&child.to_string());
    super::with_synthetic_mount_registry_lock(|| {
        for registration in registrations {
            let own = Marker::read(0, &registration.marker_file);
            let stake_line = synthetic_mount_marker_contents(&registration.target);
            let path = registration.marker_dir.join(child.to_string());
            fs::write(
                &path,
                encode(stake_line, incarnation, own.mountpoint.as_ref()),
            )
            .unwrap_or_else(|err| {
                panic!(
                    "failed to register bubblewrap child for {}: {err}",
                    registration.target.path().display()
                )
            });
        }
    });
}

/// Whether `path` carries the brand. Classifies bubblewrap arguments only;
/// claims and removals check identity on descriptors.
pub(crate) fn is_branded_path(path: &Path) -> bool {
    rustix::fs::lgetxattr(path, BRAND, &mut [0_u8; 0]).is_ok()
}

fn is_empty_mountpoint(kind: SyntheticMountTargetKind, status: &Status) -> bool {
    match kind {
        SyntheticMountTargetKind::EmptyDirectory => status.file_type == FileType::Directory,
        SyntheticMountTargetKind::EmptyFile => {
            status.file_type == FileType::RegularFile && status.size == 0
        }
    }
}

/// Removes `mountpoint` if it still stands as recorded, reached without
/// following any symlink.
fn remove(mountpoint: &Mountpoint) {
    let Some(site) = Site::open(&mountpoint.path) else {
        return;
    };
    let flags = match mountpoint.kind {
        SyntheticMountTargetKind::EmptyDirectory => AtFlags::REMOVEDIR,
        SyntheticMountTargetKind::EmptyFile => AtFlags::empty(),
    };
    // rmdir refuses a directory filled since the check.
    if site
        .status()
        .is_ok_and(|status| mountpoint.matches(&status))
    {
        let _ = rustix::fs::unlinkat(&site.parent, &site.name, flags);
    }
}

/// A path pinned by a descriptor of its parent, which was reached without
/// following any symlink.
struct Site {
    path: PathBuf,
    parent: OwnedFd,
    name: OsString,
}

impl Site {
    fn open(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_os_string();
        let parent = rustix::fs::openat2(
            CWD,
            path.parent()?,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .ok()?;
        Some(Self {
            path: path.to_path_buf(),
            parent,
            name,
        })
    }

    /// The site at the real path of `path`'s parent, as bubblewrap resolves
    /// it. bwrap.rs refuses symlinks under writable roots, so this follows only
    /// symlinks that sandboxes cannot plant, such as `/home -> /var/home`.
    fn resolve(path: &Path) -> Option<Self> {
        Self::open(
            &fs::canonicalize(path.parent()?)
                .ok()?
                .join(path.file_name()?),
        )
    }

    fn status(&self) -> rustix::io::Result<Status> {
        Status::of(&self.parent, self.name.as_os_str(), AtFlags::empty())
    }

    /// Whether the entry described by `status`, an empty mountpoint, still
    /// stands and carries the brand.
    fn is_branded(&self, status: &Status) -> bool {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let Ok(entry) = rustix::fs::openat(&self.parent, &self.name, flags, Mode::empty()) else {
            return false;
        };
        Status::of(&entry, "", AtFlags::EMPTY_PATH)
            .is_ok_and(|opened| opened.identity.is(&status.identity))
            && rustix::fs::fgetxattr(&entry, BRAND, &mut [0_u8; 0]).is_ok()
    }

    /// Creates the mountpoint that bubblewrap would otherwise create, branded.
    /// On failure bubblewrap creates it, unbranded.
    fn forge(&self, kind: SyntheticMountTargetKind) {
        let created = match kind {
            SyntheticMountTargetKind::EmptyDirectory => {
                rustix::fs::mkdirat(&self.parent, &self.name, Mode::from_raw_mode(0o755)).and_then(
                    |()| {
                        rustix::fs::openat(
                            &self.parent,
                            &self.name,
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                    },
                )
            }
            SyntheticMountTargetKind::EmptyFile => rustix::fs::openat(
                &self.parent,
                &self.name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o644),
            ),
        };
        if let Ok(entry) = created {
            let _ = rustix::fs::fsetxattr(&entry, BRAND, &[], XattrFlags::empty());
        }
    }
}
