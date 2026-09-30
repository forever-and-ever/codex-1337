use super::CredentialAuthMethod;
use super::CredentialProviderConfig;
use super::destination;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use http::HeaderName;
use rand::Rng as _;
use regex::RegexBuilder;
use regex_automata::MatchKind;
use regex_automata::nfa::thompson;
use regex_automata::nfa::thompson::pikevm::PikeVM;
use regex_syntax::hir::Class;
use regex_syntax::hir::ClassBytes;
use regex_syntax::hir::ClassBytesRange;
use regex_syntax::hir::ClassUnicode;
use regex_syntax::hir::ClassUnicodeRange;
use regex_syntax::hir::Hir;
use regex_syntax::hir::HirKind;
use regex_syntax::hir::Look;

const MAX_REGEX_BYTES: usize = 2048;
const MAX_REGEX_REPEAT: u32 = 64;
const MAX_DUMMY_ATTEMPTS: usize = 64;
const MAX_DUMMY_VALUE_BYTES: usize = 2048;

#[derive(Clone, Copy)]
enum PatternPurpose {
    Discovery,
    AsciiDummy,
    UnicodeDummy,
}

pub(super) fn validate(id: &str, config: &CredentialProviderConfig) -> Result<()> {
    ensure!(!id.is_empty(), "credential provider name must not be empty");
    ensure!(
        !config.env.is_empty(),
        "credential provider `{id}` has no environment keys"
    );
    ensure!(
        !config.patterns.is_empty(),
        "credential provider `{id}` has no credential patterns"
    );
    ensure!(
        !config.url_prefixes.is_empty() || config.url_prefix_from_env.is_some(),
        "credential provider `{id}` has no destination URL prefixes"
    );
    ensure!(
        config.env.iter().all(|key| valid_environment_key(key)),
        "credential provider `{id}` has an invalid environment key"
    );
    ensure!(
        !config
            .env
            .iter()
            .any(|key| super::is_credential_broker_provider_env_key(key)),
        "credential provider `{id}` overlaps a built-in credential source"
    );
    if let Some(key) = config.url_prefix_from_env.as_deref() {
        ensure!(
            valid_environment_key(key)
                && !config.env.iter().any(|source| {
                    source.as_str() == key || cfg!(windows) && source.eq_ignore_ascii_case(key)
                }),
            "credential provider `{id}` has an invalid host environment key"
        );
    }
    for destination in &config.url_prefixes {
        destination::validate(destination)
            .with_context(|| format!("invalid destination for credential provider `{id}`"))?;
    }
    let methods = auth_methods(config);
    let header = config
        .header
        .as_deref()
        .map(|header| HeaderName::from_bytes(header.as_bytes()))
        .transpose()
        .with_context(|| format!("invalid header for credential provider `{id}`"))?;
    ensure!(
        methods.contains(&CredentialAuthMethod::Header) == header.is_some(),
        "credential provider `{id}` requires a header name exactly when using header authentication"
    );
    ensure!(
        config.prefix.is_none() || header.is_some(),
        "credential provider `{id}` requires header authentication to use a header prefix"
    );
    config
        .patterns
        .iter()
        .try_for_each(|pattern| -> Result<()> {
            ensure!(
                pattern.len() <= MAX_REGEX_BYTES,
                "credential pattern exceeds {MAX_REGEX_BYTES} bytes"
            );
            let parsed = regex_syntax::parse(pattern)?;
            let searchable = prepare_credential_pattern(parsed.clone(), PatternPurpose::Discovery);
            let searchable_pattern = searchable.to_string();
            RegexBuilder::new(&searchable_pattern)
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20)
                .build()?;
            PikeVM::builder()
                .configure(PikeVM::config().match_kind(MatchKind::All))
                .thompson(thompson::Config::new().nfa_size_limit(Some(1 << 20)))
                .build(&searchable_pattern)?;
            RegexBuilder::new(&format!(r"\A(?s:.)(?:{searchable_pattern})(?s:.)\z"))
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20)
                .build()?;
            let ascii_generator = rand_regex::Regex::with_hir(
                prepare_credential_pattern(parsed.clone(), PatternPurpose::AsciiDummy),
                MAX_REGEX_REPEAT,
            )
            .ok();
            let generator = rand_regex::Regex::with_hir(
                prepare_credential_pattern(parsed.clone(), PatternPurpose::UnicodeDummy),
                MAX_REGEX_REPEAT,
            )?;
            let full_pattern =
                Hir::concat(vec![Hir::look(Look::Start), parsed, Hir::look(Look::End)]).to_string();
            let full_matcher = RegexBuilder::new(&full_pattern)
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20)
                .build()?;
            ensure!(
                searchable
                    .properties()
                    .minimum_len()
                    .is_some_and(|length| length > 0),
                "credential pattern must not match an empty value"
            );
            ensure!(
                generator.is_utf8() && generator.capacity() <= MAX_REGEX_BYTES,
                "credential pattern must generate bounded UTF-8 values"
            );

            ensure!(
                ascii_generator
                    .iter()
                    .chain(std::iter::once(&generator))
                    .flat_map(|generator| {
                        (0..MAX_DUMMY_ATTEMPTS)
                            .map(move |_| rand::rng().sample::<String, _>(generator))
                    })
                    .any(|candidate| {
                        candidate.len() <= MAX_DUMMY_VALUE_BYTES
                            && !candidate.contains('\0')
                            && full_matcher.is_match(&candidate)
                    }),
                "credential pattern could not independently generate a matching dummy"
            );
            Ok(())
        })
        .with_context(|| format!("invalid credential pattern for provider `{id}`"))?;

    Ok(())
}

