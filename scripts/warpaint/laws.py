"""The laws: every role, ramp and ring derived from a spec's decisions, and the doctrine the
proof holds them to. A spec states a palette's character in perceptual terms: the ground it is
painted on, the contrast of its ink, which has no hue, the hues it speaks in and the shape of its
voice; no color is chosen by hand at a call site or in a pack.
"""

from __future__ import annotations

import itertools
import tomllib
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from appearance import (
    Swatch,
    apca,
    cusp,
    distance,
    elevated,
    least_lightness,
    paint,
    tinted,
    ucs,
    yielding,
)
from vocabulary import (
    HEAT_STEPS,
    PAIRS,
    TERMINAL_SLOTS,
    Bracket,
    Figure,
    Ground,
    Paint,
    Syntax,
)

type Arc = tuple[float, float]
type Floors = tuple[tuple[Paint, float], ...]


# -- Doctrine ------------------------------------------------------------------------------

GROUND_CEILING = 7.0
"""Most colorfulness a canvas may carry. A colorful ground pulls adaptation toward itself and
every same-hue role is judged against the adapted state. In this frame the canonical dark
grounds sit between 0.3 and 6; above the ceiling the page stops being a ground."""

INK_MUTED = 0.74
INK_FAINT = 0.55
"""Secondary and tertiary ink as fractions of the ink's Lc: 84 gives 62 and 46, APCA's
content-text and least-text tiers."""
VOICE = 55.0
"""Lc of every chromatic role read as text over its own ground: the accent, links, diff ink and
code."""
SIGNAL = 47.0
"""Lc of status colors, which the TUI paints bold or as glyphs. A red that reaches the text
floor on a dark ground is pink, so the signal ring stands lower and stays red."""
COMMENT = 41.0
RULE = 16.0

DAIS = 5.0
DIFF_BAND = 1.5
SELECTION_RISE = 9.0
PROMPT_RISE = 9.5
"""Elevations in CAM16-UCS J' from the canvas. A surface is the ground's material raised at
constant hue and saturation, so a lit surface reads as the same paint in more light rather than as
a different paint. A spec's `[surfaces]` may move the dais, the selection bar and the prompt."""

SELECTION_TINT = 9.0
"""Colorfulness the selection bar adds toward the accent, as a vector in the appearance plane: the
bar keeps the colors of the row it selects, so it is a lit band of the ground and never a light
chip. A spec's `[surfaces]` may move it."""
DIFF_TINT = 7.0
"""Colorfulness of a diff band, which takes its ink's hue outright: a removed line must read as
the removed hue on every ground, not as that hue mixed with the page. A spec's `[surfaces]`
may move it."""
PROMPT_TINT = 2.25
"""Colorfulness the prompt ground adds toward the prompt bracket's hue, as a vector in the
appearance plane: the composer and the panels that open in its place are the user's own surface,
the canvas's material lit and leaning toward the color of what the user says. A spec's
`[surfaces]` may move it."""

LABEL_CEILING = 95.0
"""Most Lc a label may reach over the canvas. Bold already carries a label's prominence, and
pure white reads near Lc 107 on these canvases."""
LABEL_TINT = 9.0
"""A label's colorfulness unless its spec states one: a pale tint of the accent, never white and
never a second accent."""
LABEL_DROP = 3.0
"""How far beneath the ink's contrast a label reads unless its spec states otherwise; bold carries
the rest of its prominence."""
TAB_TINT = 15.0
"""An open disclosure tab's colorfulness unless its spec states one: deeper than the label's tint,
so the tab reads as the accent's hue rather than as a second label."""
TAB_LEVEL = 0.5
"""Where an open tab's contrast falls between the faint ink of the tabs beside it (0) and the label
(1), unless its spec states its contrast."""
VARIABLE_TINT = 6.0
"""A variable's colorfulness unless a rank holds it. Punctuation no rank holds is achromatic, as
the ink is: plain text never carries a palette's hue."""

LIVERY_RISE = 10.0
"""J_HK an identity may climb above the least lightness that clears the voice floor."""
LIVERY_LEAST = 10.0
"""Least dE between two identities."""
REPELLERS = {Figure.FAILURE: 1.6, Figure.CAUTION: 0.9, Figure.SUCCESS: 0.9}
"""How far, in multiples of LIVERY_LEAST, an identity keeps from each status color: an agent
painted near failure reads as an agent that failed."""

BRACKET_HUES: dict[Paint, float] = {
    Bracket.REASONING: 30.0,
    Bracket.CALL: 95.0,
    Bracket.MESSAGE: 125.0,
    Bracket.RESPONSE: 150.0,
    Bracket.PROMPT: 240.0,
    Bracket.HARNESS: 300.0,
}
"""The hue each bracket is chosen nearest, in the order the brackets go round the circle. The
model's span runs from green, its words to the user, through yellow toward red as its words fall
farther from the user: to other agents, to its tools, to itself. The user's span runs from blue,
the user's own words, to purple, what the harness says in the user's stead."""
BRACKET_COLORFUL = 12.0
"""Least M of an identity a bracket may take: a paler one reads as ink, not as a speaker."""

