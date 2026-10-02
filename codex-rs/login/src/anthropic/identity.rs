use std::fmt;
use std::io;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

/// Stable registry key derived only from the upstream account identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountId(String);

impl FromStr for AccountId {
    type Err = io::Error;

    fn from_str(value: &str) -> io::Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(io::Error::other("invalid Anthropic account ID"));
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for AccountId {
    type Error = io::Error;

    fn try_from(value: String) -> io::Result<Self> {
        value.parse()
    }
}

impl From<AccountId> for String {
    fn from(value: AccountId) -> Self {
        value.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IdentityWire", into = "IdentityWire")]
pub struct AccountIdentity {
    account_uuid: String,
}

impl AccountIdentity {
    pub fn new(account_uuid: &str) -> io::Result<Self> {
        let bounded = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        };
        if !bounded(account_uuid) {
            return Err(io::Error::other(
                "invalid Anthropic upstream account identity",
            ));
        }
        Ok(Self {
            account_uuid: account_uuid.to_owned(),
        })
    }

    pub fn id(&self) -> AccountId {
        let mut hash = Sha256::new();
        hash.update(self.account_uuid.as_bytes());
        AccountId(format!("{:x}", hash.finalize()))
    }

    pub fn account_uuid(&self) -> &str {
        &self.account_uuid
    }
}

#[derive(Serialize, Deserialize)]
struct IdentityWire {
    account_uuid: String,
}

impl TryFrom<IdentityWire> for AccountIdentity {
    type Error = io::Error;
    fn try_from(value: IdentityWire) -> io::Result<Self> {
        Self::new(&value.account_uuid)
    }
}

impl From<AccountIdentity> for IdentityWire {
    fn from(value: AccountIdentity) -> Self {
        Self {
            account_uuid: value.account_uuid,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub identity: AccountIdentity,
    pub email: Option<String>,
    pub organization_uuid: Option<String>,
    pub organization_name: Option<String>,
}
