use super::*;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEADLINE: Duration = Duration::from_millis(200);
const METADATA: usize = 512;
const WORDS: usize = 11 + 8 + 6 + 8 + STAGES * (STAT_WORDS + 1) + SLOW * 6;
const WIRE: usize = METADATA + WORDS * 8;
const MAGIC: u64 = 0x55494c4154454e01;

#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub start_ticks: u64,
    pub reload_generation: u64,
    pub build: String,
    pub channel: String,
    pub terminal_device: u64,
}
#[derive(Debug, Serialize)]
pub struct Worker {
    pub identity: Identity,
    pub endpoint: PathBuf,
}
#[derive(Debug, Serialize)]
pub struct Discovery {
    pub schema_version: u64,
    pub workers: Vec<Worker>,
    pub unresponsive_pids: Vec<u32>,
    pub partial: bool,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u64,
    pub identity: Identity,
    pub observed_ns: u64,
    pub requested_epoch: u64,
    pub active_epoch: u64,
    pub reset_pending: bool,
    pub recorder_bytes: u64,
    pub timing: &'static str,
    pub histogram_upper_ns: [u64; BUCKETS],
    pub last_input_consumed_ns: u64,
    // Atomic records are independently consistent, NOT a simultaneous cut.
    pub incomplete_records: usize,
    pub suppressed_scopes: u64,
    pub current: Option<Current>,
    pub last_committed: Option<Committed>,
    pub input: Option<Admission>,
    pub stages: Vec<Statistics>,
    pub recent_slow: Vec<Slow>,
}
#[derive(Debug, Serialize)]
pub struct Current {
    pub epoch: u64,
    pub published_ns: u64,
    pub stage: Option<&'static str>,
    pub started_ns: u64,
    pub parent: Option<&'static str>,
    pub depth: u64,
    pub entry_epoch: u64,
}
#[derive(Debug, Serialize)]
pub struct Committed {
    pub epoch: u64,
    pub completed_ns: u64,
    pub stage: Option<&'static str>,
    pub duration_ns: u64,
    pub completed_spans: u64,
}
#[derive(Debug, Serialize)]
pub struct Admission {
    pub epoch: u64,
    pub published_ns: u64,
    pub gate: &'static str,
    pub active_thread_backlog: u64,
    pub primary_backlog: u64,
    pub app_event_backlog: u64,
    pub gated_since_ns: u64,
}
#[derive(Debug, Serialize)]
pub struct Statistics {
    pub stage: &'static str,
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub histogram: [u64; BUCKETS],
}
#[derive(Debug, Serialize)]
pub struct Slow {
    pub sequence: u64,
    pub stage: Option<&'static str>,
    pub started_ns: u64,
    pub duration_ns: u64,
}

fn start_ticks(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, tail)| tail.split_whitespace().nth(19))
        .and_then(|ticks| ticks.parse().ok())
        .ok_or_else(|| io::Error::other("invalid process start identity"))
}
fn peer(fd: i32) -> io::Result<libc::ucred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most the supplied size into a live ucred.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no pointer or lifetime preconditions.
    if size as usize != size_of::<libc::ucred>() || cred.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "UI diagnostic peer is not this user",
        ));
    }
    Ok(cred)
}
fn runtime() -> io::Result<PathBuf> {
    codex_uds::prepare_user_runtime_directory()
}

