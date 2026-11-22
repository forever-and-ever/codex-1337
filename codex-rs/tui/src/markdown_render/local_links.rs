//! Local file-link destination parsing and display for Markdown transcripts.
//!
//! TELOS(literal-local-paths): local Markdown links show their full targets as ordinary text.
//! Preserve labels and absolute paths so they remain useful when copied. This module only formats
//! destinations; it does not resolve files or infer execution-host paths from the frontend filesystem.

use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use codex_utils_string::normalize_markdown_hash_location_suffix;
use regex_lite::Regex;
use std::sync::LazyLock;
use url::Url;

static COLON_LOCATION_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(
        || match Regex::new(r":\d+(?::\d+)?(?:[-–]\d+(?::\d+)?)?$") {
            Ok(regex) => regex,
            Err(error) => panic!("invalid location suffix regex: {error}"),
        },
    );

static HASH_LOCATION_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| match Regex::new(r"^L\d+(?:C\d+)?(?:-L\d+(?:C\d+)?)?$") {
        Ok(regex) => regex,
        Err(error) => panic!("invalid hash location regex: {error}"),
    });

pub(super) fn is_local_path_like_link(dest_url: &str) -> bool {
    dest_url.starts_with("file://")
        || dest_url.starts_with('/')
        || dest_url.starts_with("~/")
        || dest_url.starts_with("./")
        || dest_url.starts_with("../")
        || dest_url.starts_with("\\\\")
        || matches!(
            dest_url.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && matches!(separator, b'/' | b'\\')
        )
}

/// Parse a local link target into normalized path text plus an optional location suffix.
///
/// This accepts the path shapes Codex emits today: `file://` URLs, absolute and relative paths,
/// `~/...`, Windows paths, and `#L..C..` or `:line:col` suffixes.
pub(super) fn render_local_link_target(dest_url: &str) -> Option<String> {
    let (mut rendered, location_suffix) = parse_local_link_target(dest_url)?;
    if let Some(location_suffix) = location_suffix {
        rendered.push_str(&location_suffix);
    }
    Some(rendered)
}

/// Split a local-link destination into `(normalized_path_text, location_suffix)`.
///
/// The returned path text never includes a trailing `#L..` or `:line[:col]` suffix. Path
/// normalization preserves home-relative paths and uses forward slashes as separators.
/// A backslash in a POSIX file URI is part of the filename and is preserved.
///
/// Returns `None` only when the destination looks like a `file://` URL but cannot be parsed into a
/// local path. Plain path-like inputs always return `Some(...)` even if they are relative.
fn parse_local_link_target(dest_url: &str) -> Option<(String, Option<String>)> {
    if dest_url.starts_with("file://") {
        let url = Url::parse(dest_url).ok()?;
        // Infer the path's OS from the URI, not the host running the TUI.
        let path = PathUri::parse(&url[..url::Position::AfterPath]).ok()?;
        let native_path = path.inferred_native_path_string();
        let path_text = if path.infer_path_convention() == Some(PathConvention::Posix) {
            native_path
        } else {
            normalize_local_link_path_text(&native_path)
        };
        let location_suffix = url
            .fragment()
            .and_then(normalize_hash_location_suffix_fragment);
        return Some((path_text, location_suffix));
    }

    let mut path_text = dest_url;
    let mut location_suffix = None;
    // Prefer `#L..` style fragments when both forms are present so URLs like `path#L10` do not
    // get misparsed as a plain path ending in `:10`.
    if let Some((candidate_path, fragment)) = dest_url.rsplit_once('#')
        && let Some(normalized) = normalize_hash_location_suffix_fragment(fragment)
    {
        path_text = candidate_path;
        location_suffix = Some(normalized);
    }
    if location_suffix.is_none()
        && let Some(suffix) = extract_colon_location_suffix(path_text)
    {
        let path_len = path_text.len().saturating_sub(suffix.len());
        path_text = &path_text[..path_len];
        location_suffix = Some(suffix);
    }

    let decoded_path_text =
        urlencoding::decode(path_text).unwrap_or(std::borrow::Cow::Borrowed(path_text));
    Some((
        normalize_local_link_path_text(&decoded_path_text),
        location_suffix,
    ))
}

/// Normalize a hash fragment like `L12` or `L12C3-L14C9` into the display suffix we render.
///
/// Returns `None` for fragments that are not location references. This deliberately ignores other
/// `#...` fragments so non-location hashes stay part of the path text.
fn normalize_hash_location_suffix_fragment(fragment: &str) -> Option<String> {
    HASH_LOCATION_SUFFIX_RE
        .is_match(fragment)
        .then(|| format!("#{fragment}"))
        .and_then(|suffix| normalize_markdown_hash_location_suffix(&suffix))
}

/// Extract a trailing `:line`, `:line:col`, or range suffix from a plain path-like string.
///
/// The suffix must occur at the end of the input; embedded colons elsewhere in the path are left
/// alone. This is what keeps Windows drive letters like `C:/...` from being misread as locations.
pub(super) fn extract_colon_location_suffix(path_text: &str) -> Option<String> {
    COLON_LOCATION_SUFFIX_RE
        .find(path_text)
        .filter(|matched| matched.end() == path_text.len())
        .map(|matched| matched.as_str().to_string())
}

/// Normalize local-path text into the transcript display form.
///
/// Display normalization is intentionally lexical: it does not touch the filesystem, resolve
/// symlinks, or collapse `.` / `..`. It only converts separators to forward slashes and rewrites
/// UNC-style `\\\\server\\share` inputs into `//server/share` for display.
fn normalize_local_link_path_text(path_text: &str) -> String {
    // Render all local link paths with forward slashes so display is stable
    // across mixed Windows and Unix-style inputs.
    if let Some(rest) = path_text.strip_prefix("\\\\") {
        format!("//{}", rest.replace('\\', "/").trim_start_matches('/'))
    } else {
        path_text.replace('\\', "/")
    }
}
