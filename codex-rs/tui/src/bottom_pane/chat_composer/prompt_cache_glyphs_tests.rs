use super::super::*;
use chrono::Local;
use chrono::TimeZone;
use codex_app_server_protocol::PromptCacheEstimate;
use codex_app_server_protocol::PromptCacheWarming;
use tokio::sync::mpsc::unbounded_channel;

fn rows(
    composer: &ChatComposer,
    width: u16,
    prompt_cache: PromptCacheGlyphs,
    reserve: u16,
) -> String {
    let options = composer.resolve_render_options(ComposerRenderOptions {
        prompt_cache,
        textarea_right_reserve: reserve,
        ..Default::default()
    });
    let area = Rect::new(
        /*x*/ 0,
        /*y*/ 0,
        width,
        composer.desired_height_with_options(width, options),
    );
    let mut buffer = Buffer::empty(area);
    composer.render_with_options(area, &mut buffer, /*mask_char*/ None, options);
    buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// The radiator and its deadline precede the snowflake in the bottom padding row, and both
// step left of the ambient pet's reserve.
#[test]
fn warming_and_cold_glyphs_share_the_bottom_padding_row() {
    let (tx, _rx) = unbounded_channel();
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        AppEventSender::new(tx),
        /*enhanced_keys_supported*/ false,
        "Ask Codex".into(),
        /*disable_paste_burst*/ true,
    );
    composer.set_text_content("keep this draft".into(), Vec::new(), Vec::new());
    let ends_at = Local
        .with_ymd_and_hms(
            /*year*/ 2026, /*month*/ 10, /*day*/ 9, /*hour*/ 3,
            /*min*/ 12, /*sec*/ 0,
        )
        .unwrap()
        .timestamp();
    let glyphs = PromptCacheGlyphs {
        estimate: PromptCacheEstimate::Cold,
        warming: PromptCacheWarming::Holding { ends_at },
    };
    insta::assert_snapshot!(format!(
        "{}\n---\n{}",
        rows(&composer, /*width*/ 48, glyphs, /*reserve*/ 0),
        rows(&composer, /*width*/ 48, glyphs, /*reserve*/ 8)
    ));
}