WINDOWS: dict[Paint, Arc] = {
    Figure.FAILURE: (345.0, 30.0),
    Figure.CAUTION: (70.0, 105.0),
    Figure.SUCCESS: (140.0, 175.0),
    Figure.INFO: (200.0, 280.0),
    Figure.ADDED: (115.0, 175.0),
    Figure.REMOVED: (335.0, 10.0),
}
"""Hue windows that carry learned meaning. A palette chooses inside them, never outside; the
dichromat floors below then decide which choices survive."""

RUNNING: Arc = (105.0, 180.0)
"""Green, from its yellow edge to teal, says work is running: an active thread's dot wears it. No
heading may, neither a settled entry's label nor the open tab beneath it, or a finished feed reads
as one still at work. The Response heading is the exception: it wears its bracket's green, which
reads as the speaker, not as work."""

DIFF_HUES = {Figure.ADDED: 125.0, Figure.REMOVED: 345.0}
"""Lime against rose: the pair a protan or deutan still tells apart, ink and band alike, because
it runs along the blue-yellow axis that survives dichromacy. A spec may move either inside its
window."""


SURFACES = (
    Ground.CANVAS,
    Ground.DAIS,
    Ground.SELECTION,
    Ground.PROMPT,
    Ground.ADDED,
    Ground.REMOVED,
)
SURFACE_LEAST = 2.0
"""Least dE between two surfaces."""


@dataclass(frozen=True, slots=True)
class Distinct:
    label: str
    roles: tuple[Paint, ...]
    least: float
    dichromat: float | None = None
    beneath: bool = False
    """Whether a pair nearer than `least` still stands apart where every member but the ink sits
    beneath prose by value, VALUE_STEP or more below the ink over the canvas."""
    ranked: bool = False
    """Whether ranks partition the rule: it holds among the classes no rank holds at the voice's
    least, each rank holds its own classes to the rank's least, and classes of different ranks
    stand `least` apart or VALUE_STEP apart in Lc, told apart by level."""


SYNTAX_LEAST = 9.0
"""Least dE between two syntax classes the voice speaks at one level; a spec's voice or rank
may state its own."""
VALUE_STEP = 5.0
"""Lc over the canvas that tells two colors apart by value alone. Inline code that far below the
ink is set apart from prose, recessive classes that far below it may share a tone, and syntax
classes of different ranks that far apart in Lc are told apart by level."""

DISTINCT = (
    Distinct(
        "status",
        (Figure.SUCCESS, Figure.CAUTION, Figure.FAILURE, Figure.INFO),
        14.0,
        6.0,
    ),
    Distinct(
        "accent against status",
        (Figure.ACCENT, Figure.FAILURE, Figure.CAUTION, Figure.SUCCESS),
        10.0,
    ),
    Distinct("diff ink", (Figure.ADDED, Figure.REMOVED), 18.0, 9.0),
    Distinct("diff band", (Ground.ADDED, Ground.REMOVED), 6.0, 3.0),
    Distinct(
        "syntax",
        (
            Syntax.KEYWORD,
            Syntax.FUNCTION,
            Syntax.TYPE,
            Syntax.STRING,
            Syntax.NUMBER,
            Syntax.ATTRIBUTE,
        ),
        SYNTAX_LEAST,
        ranked=True,
    ),
    Distinct(
        "code recession",
        (Syntax.COMMENT, Syntax.PUNCTUATION, Syntax.VARIABLE),
        5.0,
        beneath=True,
    ),
    Distinct("surfaces", SURFACES, SURFACE_LEAST),
    Distinct("ink ladder", (Figure.INK, Figure.INK_MUTED, Figure.INK_FAINT), 8.0),
    Distinct("code against prose", (Figure.CODE, Figure.INK), 10.0, beneath=True),
)

SYNTAX_VOICE = (
    Syntax.KEYWORD,
    Syntax.FUNCTION,
    Syntax.TYPE,
    Syntax.STRING,
    Syntax.NUMBER,
    Syntax.CONSTANT,
    Syntax.ATTRIBUTE,
)
TEXT_RING = (Figure.ACCENT, Figure.LINK, Figure.ADDED, Figure.REMOVED, *SYNTAX_VOICE)
SIGNAL_RING = (Figure.SUCCESS, Figure.CAUTION, Figure.FAILURE, Figure.INFO)
UNLEANED = frozenset((*SIGNAL_RING, Figure.ADDED, Figure.REMOVED))
"""Roles that carry an alarm or a diff keep the voice's full colorfulness: lean is character,
and character may not quiet a meaning."""
AUTHORED = (
    *TEXT_RING,
    *SIGNAL_RING,
    Syntax.COMMENT,
    Syntax.PUNCTUATION,
    Syntax.VARIABLE,
    Figure.CODE,
)


