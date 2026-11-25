//! TELOS(recoverable-output): budget sections once; retain only omitted emitted text.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use codex_code_mode::FunctionCallOutputContentItem;
use codex_protocol::protocol::TruncationPolicy;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use tempfile::TempDir;

use crate::unified_exec::resolve_max_tokens;

const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 32;
const DIRECTORY_PREFIX: &str = "code-mode-output-";
const LEASE_MARKER: &[u8] = b"codex-code-mode-output-v1\n";

#[derive(Default)]
pub(super) struct OutputRetention(Mutex<RetentionState>);

#[derive(Default)]
enum RetentionState {
    #[default]
    Dormant,
    Active(RetainedOutput),
    Closed,
}

struct RetainedOutput {
    // The directory disappears while its lease is still held.
    directory: TempDir,
    _lease: File,
    artifacts: VecDeque<(PathBuf, usize)>,
}

fn reap(directory: &std::path::Path) -> io::Result<()> {
    use std::fs::OpenOptions;
    use std::fs::TryLockError;
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;

    // SAFETY: geteuid has no arguments, pointer access, or caller preconditions.
    let uid = unsafe { libc::geteuid() };
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Ok(());
    }
    let mut lease = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("lease"))?;
    let metadata = lease.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() != LEASE_MARKER.len() as u64
    {
        return Ok(());
    }
    match lease.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    let mut marker = [0; LEASE_MARKER.len()];
    lease.read_exact(&mut marker)?;
    if marker != LEASE_MARKER {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !(name == "lease" || (name.starts_with("sections-") && name.ends_with(".txt")))
            || !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o600
        {
            return Ok(());
        }
    }
    std::fs::remove_dir_all(directory)
}

impl OutputRetention {
    #[cfg(test)]
    pub(super) fn in_test_runtime(runtime: PathBuf) -> io::Result<Self> {
        Ok(Self(Mutex::new(RetentionState::Active(
            RetainedOutput::new_in(runtime)?,
        ))))
    }

    pub(super) fn clear(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = RetentionState::Closed;
    }

    fn retain(&self, sections: &[(usize, &str, usize)]) -> io::Result<PathBuf> {
        let bytes = sections.iter().try_fold(0usize, |total, (_, text, _)| {
            total.checked_add(text.len().saturating_add(128))
        });
        let Some(bytes) = bytes.filter(|bytes| *bytes <= MAX_ARTIFACT_BYTES) else {
            return Err(io::Error::other(
                "truncated text exceeds the 16 MiB recovery artifact limit",
            ));
        };
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, RetentionState::Dormant) {
            *state = RetentionState::Active(RetainedOutput::new()?);
        }
        match &mut *state {
            RetentionState::Active(output) => output.write(sections, bytes),
            RetentionState::Closed => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "thread output retention is closed",
            )),
            RetentionState::Dormant => unreachable!("retention was initialized"),
        }
    }
}

impl RetainedOutput {
    fn new() -> io::Result<Self> {
        Self::new_in(codex_uds::prepare_user_runtime_directory()?)
    }

    fn new_in(runtime: PathBuf) -> io::Result<Self> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        for entry in std::fs::read_dir(&runtime)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(DIRECTORY_PREFIX)
            {
                match reap(&entry.path()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let directory = tempfile::Builder::new()
            .prefix(DIRECTORY_PREFIX)
            .tempdir_in(runtime)?;
        let mut lease = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path().join("lease"))?;
        lease.lock()?;
        // The marker publishes custody only while the lease is held. A concurrent
        // reaper that locks an empty lease must leave the constructor alone.
        lease.write_all(LEASE_MARKER)?;
        Ok(Self {
            directory,
            _lease: lease,
            artifacts: VecDeque::new(),
        })
    }

    fn write(&mut self, sections: &[(usize, &str, usize)], bytes: usize) -> io::Result<PathBuf> {
        while self.artifacts.len() >= MAX_ARTIFACTS
            || self
                .artifacts
                .iter()
                .map(|(_, size)| size)
                .sum::<usize>()
                .saturating_add(bytes)
                > MAX_RETAINED_BYTES
        {
            let Some((path, _)) = self.artifacts.front() else {
                break;
            };
            std::fs::remove_file(path)?;
            self.artifacts.pop_front();
        }
        let mut artifact = tempfile::Builder::new()
            .prefix("sections-")
            .suffix(".txt")
            .tempfile_in(self.directory.path())?;
        for (index, text, limit) in sections {
            writeln!(
                artifact,
                "Section {index}: {} original UTF-8 bytes; effective text limit {limit} bytes",
                text.len()
            )?;
            artifact.write_all(text.as_bytes())?;
            writeln!(artifact, "\n")?;
        }
        let size =
            usize::try_from(artifact.as_file().metadata()?.len()).map_err(io::Error::other)?;
        // Ownership transfers from the temporary file to this bounded FIFO.
        let (_, path) = artifact.keep().map_err(|error| error.error)?;
        self.artifacts.push_back((path.clone(), size));
        Ok(path)
    }
}

