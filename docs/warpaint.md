# Warpaint

Warpaint paints every color the TUI shows from one data pack. Widgets paint tokens, never
pack colors; the binder resolves every cell as the frame is flushed.

## Vocabulary

`codex-rs/tui/src/warpaint/paint.rs` declares three families: grounds, what a cell is
painted on; figures, text and glyphs painted over a ground; and syntax classes, which only the
scope theme names. One macro per family emits the enum, the table a pack must fill and the
family's token range. Three ramps complete a pack: `livery` for thread identities, `heat` for
activity, and `terminal`, tool output's sixteen colors in xterm order.

`warpaint/pairs.rs` declares every ground each figure, syntax class and ramp is painted on;
whatever the scope theme paints is declared over code's grounds as well, since highlighted
code lands there. Its `roles_json_matches_the_declaration` test writes the vocabulary and those
pairs to `scripts/warpaint/roles.json`, and the generator reads nothing else, so the vocabulary
has one source. A call site that paints a figure on a new ground extends its list first.

## Binding

A token is a `Color::Indexed` value in a reserved range. Cells hold tokens, so no cell, not
even a committed transcript cell, ever holds a pack color, and wearing another pack repaints
everything on the next frame. `custom_terminal.rs` binds each frame in place before diffing
it against the screen; `insert_history.rs` binds history lines before writing them. A bound
frame never meets the binder again: at 256 colors its xterm indices can equal tokens, so the
editor handoff repaints the frame on screen through `Terminal::replay`, which first publishes
the pack's ink and canvas again over the default colors the handoff gave back. The binder
(`warpaint/bind.rs`) resolves:

- tokens to the pack's colors, exact in truecolor and the nearest fixed xterm color at 256;
- upstream's named chrome colors to the figures they stand for: red to failure, green to
  success, yellow to caution, blue to link, magenta to accent, cyan to info, gray, dark gray
  and white to the ink ladder; a black background to the trough;
- green and red text over a diff line's ground to the diff figures, which are proven there,
  rather than to success and failure;
- dimmed ink to the faint ink, so the proof rather than the terminal decides how faint;
- tool output, which `codex-ansi-escape` quarantines at ingestion: its palette colors become
  indices below 16, resolved through the terminal ramp, and its extended colors become RGB;
- RGB literals, unchanged in truecolor and quantized at 256;
- the pack's style: a syntax class's modifiers, bold or italic, added wherever the class is a
  cell's foreground, at every depth and over the modifiers the cell and the depth already
  carry. Captures are taken before binding, so they never carry style, and a preview or cancel
  restyles everything on the next frame as it recolors it. A terminal that draws bold as bright
  may brighten a bold class at sixteen colors.

At sixteen colors the terminal's palette is the user's: tokens become the ANSI color or
modifier upstream would have painted, and nothing else changes. No other code paints an
indexed color: `best_color` and contrast correction return RGB at 256 and leave quantization
to the binder. Upstream's light-terminal diff gutter still paints fixed indices, one of them
inside the livery's range; every pack is dark, so it is never drawn.

## Investiture

While a pack is worn its ink and canvas are the terminal's default colors, so window padding,
cleared rows and tool output's resets match the page. They are published over OSC 10 and 11
with the first frame after startup reports the terminal's own colors, and those colors are
restored whenever the TUI lets go of the screen. A terminal that reports no colors cannot be
restored and is never invested; the binder paints ink and canvas into every cell instead. A
process that re-executes itself restores them first, so its successor reads the terminal's
own. Where packs are not shown, upstream reads the terminal's own colors as the defaults.

## Upstream seams

Upstream sees packs as syntax themes. `[tui] theme` names a pack; `render/highlight.rs`
resolves theme names to packs, and a pack's scope theme maps TextMate scopes to tokens, so
syntax colors bind like everything else; `/theme` lists, previews and restores packs through
upstream's picker. The pack worn is the one the active syntax theme was resolved from; a theme
that is no pack's, such as upstream's `Theme::default()`, leaves the pack worn before it.
Warpaint follows upstream's theme revision rather than being told, so lazy initialization,
previews and cancel-restore cannot leave the two apart. `terminal_palette.rs` reports the
pack's ink and canvas as the default colors.

