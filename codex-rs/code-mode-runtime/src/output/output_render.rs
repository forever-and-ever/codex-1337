//! Applies the outer Code Mode response budget at an observation boundary.

use codex_code_mode_protocol::DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL;
use codex_code_mode_protocol::FunctionCallOutputContentItem as OutputItem;
use codex_code_mode_protocol::ImageDetail;
use codex_protocol::models::AudioReference;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageDetail as ProtocolImageDetail;
use codex_protocol::models::ImageReference;
use codex_utils_audio::estimate_tool_audio_tokens_without_cache;
use codex_utils_output_truncation::OutputBoundary;
use codex_utils_output_truncation::formatted_output_at_token_boundaries;
use codex_utils_output_truncation::mixed_output_at_token_boundaries;

use crate::output::CollectedOutput;
use crate::output::OutputGap;
use crate::output::OutputOmission;

// Even with four full-width u64 counts per notice, this adds less than 1 KiB.
const MAX_POSITIONED_NOTICES: usize = 4;

/// Renders the output since the preceding observation, using that request's
/// own budget. The terminal error participates in the same budget as text
/// emitted by the script; the response still carries its error status separately.
///
/// Image fidelity is preserved here. Model-specific image detail selection
/// still belongs to the caller and does not affect the truncation budget.
///
/// This is the same output Core would render as long as the session collector
/// has retained the whole observation. Collector omissions stay outside this
/// budget: Core's warning describes the retained text alone. The shared
/// formatter moves collector boundaries along with their source content.
/// If losses are too scattered for individual notices, report their totals once.
pub(crate) fn render_output(
    output: CollectedOutput,
    max_output_tokens: Option<usize>,
) -> Vec<OutputItem> {
    let CollectedOutput { items, gaps } = output;
    if items.iter().any(|item| {
        matches!(
            item,
            OutputItem::InputTextBatch { .. }
                | OutputItem::InputText {
                    max_tokens: Some(_),
                    ..
                }
        )
    }) {
        return render_grouped_output(items, gaps, max_output_tokens);
    }
    let mut boundaries: Vec<_> = gaps.iter().map(|gap| gap.boundary).collect();
    let mut content = Vec::with_capacity(items.len());
    for item in items {
        match item {
            OutputItem::InputText { text, .. } => {
                content.push(FunctionCallOutputContentItem::InputText { text });
            }
            OutputItem::InputTextBatch { .. } => {
                unreachable!("grouped emissions use their own budget path")
            }
            OutputItem::InputImage { image_url, detail } => {
                content.push(FunctionCallOutputContentItem::InputImage {
                    image: ImageReference::Inline { image_url },
                    detail: detail.map(|detail| match detail {
                        ImageDetail::Auto => ProtocolImageDetail::Auto,
                        ImageDetail::Low => ProtocolImageDetail::Low,
                        ImageDetail::High => ProtocolImageDetail::High,
                        ImageDetail::Original => ProtocolImageDetail::Original,
                    }),
                });
            }
            OutputItem::InputAudio { audio_url } => {
                content.push(FunctionCallOutputContentItem::inline_audio(audio_url));
            }
        }
    }
    let max_tokens = max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL);
    // Core budgets mixed output in order. Keep that policy even if the
    // collector removed every image or audio item in the observation.
    let formatted = if gaps
        .iter()
        .any(|gap| gap.omitted.image_items > 0 || gap.omitted.audio_items > 0)
    {
        mixed_output_at_token_boundaries(
            content,
            max_tokens,
            estimate_tool_audio_tokens_without_cache,
            &mut boundaries,
        )
    } else {
        formatted_output_at_token_boundaries(
            content,
            max_tokens,
            estimate_tool_audio_tokens_without_cache,
            &mut boundaries,
        )
    };
    insert_loss_notices(formatted, boundaries, gaps)
}

/// Max-min fair allocations, capped by demand, in original section order.
fn apportion(demands: &[usize], mut available: usize) -> Vec<usize> {
    let mut order: Vec<_> = (0..demands.len()).collect();
    order.sort_by_key(|&index| demands[index]);
    let mut budgets = vec![0; demands.len()];
    for (rank, index) in order.into_iter().enumerate() {
        let budget = demands[index].min(available / (demands.len() - rank));
        budgets[index] = budget;
        available -= budget;
    }
    budgets
}

