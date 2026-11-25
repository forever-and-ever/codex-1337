use super::Journal;
use super::Lease;
use super::OpenRouterBatchRecovery;
use super::Phase;
use super::fatal;
use super::load;
use super::turn_directory;
use crate::config::Config;
use codex_model_provider::openrouter::Client;
use codex_model_provider::openrouter::credentials;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_protocol::ThreadId;
use codex_protocol::error::Result;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

fn directories(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// The provider does not quote a maximum invoice on acceptance. Until it
/// settles, the uncommitted balance of this credential cannot be inferred from
/// its last balance reading. A custom ID alone cannot prove rejection either.
pub fn outstanding_openrouter_batches(
    home: &Path,
    base_url: &str,
    fingerprint: &str,
) -> Result<u32> {
    #[derive(serde::Deserialize)]
    struct Liability {
        base_url: String,
        fingerprint: String,
        phase: Phase,
    }
    let root = home.join("openrouter-batches");
    if !root.try_exists().map_err(fatal)? {
        return Ok(0);
    }
    let mut count = 0u32;
    for owner in directories(&root).map_err(fatal)? {
        for turn in directories(&owner).map_err(fatal)? {
            for filename in ["generation.json", "compaction.json"] {
                let file = match fs::File::open(turn.join(filename)) {
                    Ok(file) => file,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(fatal(e)),
                };
                let row: Liability = serde_json::from_reader(file).map_err(fatal)?;
                if row.base_url == base_url
                    && row.fingerprint == fingerprint
                    && matches!(row.phase, Phase::Submitting | Phase::Submitted(_))
                {
                    count = count.saturating_add(1);
                }
            }
        }
    }
    Ok(count)
}

/// Reconstitute the managed daemon handoff even after a process death with no snapshot.
/// Detached samples and abandoned jobs are collectors only; no recovery path submits.
pub async fn recover_openrouter_batches(
    config: &Config,
) -> Result<Vec<(ThreadId, String, OpenRouterBatchRecovery)>> {
    let home = &config.codex_home;
    let root = home.join("openrouter-batches");
    if !root.try_exists().map_err(fatal)? {
        return Ok(Vec::new());
    }
    let base_url = config
        .model_providers
        .get(OPENROUTER_PROVIDER_ID)
        .and_then(|provider| provider.base_url.as_deref())
        .unwrap_or("https://openrouter.ai/api/v1");
    let client = Client::new(base_url, config.http_client_factory()).map_err(fatal)?;
    let mut recover = Vec::new();
    for owner in directories(&root).map_err(fatal)? {
        for dir in directories(&owner).map_err(fatal)? {
            for kind in ["generation.json", "compaction.json"] {
                let path = dir.join(kind);
                let journal = match load(&path) {
                    Ok(Some(j)) => j,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(%e,"unreadable paid-step journal");
                        continue;
                    }
                };
                if turn_directory(home, journal.thread, &journal.turn) != dir
                    || journal.base_url != base_url
                {
                    continue;
                }
                let terminal = matches!(journal.phase, Phase::Consumed | Phase::Rejected(_));
                let detached = journal.sample_for.is_some()
                    || dir.join("abandoned").try_exists().map_err(fatal)?;
                if !journal.cleanup.is_empty()
                    && (terminal || detached && !matches!(journal.phase, Phase::Submitted(_)))
                {
                    let home = home.to_owned();
                    let path = path.clone();
                    let client = client.clone();
                    let base_url = base_url.to_owned();
                    tokio::spawn(async move {
                        if let Err(error) = sweep(&home, &path, &base_url, &client).await {
                            tracing::warn!(%error,"OpenRouter remote cleanup deferred");
                        }
                    });
                }
                if terminal {
                    continue;
                }
                if detached {
                    if matches!(journal.phase, Phase::Submitted(_)) {
                        collect_detached(home.to_path_buf(), path, client.clone(), journal);
                    }
                } else {
                    recover.push((journal.thread, journal.turn, journal.recovery));
                }
            }
        }
    }
    Ok(recover)
}

pub(super) fn collect_detached(home: PathBuf, path: PathBuf, client: Client, prior: Journal) {
    tokio::spawn(async move {
        if let Err(error) = collect(&home, &path, client, prior).await {
            tracing::warn!(%error,"unsettled detached OpenRouter batch");
        }
    });
}

async fn collect(home: &Path, path: &Path, client: Client, prior: Journal) -> Result<()> {
    let key = credentials(home, &prior.key).await.map_err(fatal)?;
    if key.batch_fingerprint() != prior.fingerprint {
        return Err(fatal("detached payer changed"));
    }
    let lock = Lease::lock(path)?;
    let Some(journal) = load(path)? else {
        return Ok(());
    };
    if journal.custom_id != prior.custom_id {
        return Err(fatal("detached job identity changed"));
    }
    let Phase::Submitted(ref job) = journal.phase else {
        return Ok(());
    };
    let job = job.clone();
    Lease {
        _lock: lock,
        path: path.to_owned(),
        journal,
    }
    .poll(&client, &key, &job)
    .await
}

async fn sweep(home: &Path, path: &Path, base_url: &str, client: &Client) -> Result<()> {
    // Never contend with a resumed step. Its atomic journal rewrite preserves
    // the obligation, so a later sweep can retry without losing the invoice.
    let lock = Lease::lock(path)?;
    let Some(journal) = load(path)? else {
        return Ok(());
    };
    let mut lease = Lease {
        _lock: lock,
        path: path.to_owned(),
        journal,
    };
    for artifact in lease.journal.cleanup.clone() {
        if artifact.base_url != base_url {
            continue;
        }
        let bearer = credentials(home, &artifact.payer).await.map_err(fatal)?;
        if bearer.batch_fingerprint() == artifact.fingerprint {
            lease.cleanup(client, &bearer, &artifact.job).await?;
        }
    }
    Ok(())
}
