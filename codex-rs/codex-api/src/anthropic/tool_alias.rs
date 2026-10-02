use super::AnthropicToolIdentity;
use crate::ApiError;
use serde_json::json;
use uuid::Uuid;

impl AnthropicToolIdentity {
    /// The alias depends only on the exact tool identity, never the current catalogue.
    /// Readable aliases reserve the namespace delimiter and generated digest suffix.
    pub fn wire_alias(&self) -> Result<String, ApiError> {
        let (kind, name, namespace) = match self {
            Self::Function { name, namespace } => ("function", name, namespace),
            Self::Custom { name, namespace } => ("custom", name, namespace),
        };
        if name.is_empty() {
            return Err(ApiError::InvalidRequest {
                message: "Anthropic tool name is empty".to_owned(),
            });
        }
        let (alias, ambiguous) = match namespace.as_deref().filter(|value| !value.is_empty()) {
            Some(namespace) => (
                format!("{namespace}__{name}"),
                namespace.contains("__")
                    || namespace.ends_with('_')
                    || name.contains("__")
                    || name.starts_with('_'),
            ),
            None => (name.clone(), name.contains("__")),
        };
        let reserved_suffix = alias.rsplit_once('_').is_some_and(|(_, suffix)| {
            suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !ambiguous
            && !reserved_suffix
            && alias.len() <= 64
            && alias
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Ok(alias);
        }
        let identity = json!(["codex-anthropic-tool-v1", kind, namespace, name]).to_string();
        let digest = Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()).simple();
        let stem: String = alias
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .take(31)
            .collect();
        Ok(format!("{stem}_{digest}"))
    }
}