fn render_grouped_output(
    items: Vec<OutputItem>,
    gaps: Vec<OutputGap>,
    max_output_tokens: Option<usize>,
) -> Vec<OutputItem> {
    use codex_utils_output_truncation::TruncationPolicy;
    use codex_utils_output_truncation::approx_token_count;
    use codex_utils_output_truncation::formatted_truncate_text;

    let max_tokens = max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL);
    let audio_tokens = items.iter().fold(0usize, |total, item| {
        let OutputItem::InputAudio { audio_url } = item else {
            return total;
        };
        let cost = estimate_tool_audio_tokens_without_cache(&AudioReference::Inline {
            audio_url: audio_url.clone(),
        });
        if cost <= max_tokens.saturating_sub(total) {
            total + cost
        } else {
            total
        }
    });
    let demands: Vec<_> = items
        .iter()
        .map(|item| {
            let (demand, cap) = match item {
                OutputItem::InputText { text, max_tokens } => {
                    (approx_token_count(text), max_tokens)
                }
                OutputItem::InputTextBatch {
                    sections,
                    max_tokens,
                } => (
                    sections.iter().fold(0usize, |total, text| {
                        total.saturating_add(approx_token_count(text))
                    }),
                    max_tokens,
                ),
                _ => return 0,
            };
            demand.min(
                cap.and_then(|cap| usize::try_from(cap).ok())
                    .unwrap_or(usize::MAX),
            )
        })
        .collect();
    let budgets = apportion(&demands, max_tokens.saturating_sub(audio_tokens));
    let mut audio_available = max_tokens;
    let mut boundaries: Vec<_> = gaps.iter().map(|gap| gap.boundary).collect();
    let mut rendered = Vec::with_capacity(items.len());
    let mut next_gap = 0;
    let item_count = items.len();
    // TELOS(recoverable-output): allocate one emission cap across its sections, never per section.
    for (index, (item, budget)) in items.into_iter().zip(budgets).enumerate() {
        let start = rendered.len();
        let mut inner = Vec::new();
        let mut inner_indices = Vec::new();
        while let Some(gap) = gaps.get(next_gap)
            && gap.boundary.item_index == index
        {
            if gap.boundary.text_offset == 0 {
                boundaries[next_gap] = OutputBoundary {
                    item_index: start,
                    text_offset: 0,
                };
            } else {
                inner_indices.push(next_gap);
                inner.push(OutputBoundary {
                    item_index: 0,
                    text_offset: gap.boundary.text_offset,
                });
            }
            next_gap += 1;
        }
        match item {
            OutputItem::InputText { text, max_tokens } => {
                let formatted = formatted_output_at_token_boundaries(
                    vec![FunctionCallOutputContentItem::InputText { text }],
                    budget,
                    estimate_tool_audio_tokens_without_cache,
                    &mut inner,
                );
                rendered.extend(formatted.into_iter().map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text } => {
                        OutputItem::InputText { text, max_tokens }
                    }
                    _ => unreachable!("text formatting preserves the content kind"),
                }));
            }
            OutputItem::InputTextBatch {
                sections,
                max_tokens,
            } => {
                let demands: Vec<_> = sections
                    .iter()
                    .map(|text| approx_token_count(text))
                    .collect();
                let budgets = apportion(&demands, budget);
                let sections = sections
                    .into_iter()
                    .zip(budgets)
                    .map(|(text, budget)| {
                        formatted_truncate_text(&text, TruncationPolicy::Tokens(budget))
                    })
                    .collect();
                rendered.push(OutputItem::InputTextBatch {
                    sections,
                    max_tokens,
                });
            }
            OutputItem::InputAudio { audio_url } => {
                let cost = estimate_tool_audio_tokens_without_cache(&AudioReference::Inline {
                    audio_url: audio_url.clone(),
                });
                if cost <= audio_available {
                    audio_available -= cost;
                    rendered.push(OutputItem::InputAudio { audio_url });
                } else {
                    rendered.push(OutputItem::InputText {
                        text: "[omitted 1 audio items ...]".to_owned(),
                        max_tokens: None,
                    });
                }
            }
            item @ OutputItem::InputImage { .. } => rendered.push(item),
        }
        for (gap_index, mut boundary) in inner_indices.into_iter().zip(inner) {
            boundary.item_index += start;
            boundaries[gap_index] = boundary;
        }
    }
    for (boundary, gap) in boundaries.iter_mut().zip(&gaps) {
        if gap.boundary.item_index >= item_count {
            *boundary = OutputBoundary {
                item_index: rendered.len(),
                text_offset: 0,
            };
        }
    }
    insert_code_mode_loss_notices(rendered, boundaries, gaps)
}

