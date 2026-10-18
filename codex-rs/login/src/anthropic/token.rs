use std::fmt;
use std::io;

use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use super::AccountId;

/// OAuth material accepted only through a validating constructor; never serialized by callers.
#[derive(Clone, PartialEq, Eq)]
pub struct SubscriptionGrant {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(super) expires_at: Option<i64>,
}

impl SubscriptionGrant {
    pub fn new(access_token: String, refresh_token: String, expires_at: i64) -> io::Result<Self> {
        GrantWire {
            access_token,
            refresh_token: Some(refresh_token),
            expires_at: Some(expires_at),
        }
        .try_into()
    }

    pub fn bearer_only(access_token: String) -> io::Result<Self> {
        GrantWire {
            access_token,
            refresh_token: None,
            expires_at: None,
        }
        .try_into()
    }

    pub(super) fn usable(&self) -> bool {
        self.expires_at
            .is_none_or(|expiry| expiry > Utc::now().timestamp() + 30)
    }
}

impl fmt::Debug for SubscriptionGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SubscriptionGrant([REDACTED])")
    }
}

/// A request captures this account and bearer together; later selection cannot change either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub(super) account_id: AccountId,
    pub(super) grant: SubscriptionGrant,
}

impl Credentials {
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.account_id
    }
    #[must_use]
    pub fn access_token(&self) -> &str {
        &self.grant.access_token
    }
    #[must_use]
    pub fn expires_at(&self) -> Option<i64> {
        self.grant.expires_at
    }
}

// Only persistence wire types can serialize secrets.
#[derive(Serialize, Deserialize)]
pub(super) struct GrantWire {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
}

impl From<SubscriptionGrant> for GrantWire {
    fn from(value: SubscriptionGrant) -> Self {
        Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            expires_at: value.expires_at,
        }
    }
}

impl TryFrom<GrantWire> for SubscriptionGrant {
    type Error = io::Error;
    fn try_from(value: GrantWire) -> io::Result<Self> {
        if value.access_token.is_empty()
            || value.access_token.len() > 65536
            || !value.access_token.bytes().all(|b| b.is_ascii_graphic())
            || value
                .refresh_token
                .as_ref()
                .is_some_and(|token| token.trim().is_empty() || token.len() > 65536)
            || value.expires_at.is_some_and(|expiry| expiry <= 0)
        {
            return Err(io::Error::other("invalid Anthropic subscription grant"));
        }
        Ok(Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            expires_at: value.expires_at,
        })
    }
}
