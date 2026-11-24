//! Warpaint, the fork's palette engine: a pack paints every color the TUI shows.
//!
//! Widgets paint tokens (grounds, figures, syntax classes, heat steps, thread livery) or
//! upstream's named colors; nothing holds a pack color. The binder resolves both as a frame is
//! flushed, against the pack worn and the terminal's depth, so wearing another pack repaints
//! everything on the next frame, committed transcript cells included. Upstream sees packs as
//! syntax themes: `tui.theme` names one, `/theme` lists and previews them, and the pack worn is
//! the one the active syntax theme was resolved from, or under a theme that is no pack's, such as
//! `Theme::default()`, the one worn before it. See `docs/warpaint.md`.

mod bind;
#[cfg(test)]
pub(crate) mod capture;
mod investiture;
mod pack;
mod paint;
#[cfg(test)]
mod pairs;
mod scopes;

use std::io;
use std::io::Write;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::style::Style;
use syntect::highlighting::Theme;

pub(crate) use pack::DEFAULT as DEFAULT_PACK;
pub(crate) use paint::Bracket;
pub(crate) use paint::Figure;
pub(crate) use paint::Ground;
pub(crate) use paint::HEAT_STEPS;
pub(crate) use paint::heat;
pub(crate) use paint::is_token;
pub(crate) use paint::livery;

use bind::Binder;
use bind::Canvas;
use bind::Depth;
use investiture::Investiture;
use pack::Pack;
use paint::Paint;

/// Whether `color` is a syntax class's token: the scope theme highlighted it.
#[cfg(test)]
pub(crate) fn is_syntax(color: Color) -> bool {
    let Color::Indexed(token) = color else {
        return false;
    };
    matches!(Paint::from_token(token), Some(Paint::Syntax(_)))
}

struct Warpaint {
    /// The syntax theme revision `worn` was read from.
    followed: Option<u64>,
    /// The pack worn, as an index into the embedded packs.
    worn: usize,
    investiture: Investiture,
    binder: Option<((usize, Depth, Canvas), Binder)>,
}

/// Runs `f` on the engine: the process's, or under test the test thread's, as upstream keeps the
/// syntax theme it follows.
fn with_warpaint<R>(f: impl FnOnce(&mut Warpaint) -> R) -> R {
    #[cfg(not(test))]
    {
        use std::sync::LazyLock;
        use std::sync::Mutex;
        use std::sync::PoisonError;

        static WARPAINT: LazyLock<Mutex<Warpaint>> = LazyLock::new(|| Mutex::new(Warpaint::new()));
        f(&mut WARPAINT.lock().unwrap_or_else(PoisonError::into_inner))
    }
    #[cfg(test)]
    {
        thread_local! {
            static WARPAINT: std::cell::RefCell<Warpaint> =
                std::cell::RefCell::new(Warpaint::new());
        }
        WARPAINT.with_borrow_mut(f)
    }
}

impl Warpaint {
    fn new() -> Self {
        Self {
            followed: None,
            worn: pack::DEFAULT_INDEX,
            investiture: Investiture::Unknown,
            binder: None,
        }
    }

    /// The pack worn: the one the active syntax theme was resolved from. A theme that is no
    /// pack's leaves the pack worn before it.
    fn pack(&mut self) -> &'static Pack {
        let revision = crate::render::highlight::syntax_theme_revision();
        if self.followed != Some(revision) {
            self.followed = Some(revision);
            if let Some(worn) = crate::render::highlight::current_syntax_theme()
                .name
                .as_deref()
                .and_then(pack::find)
            {
                self.worn = worn;
            }
        }
        &pack::packs()[self.worn]
    }

    /// The pack worn, published to the terminal where it is shown, with the depth it is shown at
    /// and where the default colors then come from.
    fn invest(&mut self, terminal: &mut impl Write) -> io::Result<(&'static Pack, Depth, Canvas)> {
        let pack = self.pack();
        let depth = Depth::current();
        let canvas = if depth.shows_packs() {
            self.investiture.invest(
                terminal,
                self.worn,
                pack.figure[Figure::Ink],
                pack.ground[Ground::Canvas],
            )?
        } else {
            Canvas::Painted
        };
        Ok((pack, depth, canvas))
    }

    /// The binder for this frame, publishing the pack to the terminal first where it is shown.
    fn binder(&mut self, terminal: &mut impl Write) -> io::Result<&mut Binder> {
        let (pack, depth, canvas) = self.invest(terminal)?;
        let key = (self.worn, depth, canvas);
        if self.binder.as_ref().is_some_and(|(bound, _)| *bound != key) {
            self.binder = None;
        }
        Ok(&mut self
            .binder
            .get_or_insert_with(|| (key, Binder::new(pack, depth, canvas)))
            .1)
    }
}