Upstream's palette helpers still compute their colors and decide, as upstream does, whether
to paint at all. Where one settles on a color of its own, an RGB or a fixed index, the role's
token replaces it (`Figure::repaint`, `Ground::repaint`); where it degrades to the terminal's
palette, with the default colors unknown or at sixteen colors, its degradation stands. So
`style.rs` repaints its fills, accent, secondary and selection; `diff_render.rs` its two line
backgrounds; the activity chart replaces a gradient it would show with the heat ramp; and
`readable_color_on` passes tokens and the ink through, since packs prove both, except that at
an unknown color depth, where upstream paints no computed color, it paints no token either.
Effects that blend over a painted cell, such as the composer's sparkle, read a token's color
through `warpaint::rgb`. Thread colors come from `warpaint/livery.rs`, mounted as
`thread_color`. RGB literals that upstream or the fork's own chrome paints, such as the session
header's, are outside the pack.

## Pipeline

`docs/diagrams/warpaint.mmd` draws the whole arrangement. The fork's terminal renderer
draws a smaller flowchart grammar than Mermaid's and rejects `subgraph`, so render it with
`scripts/mermaid-render docs/diagrams/warpaint.mmd --pack <pack>`.

| Artifact | Role |
|---|---|
| `scripts/warpaint/specs/<pack>.toml` | the spec: ground, ink, voice, authored hues, livery; ranks, gain, surfaces, terminal floor and style where it restates them |
| `scripts/warpaint/smelt.py` | the gate and generator: spec to pack and proof; a uv script locked by `smelt.py.lock` |
| `scripts/warpaint/appearance.py` | the appearance frame: CAM16, APCA, dichromacy |
| `scripts/warpaint/laws.py` | the doctrine and every law, from spec to roles and ramps |
| `scripts/warpaint/proof.py` | every declared pair, distinctness rule and ramp law, checked |
| `scripts/warpaint/vocabulary.py` | the vocabulary and pairs, read from `roles.json` |
| `codex-rs/tui/src/warpaint/packs/<pack>.toml` | the generated pack |
| `scripts/warpaint/proofs/<pack>.md` | every role's appearance and 256-color projection, Lc over every declared ground, distinctness |
| `scripts/warpaint/swatch.py`, `flip.py` | render a capture, a chip sheet, a contact sheet or a flip page under any pack |
| `scripts/warpaint/captures/<frame>.json` | live frames of a fabricated conversation, exported before binding by `just warpaint-capture` |

A spec makes the decisions that give a palette its character and nothing else: the ground's
hue, colorfulness and lightness; the ink's hue, tint and contrast; the voice's colorfulness,
`lean` and `naturalism`; and the hue of each authored role. Every color is derived from
them by law.

A spec may restate how its code speaks and how its surfaces are built, and its proof states
each restatement beside the doctrine's value:

