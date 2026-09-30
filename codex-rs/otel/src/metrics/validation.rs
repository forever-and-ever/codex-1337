use crate::metrics::error::MetricsError;
use crate::metrics::error::Result;



pub(crate) fn validate_tag_key(key: &str) -> Result<()> {
    validate_tag_component(key, "tag key")?;
    Ok(())
}

pub(crate) fn validate_tag_value(value: &str) -> Result<()> {
    validate_tag_component(value, "tag value")
}

fn validate_tag_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty() {
        return Err(MetricsError::EmptyTagComponent {
            label: label.to_string(),
        });
    }
    if !value.chars().all(is_tag_char) {
        return Err(MetricsError::InvalidTagComponent {
            label: label.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}


fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')
}