def composition(spec: Spec) -> dict[Paint, Floors]:
    """Every ground the TUI paints each text role on, with the Lc it must reach there. The solver
    lifts each role to this table and the proof checks it, so the two cannot disagree. A rank's
    classes take code's floors at the rank's Lc in place of their own; the label takes the
    ink's at its own contrast, wherever it is painted."""
    body = spec.ink_contrast
    secondary, tertiary = body * INK_MUTED, body * INK_FAINT

    def ladder(tier: float) -> Floors:
        return (
            (Ground.CANVAS, tier),
            (Ground.DAIS, tier - 4),
            (Ground.PROMPT, tier - 4),
            (Ground.SELECTION, tier - 10),
        )

    code: Floors = (
        (Ground.CANVAS, VOICE),
        (Ground.PROMPT, VOICE - 4),
        (Ground.ADDED, VOICE - 4),
        (Ground.REMOVED, VOICE - 4),
    )

    def banded(floors: Floors, floor: float) -> Floors:
        """`floors` and both diff bands at `floor`: highlighted code carries the role into diffs."""
        return (*floors, (Ground.ADDED, floor), (Ground.REMOVED, floor))

    def ink(tier: float) -> Floors:
        return (*ladder(tier - 1), (Ground.ADDED, tier - 6), (Ground.REMOVED, tier - 6))

    table = {
        Figure.INK: ink(body),
        Figure.LABEL: tuple(
            (g, f) for g, f in ink(spec.label.contrast) if g in PAIRS[Figure.LABEL]
        ),
        Figure.TAB: tuple(
            (g, f) for g, f in ink(spec.tab.contrast) if g in PAIRS[Figure.TAB]
        ),
        Figure.INK_MUTED: ink(secondary),
        Figure.INK_FAINT: ink(tertiary),
        Figure.INK_SELECTED: ((Ground.SELECTION, body - 4),),
        Figure.RULE: ((Ground.CANVAS, RULE - 1), (Ground.DAIS, RULE - 8)),
        Figure.ACCENT: ladder(VOICE),
        Figure.LINK: banded(ladder(VOICE), VOICE - 4),
        Figure.SUCCESS: ladder(SIGNAL),
        Figure.CAUTION: banded(ladder(SIGNAL), SIGNAL - 4),
        Figure.FAILURE: banded(ladder(SIGNAL), SIGNAL - 4),
        Figure.INFO: ladder(SIGNAL),
        Figure.ADDED: (
            (Ground.ADDED, VOICE),
            (Ground.CANVAS, VOICE),
            (Ground.PROMPT, VOICE - 4),
            (Ground.REMOVED, VOICE - 4),
        ),
        Figure.REMOVED: (
            (Ground.REMOVED, VOICE),
            (Ground.CANVAS, VOICE),
            (Ground.PROMPT, VOICE - 4),
            (Ground.ADDED, VOICE - 4),
        ),
        Figure.CODE: banded(
            (
                (Ground.CANVAS, body - 8),
                (Ground.DAIS, body - 12),
                (Ground.PROMPT, body - 12),
            ),
            body - 12,
        ),
        **{role: code for role in SYNTAX_VOICE},
        **{bracket: ((Ground.CANVAS, VOICE),) for bracket in Bracket},
        Syntax.COMMENT: tuple((g, COMMENT) for g, _ in code),
        Syntax.PUNCTUATION: tuple((g, secondary - 6) for g, _ in code),
        Syntax.VARIABLE: tuple((g, body - 10) for g, _ in code),
    }
    for rank in spec.ranks:
        for member in rank.members:
            table[member] = tuple((g, rank.lc + f - VOICE) for g, f in code)
    return table


RAMP_FLOORS: dict[str, Floors] = {
    "livery": (
        (Ground.CANVAS, VOICE),
        (Ground.DAIS, VOICE - 4),
        (Ground.PROMPT, VOICE - 4),
    ),
    "heat": ((Ground.CANVAS, RULE - 1),),
    "terminal": ((Ground.CANVAS, SIGNAL),),
}
"""Lc of the ramps over the grounds they are painted on. The terminal ring is solved to its
floors, at the signal level because tool output colors words rather than carrying prose; the
livery is solved over the canvas and the first heat step to the rule's floor, and both are
proved over the rest."""

TERMINAL_HUES = (310.0, 195.0)
"""Tool output's magenta and cyan, slots 5 and 6. No authored hue is snapped to: snapping put
magenta on removed ink and cyan on green."""
BRIGHT = 8.0
"""J_HK the bright terminal slots stand above the terminal ring, at the same hue and M asked."""
TERMINAL_LEAST = 10.0
"""Least dE among slots 1-6, and among their brights."""
BRIGHT_LEAST = 5.0
"""Least dE between a terminal slot and its bright."""
HEAT_LEAST = 5.0
"""Least dE between neighbors on the heat ramp, the ground included."""


