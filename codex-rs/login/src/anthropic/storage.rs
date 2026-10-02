use std::fs::File;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

use super::AccountId;
use super::AccountInfo;
use super::SubscriptionGrant;
use super::token::GrantWire;

const MAX_ACCOUNT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AccountStore {
    directory: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub info: AccountInfo,
    pub grant: SubscriptionGrant,
}

#[derive(Serialize, Deserialize)]
struct RecordWire {
    info: AccountInfo,
    grant: GrantWire,
}

impl AccountStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self {
            directory: codex_home.join("anthropic").join("accounts"),
        }
    }

    pub fn list(&self) -> io::Result<Vec<AccountInfo>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut accounts = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let id: AccountId = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| io::Error::other("invalid Anthropic account filename"))?
                    .parse()?;
                accounts.push(self.load(&id)?.info);
            }
        }
        accounts.sort_by_key(|info| info.identity.id());
        Ok(accounts)
    }

    /// Completes one login atomically without replacing another identity.
    pub async fn upsert(
        &self,
        info: AccountInfo,
        grant: SubscriptionGrant,
    ) -> io::Result<AccountId> {
        let id = info.identity.id();
        let lock = self.lock(&id).await?;
        let store = self.clone();
        let result = id.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            store.save(&id, &Record { info, grant })
        })
        .await
        .map_err(|_| io::Error::other("Anthropic account save task failed"))??;
        Ok(result)
    }

    pub(super) async fn lock(&self, id: &AccountId) -> io::Result<File> {
        std::fs::create_dir_all(&self.directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.directory.join(format!("{id}.lock")))?;
        tokio::time::timeout(Duration::from_secs(/*secs*/ 60), async {
            loop {
                match file.try_lock() {
                    Ok(()) => return Ok(()),
                    Err(TryLockError::WouldBlock) => {
                        tokio::time::sleep(Duration::from_millis(/*millis*/ 25)).await
                    }
                    Err(error) => return Err(io::Error::from(error)),
                }
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, "Anthropic account lock timed out")
        })??;
        Ok(file)
    }

    pub(super) fn load(&self, id: &AccountId) -> io::Result<Record> {
        let mut bytes = Vec::new();
        File::open(self.directory.join(format!("{id}.json")))?
            .take((MAX_ACCOUNT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ACCOUNT_BYTES {
            return Err(io::Error::other("stored Anthropic account is too large"));
        }
        let wire: RecordWire = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("invalid stored Anthropic account"))?;
        if wire.info.identity.id() != *id {
            return Err(io::Error::other(
                "stored Anthropic identity does not match its account ID",
            ));
        }
        Ok(Record {
            info: wire.info,
            grant: wire.grant.try_into()?,
        })
    }

    pub(super) fn save(&self, id: &AccountId, record: &Record) -> io::Result<()> {
        let wire = RecordWire {
            info: record.info.clone(),
            grant: record.grant.clone().into(),
        };
        let bytes = serde_json::to_vec(&wire)
            .map_err(|_| io::Error::other("failed to encode Anthropic account"))?;
        if bytes.len() > MAX_ACCOUNT_BYTES {
            return Err(io::Error::other(
                "Anthropic account is too large to persist",
            ));
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(format!("{id}.json")))
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}