/// The packs a user may wear, by name.
pub(crate) fn pack_names() -> impl Iterator<Item = &'static str> {
    pack::packs().iter().map(|pack| pack.name)
}

/// TELOS(warpaint): override, fixed config, then stable family rendezvous selection.
/// SHA-256 scores UTF-8 root UUID, NUL, then pack name; highest digest wins.
pub(crate) fn resolved_pack(
    config: Option<&str>,
    family: Option<(&str, Option<&str>)>,
) -> &'static str {
    use sha2::Digest;
    use sha2::Sha256;

    let selected = family
        .and_then(|(_, pack)| pack)
        .or(config)
        .unwrap_or(DEFAULT_PACK);
    if selected != "random" {
        return pack::find(selected).map_or(DEFAULT_PACK, |index| pack::packs()[index].name);
    }
    let Some((root, _)) = family else {
        return DEFAULT_PACK;
    };
    pack_names()
        .max_by_key(|name| {
            let mut hash = Sha256::new();
            hash.update(root.as_bytes());
            hash.update([0]);
            hash.update(name.as_bytes());
            (hash.finalize(), *name)
        })
        .unwrap_or(DEFAULT_PACK)
}

/// The theme a pack highlights with, if `name` is a pack.
pub(crate) fn theme(name: &str) -> Option<Theme> {
    pack::find(name).map(|index| scopes::theme(pack::packs()[index].name))
}

/// The theme the default pack highlights with.
pub(crate) fn default_theme() -> Theme {
    scopes::theme(pack::DEFAULT)
}

/// The colors upstream reads as the terminal's defaults: the pack's ink and canvas where packs
/// are shown, else the terminal's own, if it reported them.
#[cfg(all(unix, not(test)))]
pub(crate) fn default_colors() -> Option<crate::terminal_probe::DefaultColors> {
    with_warpaint(|warpaint| {
        if !Depth::current().shows_packs() {
            return warpaint.investiture.prior();
        }
        let pack = warpaint.pack();
        Some(crate::terminal_probe::DefaultColors {
            fg: pack.figure[Figure::Ink].tuple(),
            bg: pack.ground[Ground::Canvas].tuple(),
        })
    })
}

/// A ground's color in the pack worn, for effects that blend toward it as they animate.
pub(crate) fn ground_rgb(ground: Ground) -> (u8, u8, u8) {
    with_warpaint(|warpaint| warpaint.pack().ground[ground].tuple())
}

/// What a painted color shows as, for effects that blend over a cell: an RGB literal's own
/// color, or a token's in the pack worn.
pub(crate) fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(token) => {
            let paint = Paint::from_token(token)?;
            Some(with_warpaint(|warpaint| warpaint.pack().rgb(paint).tuple()))
        }
        _ => None,
    }
}

/// Records the default colors the terminal reported at startup, or that it reported none.
#[cfg(unix)]
pub(crate) fn note_terminal_colors(colors: Option<crate::terminal_probe::DefaultColors>) {
    with_warpaint(|warpaint| warpaint.investiture = Investiture::note(colors));
}

/// Publishes the pack worn as the terminal's default colors, where packs are shown and the
/// terminal's own can be restored: a frame bound under them paints ink and canvas as `Reset`.
pub(crate) fn invest(terminal: &mut impl Write) -> io::Result<()> {
    with_warpaint(|warpaint| warpaint.invest(terminal).map(|_| ()))
}

