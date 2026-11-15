//! Application destination checks and revocation of outstanding network operations.
//!
//! The configuration owner publishes an already-composed policy. Transports retain only its
//! read side and a permit for each request; invalidation cannot leave an old request authorized.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Weak;

use reqwest::Url;
use tokio::sync::watch;

/// Effective application destinations, after the requirements owner has applied precedence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestinationPolicy {
    Unrestricted,
    /// Only HTTPS and WSS requests to these exact, normalized hosts are permitted.
    Restricted {
        allowed_hosts: BTreeSet<String>,
    },
}

impl DestinationPolicy {
    fn allows(&self, url: &Url) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Restricted { allowed_hosts } => {
                matches!(url.scheme(), "https" | "wss")
                    && url.host_str().is_some_and(|host| {
                        allowed_hosts.contains(host.strip_suffix('.').unwrap_or(host))
                    })
            }
        }
    }
}

/// A deterministic denial, distinct from retryable network failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetworkPolicyDenied {
    #[error("application network policy is unavailable")]
    Unavailable,
    #[error("destination denied by application network policy")]
    Destination,
    #[error("application network permission was revoked")]
    Revoked,
    #[error("this SDK transport is disabled by application network restrictions")]
    UnsupportedTransport,
}

/// Read access to application policy. Clones observe the same updates.
#[derive(Clone, Default)]
pub struct NetworkPolicy {
    state: Option<Arc<Mutex<State>>>,
    endpoints: Option<Arc<BTreeSet<Url>>>,
    account: Option<u64>,
    account_scope: Option<Arc<Mutex<AccountScope>>>,
}

/// Retains policy identity without extending its application or account lifetime.
#[derive(Clone)]
pub struct WeakNetworkPolicy {
    state: Option<Weak<Mutex<State>>>,
    endpoints: Option<Arc<BTreeSet<Url>>>,
    account: Option<u64>,
    account_scope: Option<Weak<Mutex<AccountScope>>>,
}

impl WeakNetworkPolicy {
    pub fn upgrade(&self) -> Option<NetworkPolicy> {
        Some(NetworkPolicy {
            state: match &self.state {
                Some(state) => Some(state.upgrade()?),
                None => None,
            },
            endpoints: self.endpoints.clone(),
            account: self.account,
            account_scope: match &self.account_scope {
                Some(scope) => Some(scope.upgrade()?),
                None => None,
            },
        })
    }
}

impl fmt::Debug for NetworkPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkPolicy")
            .field("managed", &self.state.is_some())
            .finish_non_exhaustive()
    }
}

impl PartialEq for NetworkPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.endpoints == other.endpoints
            && self.account == other.account
            && match (&self.account_scope, &other.account_scope) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
            && match (&self.state, &other.state) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
    }
}

impl Eq for NetworkPolicy {}

struct State {
    policy: Option<DestinationPolicy>,
    revision: u64,
    account: u64,
    permits: Vec<Weak<Permit>>,
    changes: watch::Sender<()>,
}

/// Credential lifetime independent of an identity-neutral destination policy.
struct AccountScope {
    generation: u64,
    permits: Vec<Weak<Permit>>,
    changes: watch::Sender<()>,
}

struct Permit {
    destination: PermitDestination,
    revoked: watch::Sender<bool>,
}

enum PermitDestination {
    Url(Url),
    UnrestrictedSdk,
}

impl PermitDestination {
    fn allowed_by(&self, policy: &DestinationPolicy) -> bool {
        match self {
            Self::Url(url) => policy.allows(url),
            Self::UnrestrictedSdk => matches!(policy, DestinationPolicy::Unrestricted),
        }
    }
}

/// Publication access retained by the account/configuration owner.
#[derive(Clone)]
pub struct NetworkPolicyController {
    state: Arc<Mutex<State>>,
}

/// Identifies the account/configuration generation that a load is allowed to publish into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkPolicyRevision(u64);

impl Default for NetworkPolicyController {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                policy: None,
                revision: 0,
                account: 0,
                permits: Vec::new(),
                changes: watch::channel(()).0,
            })),
        }
    }
}

fn lock_state<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state
        .lock()
        .unwrap_or_else(|_| panic!("application network policy lock poisoned"))
}

impl NetworkPolicyController {
    pub fn policy(&self) -> NetworkPolicy {
        NetworkPolicy {
            state: Some(self.state.clone()),
            endpoints: None,
            account: None,
            account_scope: None,
        }
    }

    /// Fails a requirements load without changing the current account identity.
    pub fn unavailable(&self, revision: NetworkPolicyRevision) {
        let mut state = lock_state(&self.state);
        if state.revision != revision.0 {
            return;
        }
        if state.policy.take().is_some() {
            state.changes.send_replace(());
        }
        for permit in state
            .permits
            .drain(..)
            .filter_map(|permit| permit.upgrade())
        {
            permit.revoked.send_replace(/*value*/ true);
        }
    }