fn fair_allocate(limits: &mut [usize], mut remaining: usize) {
    let mut order: Vec<_> = (0..limits.len()).collect();
    order.sort_unstable_by_key(|index| limits[*index]);
    let count = order.len();
    for (position, index) in order.into_iter().enumerate() {
        limits[index] = limits[index].min(remaining / (count - position));
        remaining -= limits[index];
    }
}

pub(super) fn truncate_sections(
    emitted: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
    model_policy: TruncationPolicy,
    retention: &OutputRetention,
) -> Vec<FunctionCallOutputContentItem> {
    let max_tokens = resolve_max_tokens(max_output_tokens);
    let requested_bytes = approx_bytes_for_tokens(max_tokens);
    let budget = requested_bytes.min(model_policy.byte_budget());
    let reduction = if max_output_tokens.is_some() && budget < requested_bytes {
        format!(
            "Output cap is {budget} bytes, about {} tokens, not the {max_tokens} tokens requested.\n",
            budget / approx_bytes_for_tokens(1)
        )
    } else {
        String::new()
    };
    let mut items = Vec::new();
    let mut limits = Vec::new();
    for item in emitted {
        let (sections, cap) = match item {
            FunctionCallOutputContentItem::InputText { text, max_tokens } => {
                (vec![text], max_tokens)
            }
            FunctionCallOutputContentItem::InputTextBatch {
                sections,
                max_tokens,
            } => (sections, max_tokens),
            media => {
                items.push(media);
                continue;
            }
        };
        let sections: Vec<_> = sections
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect();
        let start = limits.len();
        limits.extend(sections.iter().map(String::len));
        if let Some(cap) = cap {
            fair_allocate(
                &mut limits[start..],
                approx_bytes_for_tokens(usize::try_from(cap).unwrap_or(usize::MAX)),
            );
        }
        items.extend(
            sections
                .into_iter()
                .map(|text| FunctionCallOutputContentItem::InputText {
                    text,
                    max_tokens: None,
                }),
        );
    }
    let requested = limits.clone();
    fair_allocate(&mut limits, budget);
    let sections: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            FunctionCallOutputContentItem::InputText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .enumerate()
        .filter_map(|(index, text)| {
            (text.len() > limits[index]).then_some((index + 1, text, limits[index]))
        })
        .collect();
    if sections.is_empty() {
        if !reduction.is_empty() {
            items.insert(
                0,
                FunctionCallOutputContentItem::InputText {
                    text: reduction,
                    max_tokens: None,
                },
            );
        }
        return items;
    }
    let retained = retention.retain(&sections);
    let recovery = match &retained {
        Ok(path) => format!("Full text: {}", path.display()),
        Err(error) => format!("Full text was not saved: {error}."),
    };
    let mut notice = format!(
        "{reduction}Truncated {} of {} sections to fit {budget} bytes. {recovery}\n",
        sections.len(),
        limits.len()
    );
    for (index, text, limit) in sections.iter().take(16) {
        let demand = requested[index - 1];
        let _ = write!(notice, "Section {index}: {} → {limit} bytes", text.len());
        if demand < text.len() {
            let _ = write!(notice, ", emission cap {demand}");
        }
        notice.push('\n');
    }
    if sections.len() > 16 {
        notice.push_str(if retained.is_ok() {
            "The saved text gives the limits of the other sections.\n"
        } else {
            "The limits of the other sections are not listed.\n"
        });
    }
    drop(sections);
    let mut index = 0;
    for item in &mut items {
        if let FunctionCallOutputContentItem::InputText { text, max_tokens } = item {
            let limit = limits[index];
            index += 1;
            *max_tokens = None;
            if text.len() > limit {
                if limit < '…'.len_utf8() {
                    *text = "…".to_string();
                    continue;
                }
                let payload = limit - '…'.len_utf8();
                let mut head = payload / 2;
                while !text.is_char_boundary(head) {
                    head -= 1;
                }
                let mut tail = text.len() - (payload - head);
                while !text.is_char_boundary(tail) {
                    tail += 1;
                }
                *text = format!("{}…{}", &text[..head], &text[tail..]);
            }
        }
    }
    items.insert(
        0,
        FunctionCallOutputContentItem::InputText {
            text: notice,
            max_tokens: None,
        },
    );
    items
}
