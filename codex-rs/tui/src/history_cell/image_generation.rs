//! Image history preserves the last known status without inventing completion or liveness.

use super::PlainHistoryCell;
use codex_utils_absolute_path::AbsolutePathBuf;
use ratatui::style::Stylize;
use ratatui::text::Line;
use url::Url;

pub(super) fn new_image_generation_call(
    call_id: String,
    status: &str,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or(call_id);
    let heading = match status {
        "completed" => vec!["• ".dim(), "Generated Image:".bold()].into(),
        "failed" => vec!["✗ ".red().bold(), "Image generation failed".bold()].into(),
        status => {
            let status = if status.is_empty() {
                "status unavailable"
            } else {
                status
            };
            vec!["• ".dim(), format!("Image generation · {status}").bold()].into()
        }
    };
    let mut lines: Vec<Line<'static>> = vec![heading, vec!["  └ ".dim(), detail.dim()].into()];
    if let Some(saved_path) = saved_path {
        let saved_path = Url::from_file_path(saved_path.as_path())
            .map(|url| url.to_string())
            .unwrap_or_else(|_| saved_path.display().to_string());
        lines.push(vec!["  └ ".dim(), "Saved to: ".dim(), saved_path.into()].into());
    }

    PlainHistoryCell::new(lines)
}