# -- Spec ----------------------------------------------------------------------------------


class Invalid(ValueError):
    pass


@dataclass(frozen=True, slots=True)
class GroundSpec:
    hue: float
    colorfulness: float
    lightness: float


@dataclass(frozen=True, slots=True)
class InkSpec:
    hue: float
    colorfulness: float
    contrast: float


@dataclass(frozen=True, slots=True)
class VoiceSpec:
    """The shape and level every chromatic text role shares.

    colorfulness is the M asked of the voice. lean concentrates it on the accent,
    M(h) = M (1 + lean cos(h - accent)), everywhere but the alarms and the diff. naturalism
    moves each hue off the common lightness toward its own natural lightness, the J_HK of its
    gamut cusp, by that fraction of its difference from the accent's; 0 is a strict
    iso-lightness ring. lift raises the solved rings by that much J_HK. least is the least dE
    between the syntax classes the voice speaks, those no rank holds."""

    colorfulness: float
    lean: float
    naturalism: float
    lift: float
    least: float


@dataclass(frozen=True, slots=True)
class Rank:
    """Syntax classes re-levelled together, out of the voice: they read at Lc `lc` over their
    own ground where the voice reads at VOICE, ask `gain` times the voice's colorfulness, leave
    the rank's common lightness by `naturalism` as the voice's rings do, and stand `least` dE
    apart. A rank of 0 least is one tone, told from the others by level alone."""

    name: str
    members: tuple[Syntax, ...]
    lc: float
    gain: float
    naturalism: float
    least: float


@dataclass(frozen=True, slots=True)
class Gain:
    """The colorfulness each family asks, as a multiple of the voice's: status, diff ink, tool
    output, comments and inline code."""

    signal: float = 1.0
    diff: float = 1.0
    terminal: float = 1.0
    comment: float = 0.4
    code: float = 0.6


@dataclass(frozen=True, slots=True)
class Surfaces:
    """How the surfaces are built from the canvas: the dais's, selection bar's and prompt ground's
    elevations in J', and the colorfulness the selection bar, the diff bands and the prompt ground
    add."""

    dais: float = DAIS
    selection_rise: float = SELECTION_RISE
    selection_tint: float = SELECTION_TINT
    diff_tint: float = DIFF_TINT
    prompt_rise: float = PROMPT_RISE
    prompt_tint: float = PROMPT_TINT


@dataclass(frozen=True, slots=True)
class Terminal:
    """The least dE among tool output's six hues, and among their brights."""

    least: float = TERMINAL_LEAST


STYLES = ("bold", "italic")
"""The modifiers a pack may give a syntax class; the TUI adds them wherever it binds the class."""


def label_law(ink: float, accent: float) -> InkSpec:
    """The label as the doctrine paints it: the accent's hue at LABEL_TINT, LABEL_DROP beneath the
    ink's contrast, `ink`."""
    return InkSpec(accent, LABEL_TINT, ink - LABEL_DROP)


def tab_law(ink: float, label: InkSpec) -> InkSpec:
    """The open tab as the doctrine paints it: the label's hue at TAB_TINT, TAB_LEVEL of the way
    from the faint ink's contrast to the label's."""
    faint = ink * INK_FAINT
    return InkSpec(label.hue, TAB_TINT, faint + TAB_LEVEL * (label.contrast - faint))