fn prepare_credential_pattern(hir: Hir, purpose: PatternPurpose) -> Hir {
    match hir.into_kind() {
        HirKind::Empty => Hir::empty(),
        HirKind::Literal(literal) => {
            if matches!(purpose, PatternPurpose::AsciiDummy) && !literal.0.is_ascii() {
                Hir::fail()
            } else {
                Hir::literal(literal.0)
            }
        }
        HirKind::Class(mut class) => {
            if matches!(purpose, PatternPurpose::AsciiDummy) {
                match &mut class {
                    Class::Unicode(class) => {
                        class.intersect(&ClassUnicode::new([ClassUnicodeRange::new(' ', '~')]))
                    }
                    Class::Bytes(class) => {
                        class.intersect(&ClassBytes::new([ClassBytesRange::new(b' ', b'~')]))
                    }
                }
            }
            Hir::class(class)
        }
        HirKind::Look(look) => {
            if matches!(purpose, PatternPurpose::Discovery)
                && !matches!(
                    look,
                    Look::Start
                        | Look::End
                        | Look::StartLF
                        | Look::EndLF
                        | Look::StartCRLF
                        | Look::EndCRLF
                )
            {
                Hir::look(look)
            } else {
                Hir::empty()
            }
        }
        HirKind::Repetition(mut repetition) => {
            if matches!(purpose, PatternPurpose::Discovery) {
                // Discovery needs the whole credential, not a lazy prefix of it.
                repetition.greedy = true;
            }
            repetition.sub = Box::new(prepare_credential_pattern(*repetition.sub, purpose));
            Hir::repetition(repetition)
        }
        HirKind::Capture(mut capture) => {
            capture.sub = Box::new(prepare_credential_pattern(*capture.sub, purpose));
            Hir::capture(capture)
        }
        HirKind::Concat(expressions) => Hir::concat(
            expressions
                .into_iter()
                .map(|expression| prepare_credential_pattern(expression, purpose))
                .collect(),
        ),
        HirKind::Alternation(expressions) => {
            let mut expressions = expressions
                .into_iter()
                .map(|expression| prepare_credential_pattern(expression, purpose))
                .filter(|expression| expression.properties().minimum_len().is_some())
                .collect::<Vec<_>>();
            expressions.sort_by_key(|expression| {
                std::cmp::Reverse(expression.properties().maximum_len().unwrap_or(usize::MAX))
            });
            Hir::alternation(expressions)
        }
    }
}

fn auth_methods(config: &CredentialProviderConfig) -> &[CredentialAuthMethod] {
    if config.auth.is_empty() {
        &[CredentialAuthMethod::Bearer]
    } else {
        &config.auth
    }
}

pub(super) fn valid_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
