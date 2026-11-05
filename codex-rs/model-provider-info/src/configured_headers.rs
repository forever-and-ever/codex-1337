//! Configuration cannot distinguish public headers from credentials.

use crate::ModelProviderInfo;
use http::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;

pub(crate) fn resolve(info: &ModelProviderInfo) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let literals = info
        .http_headers
        .iter()
        .flat_map(|headers| headers.iter())
        .map(|(name, value)| (name, Some(value.to_string())));
    let environment = info
        .env_http_headers
        .iter()
        .flat_map(|headers| headers.iter())
        .map(|(name, variable)| {
            (
                name,
                std::env::var(variable)
                    .ok()
                    .filter(|value| !value.trim().is_empty()),
            )
        });
    for (name, value) in literals.chain(environment) {
        if let Some(value) = value
            && let (Ok(name), Ok(mut value)) =
                (HeaderName::try_from(name), HeaderValue::try_from(value))
        {
            value.set_sensitive(true);
            headers.insert(name, value);
        }
    }
    headers
}