@dataclass(frozen=True, slots=True)
class Spec:
    name: str
    stance: str
    ground: GroundSpec
    ink_contrast: float
    label: InkSpec
    tab: InkSpec
    voice: VoiceSpec
    hues: dict[Paint, float]
    livery: int
    arcs: tuple[Arc, ...]
    ranks: tuple[Rank, ...]
    gain: Gain
    surfaces: Surfaces
    terminal: Terminal
    style: dict[Syntax, tuple[str, ...]]

    @classmethod
    def load(cls, path: Path) -> Spec:
        where = path.name
        raw = tomllib.loads(path.read_text())

        def table(
            key: str,
            fields: tuple[str, ...],
            required: tuple[str, ...],
            value: object = None,
        ) -> dict:
            """`[key]`, or `value` standing for it, checked against its fields; an optional table may be absent."""
            if value is None:
                value = raw.get(key, None if required else {})
            if not isinstance(value, dict):
                raise Invalid(f"{where}: missing [{key}]")
            if unknown := value.keys() - set(fields):
                raise Invalid(f"{where}: [{key}] has unknown keys {sorted(unknown)}")
            if missing := set(required) - value.keys():
                raise Invalid(f"{where}: [{key}] omits {sorted(missing)}")
            return value

        tables = {
            "stance",
            "ground",
            "ink",
            "label",
            "tab",
            "voice",
            "hues",
            "livery",
            "ranks",
            "gain",
            "surfaces",
            "terminal",
            "style",
        }
        if unknown := raw.keys() - tables:
            raise Invalid(f"{where}: unknown tables {sorted(unknown)}")
        ground = table(
            "ground",
            ("hue", "colorfulness", "lightness"),
            ("hue", "colorfulness", "lightness"),
        )
        ink = table("ink", ("contrast",), ("contrast",))
        label = table("label", ("hue", "colorfulness", "contrast"), ())
        tab = table("tab", ("hue", "colorfulness", "contrast"), ())
        voice = table(
            "voice",
            ("colorfulness", "lean", "naturalism", "lift", "least"),
            ("colorfulness",),
        )
        optional = (
            Figure.ADDED,
            Figure.REMOVED,
            Syntax.CONSTANT,
            Syntax.COMMENT,
            Syntax.PUNCTUATION,
            Syntax.VARIABLE,
            Figure.CODE,
        )
        hues = table(
            "hues",
            tuple(role.value for role in AUTHORED),
            tuple(role.value for role in AUTHORED if role not in optional),
        )

        def hue(name: str, value: object) -> float:
            if isinstance(value, bool) or not isinstance(value, int | float):
                raise Invalid(f"{where}: [hues] {name} must be a hue")
            return float(value) % 360

        livery = table("livery", ("count", "arcs"), ("count",))
        hued = {role.value: role for role in AUTHORED}
        authored = {hued[name]: hue(name, value) for name, value in hues.items()}
        defaults = {
            **DIFF_HUES,
            Syntax.CONSTANT: authored[Syntax.NUMBER],
            Syntax.COMMENT: float(ground["hue"]),
            Syntax.PUNCTUATION: float(ground["hue"]),
            Syntax.VARIABLE: float(ground["hue"]),
            Figure.CODE: authored[Syntax.STRING],
        }
        naturalism = float(voice.get("naturalism", 0.0))
        syntax = {member.value: member for member in Syntax}
        ranks: list[Rank] = []
        if not isinstance(raw.get("ranks", {}), dict):
            raise Invalid(f"{where}: [ranks] must hold a table per rank")
        for name, rank in raw.get("ranks", {}).items():
            rank = table(
                f"ranks.{name}",
                ("members", "lc", "gain", "naturalism", "least"),
                ("members", "lc", "gain"),
                rank,
            )
            members = rank["members"]
            if not members or not all(member in syntax for member in members):
                raise Invalid(f"{where}: rank {name} must hold syntax classes")
            ranks.append(
                Rank(
                    name,
                    tuple(syntax[member] for member in members),
                    float(rank["lc"]),
                    float(rank["gain"]),
                    float(rank.get("naturalism", naturalism)),
                    float(rank.get("least", SYNTAX_LEAST)),
                )
            )
        ranked = [member for rank in ranks for member in rank.members]
        if len(ranked) != len(set(ranked)):
            raise Invalid(f"{where}: a syntax class is ranked twice")
        gain = table("gain", Gain.__dataclass_fields__, ())
        if held := {member.value for member in ranked} & gain.keys():
            raise Invalid(
                f"{where}: [gain] sets {sorted(held)}, whose ranks ask their own"
            )
        surfaces = table("surfaces", Surfaces.__dataclass_fields__, ())
        terminal = table("terminal", Terminal.__dataclass_fields__, ())
        style = table("style", tuple(syntax), ())
        for name, modifiers in style.items():
            if not modifiers or not set(modifiers) <= set(STYLES):
                raise Invalid(f"{where}: [style] {name} must be some of {list(STYLES)}")
        inked = float(ink["contrast"])
        law = label_law(inked, authored[Figure.ACCENT])
        labeled = InkSpec(
            float(label.get("hue", law.hue)) % 360,
            float(label.get("colorfulness", law.colorfulness)),
            float(label.get("contrast", law.contrast)),
        )
        tabbed = tab_law(inked, labeled)
        return cls(
            name=path.stem,
            stance=str(raw["stance"]).strip(),
            ground=GroundSpec(
                float(ground["hue"]),
                float(ground["colorfulness"]),
                float(ground["lightness"]),
            ),
            ink_contrast=inked,
            label=labeled,
            tab=InkSpec(
                float(tab.get("hue", tabbed.hue)) % 360,
                float(tab.get("colorfulness", tabbed.colorfulness)),
                float(tab.get("contrast", tabbed.contrast)),
            ),
            voice=VoiceSpec(
                float(voice["colorfulness"]),
                float(voice.get("lean", 0.0)),
                naturalism,
                float(voice.get("lift", 0.0)),
                float(voice.get("least", SYNTAX_LEAST)),
            ),
            hues=defaults | authored,
            livery=int(livery["count"]),
            arcs=tuple((float(a), float(b)) for a, b in livery.get("arcs", [[0, 360]])),
            ranks=tuple(ranks),
            gain=Gain(**{key: float(value) for key, value in gain.items()}),
            surfaces=Surfaces(**{key: float(value) for key, value in surfaces.items()}),
            terminal=Terminal(**{key: float(value) for key, value in terminal.items()}),
            style={syntax[name]: tuple(modifiers) for name, modifiers in style.items()},
        )