pub(crate) struct Endpoint {
    wake: UnixStream,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
    inode: u64,
}
impl Endpoint {
    pub(super) fn start(shared: Arc<Shared>) -> io::Result<Self> {
        let pid = std::process::id();
        let start = start_ticks(pid)?;
        let path = runtime()?.join(format!("ui-latency-{pid}-{start}.sock"));
        let (wake, receiver) = UnixStream::pair()?;
        let listener = UnixListener::bind(&path)?;
        // Custody begins immediately, including failures during initialization.
        let inode = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata.ino(),
            Err(error) => {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
        };
        let mut owner = Self {
            wake,
            thread: None,
            path,
            inode,
        };
        fs::set_permissions(&owner.path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        // SAFETY: listener is a live listening socket; backlog is deliberately small.
        if unsafe { libc::listen(listener.as_raw_fd(), 4) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let info = codex_build_info::BuildInfo::get();
        let identity = Identity {
            pid,
            start_ticks: start,
            reload_generation: std::env::var("CODEX_UI_GENERATION")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            build: info.build_commit().to_owned(),
            channel: match codex_install_context::Channel::current() {
                codex_install_context::Channel::Production => "production",
                codex_install_context::Channel::Staging => "staging",
            }
            .into(),
            terminal_device: fs::metadata("/proc/self/fd/1").map_or(0, |metadata| metadata.rdev()),
        };
        let encoded = serde_json::to_vec(&identity)?;
        if encoded.len() >= METADATA {
            return Err(io::Error::other(
                "UI diagnostic identity exceeds protocol bound",
            ));
        }
        let mut metadata = [0; METADATA];
        metadata[..encoded.len()].copy_from_slice(&encoded);
        owner.thread = Some(
            thread::Builder::new()
                .name("ui-latency".into())
                .stack_size(128 * 1024)
                .spawn(move || listen(listener, receiver, shared, metadata))?,
        );
        Ok(owner)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        // The wake descriptor is independent of accept, UI locks, and Tokio.
        let _ = self.wake.write_all(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn listen(listener: UnixListener, wake: UnixStream, shared: Arc<Shared>, metadata: [u8; METADATA]) {
    let mut fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: fds is a live fixed pollfd array. No timer or periodic export.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            return;
        }
        if let Ok((mut client, _)) = listener.accept() {
            // Exactly one client, one request, one fixed response; no client threads.
            let _ = serve(&mut client, &shared, &metadata);
        }
    }
}
fn read_request(client: &mut UnixStream, deadline: Instant) -> io::Result<[u8; 8]> {
    let mut request = [0; 8];
    let mut filled = 0;
    while filled < request.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        client.set_read_timeout(Some(remaining))?;
        let count = client.read(&mut request[filled..])?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        filled += count;
    }
    Ok(request)
}
fn serve(client: &mut UnixStream, shared: &Shared, metadata: &[u8; METADATA]) -> io::Result<()> {
    peer(client.as_raw_fd())?;
    let deadline = Instant::now() + DEADLINE;
    match &read_request(client, deadline)? {
        b"UIR1DUMP" => {}
        b"UIR1RSET" => {
            // Only this listener mutates requested_epoch, never the UI writer.
            let epoch = shared
                .requested_epoch
                .load(Ordering::Relaxed)
                .saturating_add(1);
            shared.requested_epoch.store(epoch, Ordering::Relaxed);
        }
        _ => return Err(io::ErrorKind::InvalidData.into()),
    }
    let words = snapshot(shared);
    let mut response = [0; WIRE];
    response[..METADATA].copy_from_slice(metadata);
    for (word, bytes) in words
        .into_iter()
        .zip(response[METADATA..].chunks_exact_mut(8))
    {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    let mut written = 0;
    while written < response.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        client.set_write_timeout(Some(remaining))?;
        let count = client.write(&response[written..])?;
        if count == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        written += count;
    }
    Ok(())
}
fn snapshot(shared: &Shared) -> [u64; WORDS] {
    let mut words = [0; WORDS];
    words[..11].copy_from_slice(&[
        MAGIC,
        shared.now(),
        shared.requested_epoch.load(Ordering::Relaxed),
        shared.active_epoch.load(Ordering::Acquire),
        size_of::<Shared>() as u64 + size_of::<Writer>() as u64,
        BUCKETS as u64,
        STAGES as u64,
        SLOW as u64,
        SLOW_NS,
        shared.last_input_ns.load(Ordering::Relaxed),
        shared.suppressed_scopes.load(Ordering::Relaxed),
    ]);
    let mut rest = &mut words[11..];
    fn append<const N: usize>(rest: &mut &mut [u64], record: &Record<N>) {
        let (slot, tail) = std::mem::take(rest).split_at_mut(N + 1);
        if let Some(values) = record.read() {
            slot[0] = 1;
            slot[1..].copy_from_slice(&values);
        }
        *rest = tail;
    }
    append(&mut rest, &shared.current);
    append(&mut rest, &shared.committed);
    append(&mut rest, &shared.admission);
    for record in &shared.stats {
        append(&mut rest, record);
    }
    for record in &shared.slow {
        append(&mut rest, record);
    }
    words[1] = shared.now();
    words
}

pub async fn discover() -> io::Result<Discovery> {
    let mut result = Discovery {
        schema_version: 1,
        workers: Vec::new(),
        unresponsive_pids: Vec::new(),
        partial: false,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    // No central registry and no stale-endpoint sweep. Bound both directory
    // work and total network time, including living but unresponsive workers.
    for (seen, entry) in fs::read_dir(runtime()?)?.enumerate() {
        if seen == 1024
            || result.workers.len() + result.unresponsive_pids.len() == 64
            || tokio::time::Instant::now() >= deadline
        {
            result.partial = true;
            break;
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name
            .to_str()
            .and_then(|name| name.strip_prefix("ui-latency-"))
            .and_then(|name| name.strip_suffix(".sock"))
        else {
            continue;
        };
        let Some((pid, start)) = name.split_once('-') else {
            continue;
        };
        let (Ok(pid), Ok(start)) = (pid.parse::<u32>(), start.parse::<u64>()) else {
            continue;
        };
        if start_ticks(pid).ok() != Some(start) {
            continue;
        }
        match tokio::time::timeout_at(deadline, query_path(&entry.path(), pid, start, false)).await
        {
            Ok(Ok(report)) => result.workers.push(Worker {
                identity: report.identity,
                endpoint: entry.path(),
            }),
            Ok(Err(_)) => result.unresponsive_pids.push(pid),
            Err(_) => {
                result.unresponsive_pids.push(pid);
                result.partial = true;
                break;
            }
        }
    }
    result.workers.sort_by_key(|worker| worker.identity.pid);
    Ok(result)
}
pub async fn query(pid: u32, reset: bool) -> io::Result<Report> {
    let start = start_ticks(pid)?;
    let path = runtime()?.join(format!("ui-latency-{pid}-{start}.sock"));
    query_path(&path, pid, start, reset).await
}
async fn query_path(path: &Path, pid: u32, start: u64, reset: bool) -> io::Result<Report> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let response = tokio::time::timeout(Duration::from_millis(500), async {
        let mut client = tokio::net::UnixStream::connect(path).await?;
        if peer(client.as_raw_fd())?.pid != pid as i32 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        client
            .write_all(if reset { b"UIR1RSET" } else { b"UIR1DUMP" })
            .await?;
        let mut response = [0; WIRE];
        client.read_exact(&mut response).await?;
        Ok::<_, io::Error>(response)
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    let identity: Identity = serde_json::from_slice(
        response[..METADATA]
            .split(|b| *b == 0)
            .next()
            .unwrap_or_default(),
    )?;
    if identity.pid != pid || identity.start_ticks != start || start_ticks(pid)? != start {
        return Err(io::Error::other("UI worker identity changed"));
    }
    let mut words = [0; WORDS];
    for (word, bytes) in words.iter_mut().zip(response[METADATA..].chunks_exact(8)) {
        *word = u64::from_le_bytes(bytes.try_into().map_err(io::Error::other)?);
    }
    decode(identity, &words)
}
fn stage(value: u64) -> Option<&'static str> {
    value
        .checked_sub(1)
        .and_then(|i| STAGE_NAMES.get(i as usize))
        .copied()
}
fn decode(identity: Identity, words: &[u64; WORDS]) -> io::Result<Report> {
    if words[0] != MAGIC
        || words[5] != BUCKETS as u64
        || words[6] != STAGES as u64
        || words[7] != SLOW as u64
    {
        return Err(io::Error::other("unsupported UI diagnostic schema"));
    }
    let epoch = words[2];
    let mut incomplete = 0;
    let mut rest = &words[11..];
    fn take<'a>(rest: &mut &'a [u64], n: usize, incomplete: &mut usize) -> Option<&'a [u64]> {
        let (slot, tail) = rest.split_at(n + 1);
        *rest = tail;
        if slot[0] == 0 {
            *incomplete += 1;
            None
        } else {
            Some(&slot[1..])
        }
    }
    let current = take(&mut rest, 7, &mut incomplete).map(|w| Current {
        epoch: w[0],
        published_ns: w[1],
        stage: stage(w[2]),
        started_ns: w[3],
        parent: stage(w[4]),
        depth: w[5],
        entry_epoch: w[6],
    });
    // Preserve pre-reset committed/current evidence, explicitly epoch-labelled.
    let last_committed = take(&mut rest, 5, &mut incomplete).map(|w| Committed {
        epoch: w[0],
        completed_ns: w[1],
        stage: stage(w[2]),
        duration_ns: w[3],
        completed_spans: w[4],
    });
    let input = take(&mut rest, 7, &mut incomplete).map(|w| Admission {
        epoch: w[0],
        published_ns: w[1],
        gate: match w[2] {
            0 => "open",
            1 => "thread_switch",
            2 => "replay",
            3 => "startup",
            _ => "unknown",
        },
        app_event_backlog: w[3],
        gated_since_ns: w[4],
        active_thread_backlog: w[5],
        primary_backlog: w[6],
    });
    let mut stages = Vec::with_capacity(STAGES);
    for name in STAGE_NAMES {
        if let Some(w) = take(&mut rest, STAT_WORDS, &mut incomplete).filter(|w| w[0] == epoch) {
            stages.push(Statistics {
                stage: name,
                count: w[1],
                total_ns: w[2],
                max_ns: w[3],
                histogram: std::array::from_fn(|i| w[4 + i]),
            });
        }
    }
    let mut recent_slow = Vec::with_capacity(SLOW);
    for _ in 0..SLOW {
        if let Some(w) = take(&mut rest, 5, &mut incomplete).filter(|w| w[0] == epoch) {
            recent_slow.push(Slow {
                sequence: w[1],
                stage: stage(w[2]),
                started_ns: w[3],
                duration_ns: w[4],
            });
        }
    }
    recent_slow.sort_by_key(|span| span.sequence);
    Ok(Report {
        schema_version: 1,
        identity,
        observed_ns: words[1],
        requested_epoch: epoch,
        active_epoch: words[3],
        reset_pending: epoch != words[3],
        recorder_bytes: words[4],
        incomplete_records: incomplete,
        suppressed_scopes: words[10],
        current,
        last_committed,
        input,
        stages,
        recent_slow,
        timing: "wall_inclusive",
        histogram_upper_ns: BUCKET_UPPER_NS,
        last_input_consumed_ns: words[9],
    })
}