fn insert_loss_notices(
    formatted: Vec<FunctionCallOutputContentItem>,
    boundaries: Vec<OutputBoundary>,
    gaps: Vec<OutputGap>,
) -> Vec<OutputItem> {
    insert_code_mode_loss_notices(
        formatted.into_iter().map(into_code_mode_output).collect(),
        boundaries,
        gaps,
    )
}

fn insert_code_mode_loss_notices(
    mut formatted: Vec<OutputItem>,
    boundaries: Vec<OutputBoundary>,
    gaps: Vec<OutputGap>,
) -> Vec<OutputItem> {
    if gaps.is_empty() {
        return formatted;
    }
    // One extra slot detects when an observation-wide summary is needed.
    let mut notices = [((0, 0), OutputOmission::default()); MAX_POSITIONED_NOTICES + 1];
    let mut notice_count = 0;
    let mut total = OutputOmission::default();
    let mut summary_position = (formatted.len(), 0);
    for (gap, boundary) in gaps.into_iter().zip(boundaries) {
        total.combine(gap.omitted);
        let OutputBoundary {
            item_index,
            text_offset,
        } = boundary;
        // The end of a text item and the start of its successor are one position.
        let position = if text_offset != 0
            && let Some(OutputItem::InputText { text, .. }) = formatted.get(item_index)
            && text_offset == text.len()
        {
            (item_index + 1, 0)
        } else {
            (item_index, text_offset)
        };
        // The collector can split one original text item. Even an observation-wide
        // summary must keep its two fragments from reading as contiguous text.
        if gap.boundary.text_offset != 0 {
            summary_position = position;
        }
        if notice_count == notices.len() {
            continue;
        }
        if let Some((last, omitted)) = notices[..notice_count].last_mut()
            && *last == position
        {
            omitted.combine(gap.omitted);
        } else {
            notices[notice_count] = (position, gap.omitted);
            notice_count += 1;
        }
    }
    let scope = if notice_count == notices.len() {
        notices[0] = (summary_position, total);
        notice_count = 1;
        "across this observation, "
    } else {
        ""
    };
    // Drain from the end so earlier positions stay valid as text is split.
    let mut reversed = Vec::with_capacity(formatted.len() + 2 * notice_count);
    for &((index, offset), omission) in notices[..notice_count].iter().rev() {
        let after = index + usize::from(offset != 0);
        reversed.extend(formatted.drain(after..).rev());
        if offset != 0 {
            let OutputItem::InputText { text, max_tokens } = &mut formatted[index] else {
                unreachable!("only text has an inner byte position");
            };
            if offset < text.len() {
                reversed.push(OutputItem::InputText {
                    text: text.split_off(offset),
                    max_tokens: *max_tokens,
                });
            }
        }
        reversed.push(OutputItem::InputText {
            max_tokens: None,
            text: format!(
                "[Code Mode session output limit reached: {scope}omitted {} text bytes; discarded {} text segments, {} images, {} audio items]",
                omission.text_bytes,
                omission.text_items,
                omission.image_items,
                omission.audio_items
            ),
        });
    }
    reversed.extend(formatted.into_iter().rev());
    reversed.reverse();
    reversed
}

fn into_code_mode_output(item: FunctionCallOutputContentItem) -> OutputItem {
    match item {
        FunctionCallOutputContentItem::InputText { text } => OutputItem::InputText {
            text,
            max_tokens: None,
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline { image_url },
            detail,
        } => OutputItem::InputImage {
            image_url,
            detail: detail.map(|detail| match detail {
                ProtocolImageDetail::Auto => ImageDetail::Auto,
                ProtocolImageDetail::Low => ImageDetail::Low,
                ProtocolImageDetail::High => ImageDetail::High,
                ProtocolImageDetail::Original => ImageDetail::Original,
            }),
        },
        FunctionCallOutputContentItem::InputAudio {
            audio: AudioReference::Inline { audio_url },
        } => OutputItem::InputAudio { audio_url },
        FunctionCallOutputContentItem::EncryptedContent { .. }
        | FunctionCallOutputContentItem::InputAudio {
            audio: AudioReference::File { .. },
        }
        | FunctionCallOutputContentItem::InputImage {
            image: ImageReference::File { .. },
            ..
        } => {
            unreachable!("output truncation must not introduce new content types")
        }
    }
}

#[cfg(test)]
#[path = "output_render_tests.rs"]
mod tests;
