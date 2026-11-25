//! `OpenRouter` values shared by config, the API client, core and the app-server protocol.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Error;
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Add;
use std::ops::AddAssign;
use std::str::FromStr;
use ts_rs::TS;

/// String conversions of a validated string newtype, derived from its `FromStr` and `as_str`.
macro_rules! string_value {
    ($name:ident) => {
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl TryFrom<&str> for $name {
            type Error = String;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.as_str().to_owned()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

/// The name of an `OpenRouter` key; config maps it to the file holding the secret.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
#[ts(type = "string")]
pub struct KeyName(String);

impl KeyName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for KeyName {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.starts_with("sk-or-") {
            return Err("Name the OpenRouter key; the key itself belongs in its file".into());
        }
        if !(1..=64).contains(&value.len()) || !value.bytes().all(slug_byte) {
            return Err(
                "OpenRouter key name must be 1–64 letters, digits, dots, underscores or hyphens"
                    .into(),
            );
        }
        Ok(Self(value.to_owned()))
    }
}
string_value!(KeyName);

/// Dispatch selected for a thread's next `OpenRouter` model step.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum OpenRouterTransport {
    #[default]
    Live,
    Batch,
}

impl OpenRouterTransport {
    pub fn is_live(&self) -> bool {
        *self == Self::Live
    }
}

/// The `OpenRouter` model id sent as `model`: `[~]author/name[:variant]` or `@preset/name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub enum ModelSlug {
    Model(String),
    Preset(String),
}

impl ModelSlug {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Model(value) | Self::Preset(value) => value,
        }
    }
}

fn slug_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn slug_segment(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(slug_byte)
}

impl FromStr for ModelSlug {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let invalid = || {
            "OpenRouter model slug must be author/name[:variant] or @preset/name (at most 256 bytes)".to_owned()
        };
        if value.len() > 256 {
            return Err(invalid());
        }
        if let Some(name) = value.strip_prefix("@preset/") {
            return slug_segment(name)
                .then(|| Self::Preset(value.to_owned()))
                .ok_or_else(invalid);
        }
        let model = value.strip_prefix('~').unwrap_or(value);
        let (model, variant) = model
            .split_once(':')
            .map_or((model, None), |(model, variant)| (model, Some(variant)));
        if variant.is_some_and(|variant| !slug_segment(variant)) {
            return Err(invalid());
        }
        let Some((author, name)) = model.split_once('/') else {
            return Err(invalid());
        };
        if !slug_segment(author) || !slug_segment(name) {
            return Err(invalid());
        }
        Ok(Self::Model(value.to_owned()))
    }
}
string_value!(ModelSlug);

impl JsonSchema for ModelSlug {
    fn schema_name() -> String {
        "ModelSlug".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        String::json_schema(generator)
    }
}

/// An `OpenRouter` endpoint, `host[/variant][/region][/tier]`; a base slug names a host.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
#[ts(type = "string")]
pub struct EndpointSlug(String);

impl EndpointSlug {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this slug names the endpoint `tag` or a slash-delimited prefix of it.
    pub fn matches(&self, tag: &str) -> bool {
        tag == self.as_str()
            || tag
                .strip_prefix(self.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

impl FromStr for EndpointSlug {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 128
            || !value.split('/').all(|segment| {
                slug_segment(segment) && !segment.bytes().any(|byte| byte.is_ascii_uppercase())
            })
        {
            return Err("OpenRouter endpoint slug must use lowercase letters, digits, dots, underscores or hyphens in nonempty slash-separated segments (at most 128 bytes)".into());
        }
        Ok(Self(value.to_owned()))
    }
}
string_value!(EndpointSlug);

/// A nonempty set of endpoint slugs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSet(BTreeSet<EndpointSlug>);

impl EndpointSet {
    /// `None` when `slugs` is empty.
    pub fn new(slugs: BTreeSet<EndpointSlug>) -> Option<Self> {
        (!slugs.is_empty()).then_some(Self(slugs))
    }

    pub fn slugs(&self) -> &BTreeSet<EndpointSlug> {
        &self.0
    }

    /// Whether some slug in the set matches the endpoint `tag`.
    pub fn matches(&self, tag: &str) -> bool {
        self.0.iter().any(|slug| slug.matches(tag))
    }
}

impl Serialize for EndpointSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EndpointSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(BTreeSet::deserialize(deserializer)?)
            .ok_or_else(|| D::Error::custom("endpoint filter lists must not be empty"))
    }
}

/// Which endpoints may serve a saved model. Wire shape: `{}`, `{"only": [..]}` or
/// `{"ignore": [..]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, TS)]
#[ts(type = "{ only?: Array<string>, ignore?: never } | { ignore: Array<string>, only?: never }")]
pub enum EndpointFilter {
    #[default]
    All,
    Only(EndpointSet),
    Ignore(EndpointSet),
}

