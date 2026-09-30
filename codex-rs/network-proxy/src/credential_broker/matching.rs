use super::providers;

fn ignored_credential_match(
    provider: &providers::CredentialProvider,
    text: &str,
    start: usize,
    credential: &str,
) -> bool {
    provider
        .ignored_credential_prefixes
        .iter()
        .any(|prefix| credential.starts_with(prefix))
        && provider
            .credential_watermark
            .is_none_or(|watermark| !credential.contains(watermark))
        || credential.starts_with("sk-")
            && provider
                .credential_watermark
                .is_none_or(|watermark| !credential.contains(watermark))
            && credential[3..].split(['-', '_']).any(|segment| {
                segment.len() == 64
                    && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && credential
                        .find(segment)
                        .is_some_and(|offset| offset < provider.minimum_credential_len)
            })
            && text[..start]
                .rsplit(|character: char| {
                    character.is_ascii() && !character.is_ascii_alphanumeric()
                })
                .next()
                .is_some_and(|word| {
                    ((1..=3).contains(&word.len())
                        || word
                            .get(word.len().saturating_sub(2)..)
                            .is_some_and(|suffix| {
                                suffix.eq_ignore_ascii_case("di")
                                    || suffix.eq_ignore_ascii_case("ta")
                                        && !word.eq_ignore_ascii_case("data")
                            })
                        || credential.strip_prefix("sk-").is_some_and(|hash| {
                            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        }))
                        && word.bytes().all(|byte| byte.is_ascii_alphabetic())
                        && text[..start]
                            .rsplit_once(['/', '\\'])
                            .is_some_and(|(_, component)| {
                                component.chars().all(|character| {
                                    !character.is_ascii()
                                        || character.is_ascii_alphanumeric()
                                        || matches!(character, '_' | '-')
                                })
                            })
                })
}

pub(super) fn recognized_credential_match<'a>(
    provider: &providers::CredentialProvider,
    value: &'a str,
    virtualized: &str,
    start: usize,
) -> Option<&'a str> {
    let credential = builtin_credential_candidate(provider, value, start);
    let length = credential.len();
    let enclosing_start = value.as_bytes()[..start]
        .iter()
        .rposition(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-'))
        .map_or(0, |offset| offset + 1);
    let enclosing = &value[enclosing_start..start + length];
    (length >= provider.minimum_credential_len || !virtualized.contains(credential))
        .then_some(credential)
        .filter(|credential| !ignored_credential_match(provider, value, start, credential))
        .filter(|_| {
            !provider.ignored_credential_prefixes.iter().any(|prefix| {
                enclosing.match_indices(prefix).any(|(offset, _)| {
                    let ignored = &enclosing[offset..];
                    offset <= start - enclosing_start
                        && ignored_credential_match(
                            provider,
                            value,
                            enclosing_start + offset,
                            ignored,
                        )
                        && virtualized.contains(ignored)
                })
            })
        })
}

fn builtin_credential_candidate<'a>(
    provider: &providers::CredentialProvider,
    value: &'a str,
    start: usize,
) -> &'a str {
    let mut length = value.as_bytes()[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        .count();
    let candidate = &value[start..start + length];
    let current_prefix_len = provider
        .credential_prefixes
        .iter()
        .filter(|prefix| candidate.starts_with(**prefix))
        .map(|prefix| prefix.len())
        .max()
        .unwrap_or(0);
    if let Some(separator) = providers::credential_providers()
        .flat_map(|candidate_provider| {
            candidate_provider
                .credential_prefixes
                .iter()
                .map(move |prefix| (prefix, candidate_provider.minimum_credential_len))
        })
        .filter_map(|(candidate_prefix, minimum_length)| {
            candidate[current_prefix_len..]
                .match_indices(*candidate_prefix)
                .find_map(|(offset, _)| {
                    let offset = current_prefix_len + offset;
                    (matches!(candidate.as_bytes()[offset - 1], b'_' | b'-')
                        && offset > provider.minimum_credential_len
                        && candidate.len() - offset >= minimum_length)
                        .then_some(offset - 1)
                })
        })
        .min()
    {
        length = separator;
    }
    let candidate = &value[start..start + length];
    if value.as_bytes().get(start + length) == Some(&b'\0') {
        // A masked known span may follow an alias separator, not part of this builtin token.
        candidate.trim_end_matches(['_', '-'])
    } else {
        candidate
    }
}