| table | restates |
|---|---|
| `[ranks.<name>]` | syntax classes `members` re-levelled out of the voice: read at `lc` over their own ground where the voice reads at 55, asking `gain` times the voice's colorfulness, with their own `naturalism`. `least` is the dE the rank's voice classes stand apart, 9 unless stated; at 0 the rank is one tone, told from the others by level alone. |
| `[voice] least` | the dE the voice classes no rank holds stand apart, 9 unless stated |
| `[gain]` | each family's colorfulness: `signal`, `diff`, `terminal`, `comment` and `code` as multiples of the voice's (1, 1, 1, 0.4, 0.6), `punctuation` and `variable` of the ink's (1.5, 2) |
| `[surfaces]` | the elevations `trough`, `dais` and `selection_rise` (-3, 5, 9 J'), and the tints `band_tint`, `selection_tint` and `diff_tint` (2.8, 9, 7 M) |
| `[terminal] least` | the dE tool output's hues, and their brights, stand apart (10) |
| `[style]` | modifiers, `bold` or `italic`, for syntax classes; the pack carries them for the binder |

Colors are constructed in the Hellwig-Fairchild 2022 revision of CAM16 with the
Hellwig-Stolitzka-Fairchild Helmholtz-Kohlrausch term, as apparent lightness, colorfulness and
hue, under fixed viewing conditions: a D65 panel at 100 cd/m² in a dim room, a dark interface
as the adapting field, and an IPS panel's glow at black added to every color it shows.
Distances are CAM16-UCS; legibility is APCA-W3 0.98G, which unlike the WCAG 2.x ratio reads
light text on a dark ground correctly.

- Surfaces are the canvas's material raised or recessed in equal perceptual steps at constant
  hue and saturation. The prompt, reasoning, aside and subagent bands and the selection bar are
  that material tinted toward a hue; diff bands take their ink's hue outright. A spec may author
  the reasoning band's hue; otherwise the law takes the hue inside caution's window farthest
  from every other surface.
- The ink ladder is solved to APCA tiers. Every text role is lifted until it clears its floor
  over every ground the TUI paints it on; `composition` in `laws.py` holds the floors, the TUI's
  declaration in `roles.json` names the pairs, and the proof checks every declared pair. A
  declared pair without a floor is a fault.
- Chromatic roles speak in one voice, rings at one apparent lightness each: a text ring for
  the accent, links, diff ink and syntax, a deeper signal ring for status, which the TUI paints
  bold, and a terminal ring for tool output's six hues, whose brights stand above it. A ring is lifted as a whole until its worst composition clears its floor.
  `lean` concentrates colorfulness on the accent; `naturalism` lets each hue leave the common
  lightness toward its own natural lightness, so 0 is a strict iso-lightness ring. Status and
  diff colors are exempt from `lean`: character may not quiet a meaning.
- Colorfulness yields to the sRGB gamut; lightness and hue never do, and the proof reports
  every yield.
- Status and diff hues stay inside windows that carry learned meaning, and must stay apart for
  protan and deutan observers as well as trichromats. Diff ink defaults to lime against rose,
  the pair that survives dichromacy.
- Syntax classes at one level stand 9 dE apart, or a rank's or the voice's stated least; classes
  at different levels stand 9 apart or 5 Lc apart over the canvas, told apart by level.
- Inline code is solved alone at a fraction of the voice's colorfulness and set apart from
  prose: 10 dE from the ink, or beneath it by value, 5 Lc below it over the canvas. Comments,
  punctuation and variables stand 5 dE apart, unless both sit beneath prose by value, where
  they may share a tone.
- Heat rises from the faintest mark that clears the rule's floor to the accent, in steps the
  eye can tell apart.
- Tool output's sixteen colors keep their meaning, red is red and cyan is cyan, but are solved
  against the pack's grounds; the neutrals are the pack's own grounds and inks.
- The livery maximizes its least pairwise distance and keeps away from the status colors.
- The canvas may be tinted, analogous to the accent or not, up to a colorfulness ceiling that
  admits the canonical dark grounds.

`smelt.py --check` fails on a fault: a ground, contrast, window or distinctness law that a pack breaks.

## Changing a palette

Edit the spec, run `scripts/warpaint/smelt.py`, re-swatch, and look at the result. A generated
pack must not be edited: the value would be unprovable and the next smelt would overwrite it.
`just warpaint-check` fails when a pack or proof has drifted from its spec or a pack breaks the
doctrine. When the TUI's painting changes, `just warpaint-capture` re-exports the captures.

A new pack is a spec file and an entry in `EMBEDDED` in `warpaint/pack.rs`; the
`embedded_packs_are_the_pack_directory` test fails until both exist. Eight packs ship:
crucible, eclipse, magnetar, prominence, shadow-play, slag, sodium and uranium. The default is
`slag`; a default that names no embedded pack fails the build, and an embedded pack that fails
to parse stops the TUI at startup rather than leave it wearing a pack nobody chose.
