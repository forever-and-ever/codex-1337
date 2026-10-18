use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ContextRole(String);

impl ContextRole {
    pub fn new(text: &str) -> Result<Self, &'static str> {
        let text = text.trim();
        if text.is_empty() {
            return Err("context role must not be empty");
        }
        if text
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
        {
            return Err("context role must be a single line without control characters");
        }
        if text.chars().take(201).count() > 200 {
            return Err("context role must be at most 200 characters");
        }
        Ok(Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContextRole {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