impl EndpointFilter {
    /// Whether the endpoint `tag` may serve under this filter.
    pub fn admits(&self, tag: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(slugs) => slugs.matches(tag),
            Self::Ignore(slugs) => !slugs.matches(tag),
        }
    }
}

impl Serialize for EndpointFilter {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let entry = match self {
            Self::All => None,
            Self::Only(slugs) => Some(("only", slugs)),
            Self::Ignore(slugs) => Some(("ignore", slugs)),
        };
        let mut map = serializer.serialize_map(Some(usize::from(entry.is_some())))?;
        if let Some((kind, slugs)) = entry {
            map.serialize_entry(kind, slugs)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for EndpointFilter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FilterVisitor;
        impl<'de> serde::de::Visitor<'de> for FilterVisitor {
            type Value = EndpointFilter;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an empty map or one nonempty only/ignore list")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let Some(kind) = map.next_key::<String>()? else {
                    return Ok(EndpointFilter::All);
                };
                let filter = match kind.as_str() {
                    "only" => EndpointFilter::Only(map.next_value()?),
                    "ignore" => EndpointFilter::Ignore(map.next_value()?),
                    _ => return Err(M::Error::unknown_field(&kind, &["only", "ignore"])),
                };
                if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(M::Error::custom(
                        "endpoint filter takes one of only or ignore, not both",
                    ));
                }
                Ok(filter)
            }
        }
        deserializer.deserialize_map(FilterVisitor)
    }
}

impl JsonSchema for EndpointFilter {
    fn schema_name() -> String {
        "EndpointFilter".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        EndpointFilterSchema::json_schema(generator)
    }
}

// EndpointFilter's wire shape, for its JSON schema only.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(description = "Which endpoints may serve a saved model.")]
enum EndpointFilterSchema {
    All(EndpointFilterAll),
    Only(EndpointFilterOnly),
    Ignore(EndpointFilterIgnore),
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EndpointFilterAll {}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EndpointFilterOnly {
    #[schemars(length(min = 1))]
    only: BTreeSet<EndpointSlug>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EndpointFilterIgnore {
    #[schemars(length(min = 1))]
    ignore: BTreeSet<EndpointSlug>,
}

/// An exact US dollar amount in nanodollars; serializes as a number of dollars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "number")]
pub struct Usd(i64);

impl Usd {
    pub const fn from_nanodollars(value: i64) -> Self {
        Self(value)
    }
    pub const fn nanodollars(self) -> i64 {
        self.0
    }
}

impl Add for Usd {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(self.0.strict_add(other.0))
    }
}
impl AddAssign for Usd {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl TryFrom<f64> for Usd {
    type Error = String;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "The rounded finite value is checked against the inclusive -2^63 and exclusive 2^63 bounds before casting"
    )]
    fn try_from(dollars: f64) -> Result<Self, Self::Error> {
        let nanos = (dollars * 1_000_000_000.0).round();
        if !nanos.is_finite()
            || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&nanos)
        {
            return Err("USD amount is invalid or exceeds nanodollar range".into());
        }
        Ok(Self(nanos as i64))
    }
}

impl FromStr for Usd {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(
            value
                .parse::<f64>()
                .map_err(|_| "Invalid USD amount".to_owned())?,
        )
    }
}

impl Serialize for Usd {
    #[expect(
        clippy::cast_precision_loss,
        reason = "The wire contract represents dollars as floating-point numbers; stored nanodollars and arithmetic remain exact"
    )]
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0 as f64 / 1_000_000_000.0)
    }
}
impl<'de> Deserialize<'de> for Usd {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(f64::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
impl JsonSchema for Usd {
    fn schema_name() -> String {
        "Usd".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        f64::json_schema(generator)
    }
}

/// `$0.42`: two decimals, or two significant digits below one cent.
impl fmt::Display for Usd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let magnitude = self.0.unsigned_abs();
        let decimals = if magnitude == 0 || magnitude >= 10_000_000 {
            2
        } else {
            (10 - magnitude.ilog10()).min(9)
        };
        let unit = 10u64.pow(9 - decimals);
        let rounded = (magnitude + unit / 2) / unit;
        let factor = 10u64.pow(decimals);
        write!(
            f,
            "{}${}.{:0width$}",
            if self.0 < 0 { "-" } else { "" },
            rounded / factor,
            rounded % factor,
            width = decimals as usize
        )
    }
}

/// Adds a known `cost` to a running total; an unknown cost leaves it unchanged.
pub fn add_cost(total: &mut Option<Usd>, cost: Option<Usd>) {
    if let Some(cost) = cost {
        *total = Some(total.unwrap_or_default() + cost);
    }
}

/// The spend accrued between two running totals, never negative.
pub fn cost_since(total: Option<Usd>, previous: Option<Usd>) -> Option<Usd> {
    total.map(|total| {
        Usd::from_nanodollars(
            total
                .nanodollars()
                .saturating_sub(previous.unwrap_or_default().nanodollars())
                .max(0),
        )
    })
}