    /// Publishes a successful load only if its account/configuration generation is still current.
    pub fn publish(&self, revision: NetworkPolicyRevision, policy: DestinationPolicy) -> bool {
        let mut state = lock_state(&self.state);
        if state.revision != revision.0 {
            return false;
        }
        state.permits.retain(|permit| {
            let Some(permit) = permit.upgrade() else {
                return false;
            };
            if !permit.destination.allowed_by(&policy) {
                permit.revoked.send_replace(/*value*/ true);
            }
            true
        });
        if state.policy.as_ref() != Some(&policy) {
            state.policy = Some(policy);
            state.changes.send_replace(());
        }
        true
    }
}

impl NetworkPolicy {
    pub fn downgrade(&self) -> WeakNetworkPolicy {
        WeakNetworkPolicy {
            state: self.state.as_ref().map(Arc::downgrade),
            endpoints: self.endpoints.clone(),
            account: self.account,
            account_scope: self.account_scope.as_ref().map(Arc::downgrade),
        }
    }

    pub const fn unmanaged() -> Self {
        Self {
            state: None,
            endpoints: None,
            account: None,
            account_scope: None,
        }
    }

    /// Give one account its own credential fence while retaining live, identity-neutral
    /// destination restrictions. A credential-bound capability cannot be rebound this way.
    pub fn independent_account_scope(mut self) -> Result<Self, NetworkPolicyDenied> {
        if self.account.is_some() || self.account_scope.is_some() {
            return Err(NetworkPolicyDenied::Revoked);
        }
        self.account_scope = Some(Arc::new(Mutex::new(AccountScope {
            generation: 0,
            permits: Vec::new(),
            changes: watch::channel(()).0,
        })));
        Ok(self)
    }

    /// Narrows a bootstrap client's existing local policy to exact endpoint URLs.
    /// This cannot grant access to a destination denied by the underlying policy.
    pub fn restrict_to_endpoints(mut self, endpoints: BTreeSet<Url>) -> Self {
        self.endpoints = Some(Arc::new(match &self.endpoints {
            Some(existing) => existing.intersection(&endpoints).cloned().collect(),
            None => endpoints,
        }));
        self
    }

    /// Binds a content client to the current account so retained credentials cannot
    /// be used after another workspace's policy has been installed.
    pub fn for_current_account(mut self) -> Self {
        self.account = match &self.account_scope {
            Some(scope) => Some(lock_state(scope).generation),
            None => self.state.as_ref().map(|state| lock_state(state).account),
        };
        self
    }

    /// Bind a caller's restrictions to a captured credential policy without widening either.
    /// Different managed controllers or account bindings cannot authorize the same request.
    pub fn with_account_binding(mut self, credentials: &Self) -> Option<Self> {
        if let Some(state) = &credentials.state {
            if let Some(caller) = &self.state {
                if !Arc::ptr_eq(caller, state) {
                    return None;
                }
            } else {
                self.state = Some(Arc::clone(state));
            }
        }
        match (&self.account_scope, &credentials.account_scope) {
            (Some(caller), Some(credentials)) if !Arc::ptr_eq(caller, credentials) => return None,
            (Some(_), None) if credentials.account.is_some() => return None,
            (None, Some(_)) if self.account.is_some() => return None,
            (None, Some(scope)) => self.account_scope = Some(Arc::clone(scope)),
            _ => {}
        }
        if let (Some(caller), Some(credentials)) = (self.account, credentials.account)
            && caller != credentials
        {
            return None;
        }
        self.account = self.account.or(credentials.account);
        if let Some(endpoints) = &credentials.endpoints {
            self = self.restrict_to_endpoints(endpoints.as_ref().clone());
        }
        Some(self)
    }

    /// Whether this policy carries an independent account-revocation scope.
    pub fn has_account_scope(&self) -> bool {
        self.account_scope.is_some()
    }

    /// Intersect endpoint restrictions without changing the policy's authority.
    pub fn restrict_to_policy_endpoints(self, restrictions: &Self) -> Self {
        match &restrictions.endpoints {
            Some(endpoints) => self.restrict_to_endpoints(endpoints.as_ref().clone()),
            None => self,
        }
    }

    /// Whether this transport participates in the application policy lifecycle.
    pub fn is_managed(&self) -> bool {
        self.state.is_some() || self.account_scope.is_some()
    }

    /// Static endpoint restrictions need enforcement even without a policy owner.
    pub fn requires_enforcement(&self) -> bool {
        self.is_managed() || self.endpoints.is_some()
    }

    /// Watches effective changes; unmanaged callers have no application policy owner.
    pub fn changes(&self) -> Option<watch::Receiver<()>> {
        self.state
            .as_ref()
            .map(|state| lock_state(state).changes.subscribe())
            .or_else(|| {
                self.account_scope
                    .as_ref()
                    .map(|scope| lock_state(scope).changes.subscribe())
            })
    }

    pub fn revision(&self) -> NetworkPolicyRevision {
        NetworkPolicyRevision(
            self.state
                .as_ref()
                .map_or(0, |state| lock_state(state).revision),
        )
    }

