//! Observes logical Codex credential operations without inspecting authentication material.

use super::AuthDocument;
use super::AuthDotJson;
use super::AuthStorageBackend;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_otel::auth_storage::AuthStorageOriginator;
use codex_otel::auth_storage::CredentialKind;
use codex_otel::auth_storage::Operation;
use codex_otel::auth_storage::StorageTelemetry;
use codex_otel::auth_storage::Store;
use codex_otel::auth_storage::StoreMode;
use std::path::Path;
use std::sync::Arc;

pub(super) fn keyring_store(kind: AuthKeyringBackendKind) -> Store {
    match kind {
        AuthKeyringBackendKind::Direct => Store::DirectKeyring,
        AuthKeyringBackendKind::Secrets => Store::Secrets,
    }
}

pub(super) fn telemetry(
    mode: AuthCredentialsStoreMode,
    kind: AuthKeyringBackendKind,
    operation: Operation,
) -> StorageTelemetry {
    let mode = match mode {
        AuthCredentialsStoreMode::File => StoreMode::File,
        AuthCredentialsStoreMode::Auto => StoreMode::Auto,
        AuthCredentialsStoreMode::Keyring => StoreMode::Keyring,
        AuthCredentialsStoreMode::Ephemeral => StoreMode::Ephemeral,
    };
    StorageTelemetry::new(
        CredentialKind::Codex,
        mode,
        keyring_store(kind),
        operation,
        AuthStorageOriginator::current(),
    )
}

pub(super) fn observe(
    inner: Arc<dyn AuthStorageBackend>,
    mode: AuthCredentialsStoreMode,
    kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    // Auto records both attempts together so file fallback cannot hide a secure-store failure.
    if mode == AuthCredentialsStoreMode::Auto {
        return inner;
    }
    Arc::new(ObservedStorage { inner, mode, kind })
}

#[derive(Debug)]
struct ObservedStorage {
    inner: Arc<dyn AuthStorageBackend>,
    mode: AuthCredentialsStoreMode,
    kind: AuthKeyringBackendKind,
}

impl ObservedStorage {
    fn store(&self) -> Store {
        match self.mode {
            AuthCredentialsStoreMode::File => Store::File,
            AuthCredentialsStoreMode::Ephemeral => Store::Ephemeral,
            AuthCredentialsStoreMode::Keyring | AuthCredentialsStoreMode::Auto => {
                keyring_store(self.kind)
            }
        }
    }
}

impl AuthStorageBackend for ObservedStorage {
    fn is_ephemeral(&self) -> bool {
        self.inner.is_ephemeral()
    }

    fn home(&self) -> &Path {
        self.inner.home()
    }

    fn read(&self) -> std::io::Result<Option<AuthDocument>> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Load);
        let result = self.inner.read();
        telemetry.record_load_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    fn write(&self, auth: &AuthDocument) -> std::io::Result<()> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = self.inner.write(auth);
        telemetry.record_save_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        // TELOS(credential-backend-authority): Keep the backend's storage effects, not trait defaults.
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = self.inner.save(auth);
        telemetry.record_save_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    fn compare_exchange(
        &self,
        previous: &AuthDotJson,
        updated: &AuthDotJson,
    ) -> std::io::Result<bool> {
        // TELOS(credential-backend-authority): Keep the backend's atomic transaction, not default read/write.
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = self.inner.compare_exchange(previous, updated);
        if !matches!(result, Ok(false)) {
            telemetry.record_save_attempt(self.store(), &result.as_ref().map(|_| ()));
        }
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    fn delete_unlocked(&self) -> std::io::Result<bool> {
        let store = match self.mode {
            AuthCredentialsStoreMode::File | AuthCredentialsStoreMode::Ephemeral => self.store(),
            AuthCredentialsStoreMode::Auto | AuthCredentialsStoreMode::Keyring => Store::Multiple,
        };
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Delete);
        let result = self.inner.delete_unlocked();
        telemetry.record_delete_attempt(store, &result);
        result
    }
}