/// True when dimmed ink is rendered as a faint pack color rather than a terminal modifier.
pub(crate) fn dimmed_ink_binds_to_faint(foreground: Color) -> bool {
    Depth::current().shows_packs() && bind::is_ink(foreground)
}

/// Binds a rendered frame in place, before it is diffed against the frame on screen.
pub(crate) fn bind_frame(buffer: &mut Buffer, terminal: &mut impl Write) -> io::Result<()> {
    #[cfg(test)]
    capture::witness(buffer);
    with_warpaint(|warpaint| {
        let binder = warpaint.binder(terminal)?;
        for cell in &mut buffer.content {
            (cell.fg, cell.bg, cell.modifier) = binder.bind(cell.fg, cell.bg, cell.modifier);
        }
        Ok(())
    })
}

/// Binds a history line before it is written into the terminal's scrollback.
pub(crate) fn bind_history_line(
    line: &crate::terminal_hyperlinks::HyperlinkLine,
    terminal: &mut impl Write,
) -> io::Result<crate::terminal_hyperlinks::HyperlinkLine> {
    with_warpaint(|warpaint| {
        let binder = warpaint.binder(terminal)?;
        let mut line = line.clone();
        let under = line.line.style;
        line.line.style = binder.bind_style(under, Style::default());
        for span in &mut line.line.spans {
            span.style = binder.bind_style(span.style, under);
        }
        Ok(line)
    })
}

/// Gives the terminal its own colors back; the next frame publishes the pack again.
pub(crate) fn revoke(terminal: &mut impl Write) -> io::Result<()> {
    with_warpaint(|warpaint| warpaint.investiture.revoke(terminal))
}

#[cfg(test)]
mod tests {
    use crate::custom_terminal::Terminal;
    use crate::test_backend::VT100Backend;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::Widget;

    use super::capture;

    #[test]
    fn previewing_and_cancelling_rebinds_pack_style_over_unchanged_frames() {
        // Slag, the default, sets constants bold; eclipse styles no syntax class.
        let slag =
            crate::render::highlight::resolve_theme_by_name("slag", /*codex_home*/ None)
                .expect("slag");
        crate::render::highlight::set_syntax_theme(slag);
        let params = crate::theme_picker::build_theme_picker_params(
            Some("slag"),
            /*codex_home*/ None,
            /*terminal_width*/ None,
        );
        let eclipse = params
            .items
            .iter()
            .position(|item| item.search_value.as_deref() == Some("eclipse"))
            .expect("eclipse is listed");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = crate::app_event_sender::AppEventSender::new(tx);
        let code = crate::render::highlight::highlight_code_to_lines("let ready = true;", "rust");
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 20, /*height*/ 1,
        );
        let mut terminal =
            Terminal::with_options(VT100Backend::new(area.width, area.height)).expect("terminal");
        terminal.set_viewport_area(area);
        // The frame as painted, and which of `let` and `true` the bound frame shows bold.
        let mut draw = || {
            let (drawn, source) = capture::unbound(|| {
                terminal.draw(|frame| {
                    Paragraph::new(code.clone()).render(frame.area(), frame.buffer_mut());
                })
            });
            drawn.expect("draw");
            let bold = |columns: std::ops::Range<u16>| {
                columns
                    .map(|x| {
                        terminal.previous_buffer()[(x, 0)]
                            .modifier
                            .contains(Modifier::BOLD)
                    })
                    .collect::<Vec<_>>()
            };
            (source, bold(0..3), bold(12..16))
        };

        let (worn, keyword, constant) = draw();
        assert_eq!((keyword, constant), (vec![false; 3], vec![true; 4]));
        (params.on_selection_changed.as_ref().expect("previews"))(eclipse, &tx);
        let (previewed, keyword, constant) = draw();
        assert_eq!((keyword, constant), (vec![false; 3], vec![false; 4]));
        (params.on_cancel.as_ref().expect("cancels"))(&tx);
        let (restored, keyword, constant) = draw();
        assert_eq!((keyword, constant), (vec![false; 3], vec![true; 4]));
        assert_eq!((&previewed, &restored), (&worn, &worn));
    }
}