def within(hue: float | np.ndarray, arc: Arc) -> np.ndarray:
    low, high = arc
    hue = np.asarray(hue) % 360
    if high - low >= 360:
        return np.ones_like(hue, bool)
    low, high = low % 360, high % 360
    return (low <= hue) & (hue <= high) if low <= high else (hue >= low) | (hue <= high)


# -- Laws ----------------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Ring:
    """A family of roles sharing one level, and the composition that holds the level there."""

    label: str
    level: float
    held_by: Paint | str
    over: Paint


@dataclass(frozen=True, slots=True)
class Pack:
    spec: Spec
    roles: dict[Paint, Swatch]
    livery: tuple[Swatch, ...]
    heat: tuple[Swatch, ...]
    terminal: tuple[Swatch, ...]
    rings: tuple[Ring, ...]


def smelt(spec: Spec) -> Pack:
    g, voice, hues, gain, surfaces = (
        spec.ground,
        spec.voice,
        spec.hues,
        spec.gain,
        spec.surfaces,
    )
    floors = composition(spec)
    roles: dict[Paint, Swatch] = {}
    canvas = roles[Ground.CANVAS] = paint(g.lightness, g.colorfulness, g.hue)
    accent = hues[Figure.ACCENT]

    roles[Ground.DAIS] = paint(*elevated(canvas, surfaces.dais), g.hue)
    for lit, rise, toward, tint in (
        (Ground.SELECTION, surfaces.selection_rise, accent, surfaces.selection_tint),
        (
            Ground.PROMPT,
            surfaces.prompt_rise,
            BRACKET_HUES[Bracket.PROMPT],
            surfaces.prompt_tint,
        ),
    ):
        j, m = elevated(canvas, rise)
        roles[lit] = paint(j, *tinted(m, g.hue, toward, tint))
    for fill, figure in (
        (Ground.ADDED, Figure.ADDED),
        (Ground.REMOVED, Figure.REMOVED),
    ):
        roles[fill] = paint(
            elevated(canvas, DIFF_BAND)[0], surfaces.diff_tint, hues[figure]
        )

    def margin(role: Paint, rgb: np.ndarray) -> np.ndarray:
        """Least Lc surplus of each candidate color over every ground the role is composed on."""
        return np.min(
            [apca(rgb, roles[ground].array) - floor for ground, floor in floors[role]],
            axis=0,
        )

    def text(role: Paint, m: float, hue: float) -> Swatch:
        """The darkest color of this hue and colorfulness that clears every floor of the role."""
        j = least_lightness(
            lambda j: margin(role, yielding(j, m, hue)[0]), canvas.lightness, 100.0
        )
        return paint(j, m, hue)

    def gray(role: Paint) -> Swatch:
        """The darkest gray that clears every floor of the role. Plain text carries no hue: equal
        channels, not the frame's neutral, which partial adaptation warms."""
        levels = np.repeat(np.arange(256)[:, None], 3, axis=1)
        clear = margin(role, levels) >= 0
        return Swatch.of(levels[int(np.argmax(clear)) if clear.any() else -1], 0.0)

    for role in (Figure.INK, Figure.INK_MUTED, Figure.INK_FAINT, Figure.INK_SELECTED):
        roles[role] = gray(role)
    roles[Figure.LABEL] = text(Figure.LABEL, spec.label.colorfulness, spec.label.hue)
    roles[Figure.TAB] = text(Figure.TAB, spec.tab.colorfulness, spec.tab.hue)
    roles[Figure.RULE] = text(Figure.RULE, canvas.colorfulness * 1.6, g.hue)

    home = cusp(accent)
    ranked = {member for rank in spec.ranks for member in rank.members}
    families = (
        (
            "text",
            tuple(role for role in TEXT_RING if role not in ranked),
            1.0,
            voice.naturalism,
        ),
        ("signal", SIGNAL_RING, gain.signal, voice.naturalism),
        *((rank.name, rank.members, rank.gain, rank.naturalism) for rank in spec.ranks),
    )
    rings = []
    for label, members, scale, naturalism in families:
        ring_hues = np.array([hues[role] for role in members])
        leaning = np.array([role not in UNLEANED for role in members])
        diff = np.array(
            [
                gain.diff if role in (Figure.ADDED, Figure.REMOVED) else 1.0
                for role in members
            ]
        )
        asked = (
            voice.colorfulness
            * scale
            * diff
            * (1 + leaning * voice.lean * np.cos(np.radians(ring_hues - accent)))
        )
        offset = naturalism * (np.array([cusp(h) for h in ring_hues]) - home)

        def surplus(
            levels: np.ndarray,
            members=members,
            ring_hues=ring_hues,
            asked=asked,
            offset=offset,
        ) -> np.ndarray:
            """(levels, compositions) Lc surplus of every member over every ground it is composed on."""
            rgb = yielding(levels[:, None] + offset, asked, ring_hues)[0]
            return np.stack(
                [
                    apca(rgb[:, i], roles[ground].array) - floor
                    for i, role in enumerate(members)
                    for ground, floor in floors[role]
                ],
                -1,
            )

        level = least_lightness(
            lambda j: surplus(j).min(-1) - 1.0,
            canvas.lightness,
            100.0 - float(offset.max()),
        )
        pairs = [(role, ground) for role in members for ground, _ in floors[role]]
        held_by, over = pairs[int(np.argmin(surplus(np.array([level]))[0]))]
        level += voice.lift
        rings.append(Ring(label, level, held_by, over))
        for role, rgb, m in zip(
            members, yielding(level + offset, asked, ring_hues)[0], asked, strict=True
        ):
            roles[role] = Swatch.of(rgb, m)

    if Syntax.COMMENT not in ranked:
        roles[Syntax.COMMENT] = text(
            Syntax.COMMENT, voice.colorfulness * gain.comment, hues[Syntax.COMMENT]
        )
    if Syntax.PUNCTUATION not in ranked:
        roles[Syntax.PUNCTUATION] = gray(Syntax.PUNCTUATION)
    if Syntax.VARIABLE not in ranked:
        roles[Syntax.VARIABLE] = text(
            Syntax.VARIABLE, VARIABLE_TINT, hues[Syntax.VARIABLE]
        )
    roles[Figure.CODE] = text(
        Figure.CODE, voice.colorfulness * gain.code, hues[Figure.CODE]
    )
    terminal_ring, terminal_ramp = terminal(spec, roles)
    rings.append(terminal_ring)
    ramp = livery(spec, roles, rings[0].level)
    roles.update(brackets(spec, ramp))
    return Pack(spec, roles, ramp, heat(roles), terminal_ramp, tuple(rings))


