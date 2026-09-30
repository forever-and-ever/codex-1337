use crate::policy::Host;
use crate::policy::is_loopback_host;
use crate::policy::normalize_host;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use url::Url;

#[derive(Clone, Copy)]
enum CredentialDestinationScheme {
    Http,
    Https,
}

pub(super) fn validate(value: &str) -> Result<()> {
    let value = value.trim();
    let (explicit_scheme, authority) = value
        .split_once("://")
        .map_or((None, value), |(scheme, authority)| {
            (Some(scheme), authority)
        });
    let (wildcard, authority) = authority
        .strip_prefix("*.")
        .map_or((false, authority), |suffix| (true, suffix));
    let authority = if wildcard {
        format!("wildcard.{authority}")
    } else {
        authority.to_string()
    };
    let url = Url::parse(&format!("https://{authority}"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "credential destination cannot include user information, a query, or a fragment"
    );
    let host = url
        .host_str()
        .context("credential destination has no hostname")?;
    let host = if wildcard {
        host.strip_prefix("wildcard.")
            .context("credential destination has an invalid wildcard")?
    } else {
        host
    };
    let host = normalize_host(host);
    ensure!(
        !host.is_empty() && !host.contains('*') && !host.contains('/'),
        "credential destination must have an exact hostname or scoped wildcard"
    );
    let loopback = !wildcard && is_loopback_host(&Host::parse(&host)?);
    let scheme = match explicit_scheme {
        None if loopback => CredentialDestinationScheme::Http,
        None => CredentialDestinationScheme::Https,
        Some(scheme) if scheme.eq_ignore_ascii_case("https") => CredentialDestinationScheme::Https,
        Some(scheme) if scheme.eq_ignore_ascii_case("http") && loopback => {
            CredentialDestinationScheme::Http
        }
        Some(_) => {
            bail!("credential destination must use HTTPS unless it targets loopback over HTTP")
        }
    };
    // Reparse HTTP destinations to preserve its distinct authority validation.
    if matches!(scheme, CredentialDestinationScheme::Http) {
        Url::parse(&format!("http://{authority}"))?;
    }
    Ok(())
}