    /// Revokes outstanding operations before an account change or a failed requirements load.
    /// This read-side operation can only remove access; publication stays with the controller.
    pub fn invalidate(&self) {
        if let Some(scope) = &self.account_scope {
            // Match acquisition's lock order. Machine-policy changes and this account's
            // credential changes both revoke the same request permits, never sibling accounts.
            let state = self.state.as_ref().map(|state| lock_state(state));
            let mut scope = lock_state(scope);
            scope.generation += 1;
            scope.changes.send_replace(());
            if let Some(state) = state.as_ref() {
                state.changes.send_replace(());
            }
            for permit in scope
                .permits
                .drain(..)
                .filter_map(|permit| permit.upgrade())
            {
                permit.revoked.send_replace(/*value*/ true);
            }
            return;
        }
        if let Some(state) = &self.state {
            let mut state = lock_state(state);
            state.revision += 1;
            state.account += 1;
            state.policy = None;
            state.changes.send_replace(());
            for permit in state
                .permits
                .drain(..)
                .filter_map(|permit| permit.upgrade())
            {
                permit.revoked.send_replace(/*value*/ true);
            }
        }
    }

    /// Checks a destination before proxy resolution, DNS, or transport work starts.
    pub fn acquire(&self, url: &Url) -> Result<NetworkPermit, NetworkPolicyDenied> {
        if self
            .endpoints
            .as_ref()
            .is_some_and(|endpoints| !endpoints.contains(url))
        {
            return Err(NetworkPolicyDenied::Destination);
        }
        self.acquire_destination(PermitDestination::Url(url.clone()))
    }

    /// Disables SDKs without destination enforcement whenever restrictions apply.
    /// The returned permit also cancels work if an unrestricted policy changes.
    pub fn acquire_for_unsupported_sdk(&self) -> Result<NetworkPermit, NetworkPolicyDenied> {
        if self.endpoints.is_some() {
            return Err(NetworkPolicyDenied::UnsupportedTransport);
        }
        self.acquire_destination(PermitDestination::UnrestrictedSdk)
    }

    fn acquire_destination(
        &self,
        destination: PermitDestination,
    ) -> Result<NetworkPermit, NetworkPolicyDenied> {
        let mut state = self.state.as_ref().map(|state| lock_state(state));
        let mut scope = self.account_scope.as_ref().map(|scope| lock_state(scope));
        if state.is_none() && scope.is_none() {
            return Ok(NetworkPermit { inner: None });
        }
        let generation = scope
            .as_ref()
            .map(|scope| scope.generation)
            .or_else(|| state.as_ref().map(|state| state.account));
        if self.account.is_some() && self.account != generation {
            return Err(NetworkPolicyDenied::Revoked);
        }
        if let Some(state) = state.as_ref() {
            let policy = state
                .policy
                .as_ref()
                .ok_or(NetworkPolicyDenied::Unavailable)?;
            if !destination.allowed_by(policy) {
                return Err(match destination {
                    PermitDestination::Url(_) => NetworkPolicyDenied::Destination,
                    PermitDestination::UnrestrictedSdk => NetworkPolicyDenied::UnsupportedTransport,
                });
            }
        }
        let (revoked, _) = watch::channel(/*init*/ false);
        let permit = Arc::new(Permit {
            destination,
            revoked,
        });
        if let Some(state) = state.as_mut() {
            state.permits.retain(|permit| permit.strong_count() != 0);
            state.permits.push(Arc::downgrade(&permit));
        }
        if let Some(scope) = scope.as_mut() {
            scope.permits.retain(|permit| permit.strong_count() != 0);
            scope.permits.push(Arc::downgrade(&permit));
        }
        Ok(NetworkPermit {
            inner: Some(permit),
        })
    }
}

/// Authorization retained for the entire operation, including response or WebSocket streaming.
#[derive(Clone)]
pub struct NetworkPermit {
    inner: Option<Arc<Permit>>,
}

impl fmt::Debug for NetworkPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkPermit").finish_non_exhaustive()
    }
}

impl NetworkPermit {
    pub fn check(&self) -> Result<(), NetworkPolicyDenied> {
        if self
            .inner
            .as_ref()
            .is_some_and(|permit| *permit.revoked.borrow())
        {
            Err(NetworkPolicyDenied::Revoked)
        } else {
            Ok(())
        }
    }

    pub async fn revoked(&self) {
        match &self.inner {
            Some(permit) => {
                let _ = permit
                    .revoked
                    .subscribe()
                    .wait_for(|revoked| *revoked)
                    .await;
            }
            None => std::future::pending().await,
        }
    }

    pub async fn run<T>(
        &self,
        operation: impl Future<Output = T>,
    ) -> Result<T, NetworkPolicyDenied> {
        tokio::select! {
            biased;
            _ = self.revoked() => Err(NetworkPolicyDenied::Revoked),
            result = operation => Ok(result),
        }
    }
}

#[cfg(test)]
#[path = "network_policy_tests.rs"]
mod tests;