def livery(spec: Spec, roles: dict[Paint, Swatch], level: float) -> tuple[Swatch, ...]:
    """Identities as far apart as the panel allows, and apart from the status colors.

    Identity is the one family where distinctness outranks the voice: two agents that look alike
    are a lie about the screen. Each identity takes all the colorfulness the gamut gives, up to a
    fifth again the voice's, at any lightness from the least that clears the voice floor to
    LIVERY_RISE above it, and the hues maximize the least pairwise distance."""
    canvas = roles[Ground.CANVAS].array
    cap = spec.voice.colorfulness * 1.2
    hues = np.arange(0.0, 360.0, 1.0)
    allowed = np.any([within(hues, arc) for arc in spec.arcs], axis=0)
    low, high = np.full_like(hues, level - 6.0), np.full_like(hues, 99.0)
    for _ in range(20):
        mid = (low + high) / 2
        ok = apca(yielding(mid, cap, hues)[0], canvas) >= VOICE + 1.0
        low, high = np.where(ok, low, mid), np.where(ok, mid, high)
    rises = np.arange(0.0, LIVERY_RISE + 0.1, 1.0)
    j = np.minimum(high[:, None] + rises[None], 99.0)
    grid_rgb = yielding(j, cap, np.broadcast_to(hues[:, None], j.shape))[0]
    grid = ucs(grid_rgb)
    repellers = ucs(np.array([roles[role].rgb for role in REPELLERS]))
    reach = np.array(list(REPELLERS.values()))
    repel = np.min(
        np.linalg.norm(grid[:, :, None] - repellers[None, None], axis=-1) / reach,
        axis=-1,
    )
    count = spec.livery

    def score(h: np.ndarray, r: np.ndarray) -> tuple[float, float]:
        if not allowed[h].all():
            return (-1.0, -1.0)
        points = grid[h, r]
        d = np.linalg.norm(points[:, None] - points[None], axis=-1)[
            np.triu_indices(count, 1)
        ]
        ordered = np.sort(np.concatenate([d, repel[h, r]]))
        return (float(ordered[0]), float(ordered[1]))

    best: tuple[tuple[float, float], np.ndarray, np.ndarray] | None = None
    permitted = np.flatnonzero(allowed)
    spacing = len(permitted) / count
    for start in range(max(1, int(spacing))):
        h = permitted[
            np.round(start + spacing * np.arange(count)).astype(int) % len(permitted)
        ]
        r = (np.arange(count) % 2) * (len(rises) // 2)
        current = score(h, r)
        if current[0] < 0:
            continue
        step = max(1, int(spacing // 4))
        while step >= 1:
            improved = False
            for i in range(count):
                for dh, dr in ((step, 0), (-step, 0), (0, 1), (0, -1)):
                    th, tr = h.copy(), r.copy()
                    th[i] = (th[i] + dh) % 360
                    tr[i] = min(max(tr[i] + dr, 0), len(rises) - 1)
                    candidate = score(th, tr)
                    if candidate > current:
                        h, r, current, improved = th, tr, candidate, True
            if not improved:
                step //= 2
        if best is None or current > best[0]:
            best = (current, h, r)
    if best is None:
        raise Invalid(f"{spec.name}: no livery fits the permitted arcs")
    _, h, r = best
    return tuple(Swatch.of(grid_rgb[h[i], r[i]], cap) for i in np.argsort(h))


def brackets(spec: Spec, ramp: tuple[Swatch, ...]) -> dict[Paint, Swatch]:
    """Each bracket's identity: a palette marks its speakers only in colors it already has. The
    brackets keep BRACKET_HUES's order round the hue circle among the identities BRACKET_COLORFUL
    admits, and of the assignments that keep it, take the one whose hues stray least, summed, from
    the hues they are chosen nearest."""
    order = tuple(BRACKET_HUES)
    admitted = sorted(
        (s for s in ramp if s.colorfulness >= BRACKET_COLORFUL), key=lambda s: s.hue
    )
    if len(admitted) < len(order):
        raise Invalid(
            f"{spec.name}: {len(admitted)} identities reach M {BRACKET_COLORFUL:g}, and the brackets need {len(order)}"
        )

    def stray(ring: tuple[Swatch, ...]) -> float:
        return sum(
            180.0 - abs(abs(s.hue - BRACKET_HUES[b]) % 360.0 - 180.0)
            for b, s in zip(order, ring, strict=True)
        )

    rings = (
        chosen[turn:] + chosen[:turn]
        for chosen in itertools.combinations(admitted, len(order))
        for turn in range(len(order))
    )
    return dict(zip(order, min(rings, key=stray), strict=True))


def heat(roles: dict[Paint, Swatch]) -> tuple[Swatch, ...]:
    """Activity, from the faintest trace to full intensity. The first step is the darkest at a
    quarter of the accent's M that clears the rule's floor over the canvas, the last is the accent,
    and the steps between divide J_HK evenly while M rises by quarters."""
    canvas, accent = roles[Ground.CANVAS], roles[Figure.ACCENT]
    first = accent.colorfulness / HEAT_STEPS
    floor = dict(RAMP_FLOORS["heat"])[Ground.CANVAS]
    j1 = least_lightness(
        lambda j: apca(yielding(j, first, accent.hue)[0], canvas.array) - floor,
        canvas.lightness,
        100.0,
    )
    rising = (
        paint(
            j1 + (k - 1) / (HEAT_STEPS - 1) * (accent.lightness - j1),
            k / HEAT_STEPS * accent.colorfulness,
            accent.hue,
        )
        for k in range(1, HEAT_STEPS)
    )
    return (*rising, accent)


def terminal(spec: Spec, roles: dict[Paint, Swatch]) -> tuple[Ring, tuple[Swatch, ...]]:
    """Tool output's sixteen colors in xterm order. The neutrals are the pack's own: black is the
    dais, white and bright black the muted and faint ink, bright white the ink. The six hues are a
    third ring, unleaned at the voice's M with the rings' naturalism and lift, over the grounds
    tool output is painted on; their brights stand BRIGHT above it."""
    voice, accent = spec.voice, spec.hues[Figure.ACCENT]
    floors = RAMP_FLOORS["terminal"]
    hues = np.array(
        [
            spec.hues[role]
            for role in (Figure.FAILURE, Figure.SUCCESS, Figure.CAUTION, Figure.INFO)
        ]
        + list(TERMINAL_HUES)
    )
    asked = np.full(len(hues), voice.colorfulness * spec.gain.terminal)
    offset = voice.naturalism * (np.array([cusp(h) for h in hues]) - cusp(accent))

    def surplus(levels: np.ndarray) -> np.ndarray:
        rgb = yielding(levels[:, None] + offset, asked, hues)[0]
        return np.stack(
            [
                apca(rgb[:, i], roles[ground].array) - floor
                for i in range(len(hues))
                for ground, floor in floors
            ],
            -1,
        )

    level = least_lightness(
        lambda j: surplus(j).min(-1) - 1.0,
        roles[Ground.CANVAS].lightness,
        100.0 - float(offset.max()),
    )
    pairs = [
        (f"slot {slot}", ground)
        for slot in range(1, len(hues) + 1)
        for ground, _ in floors
    ]
    held_by, over = pairs[int(np.argmin(surplus(np.array([level]))[0]))]
    level += voice.lift

    def at(lightness: float) -> list[Swatch]:
        rgb = yielding(lightness + offset, asked, hues)[0]
        return [Swatch.of(rgb[i], asked[i]) for i in range(len(hues))]

    ramp = (
        roles[Ground.DAIS],
        *at(level),
        roles[Figure.INK_MUTED],
        roles[Figure.INK_FAINT],
        *at(level + BRIGHT),
        roles[Figure.INK],
    )
    if len(ramp) != TERMINAL_SLOTS:
        raise Invalid(
            f"{spec.name}: the terminal ramp has {len(ramp)} slots, the vocabulary {TERMINAL_SLOTS}"
        )
    return Ring("terminal", level, held_by, over), ramp
