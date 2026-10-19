"""The laws: every role, ramp and ring derived from a spec's decisions, and the doctrine the
proof holds them to. A spec states a palette's character in perceptual terms: the ground it is
painted on, the temperature of its ink, the hues it speaks in and the shape of its voice; no
color is chosen by hand at a call site or in a pack.
"""

from __future__ import annotations

import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from appearance import Swatch, apca, cusp, distance, elevated, least_lightness, paint, tinted, ucs, yielding
from vocabulary import HEAT_STEPS, TERMINAL_SLOTS, Figure, Ground, Paint, Syntax

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
COMMENT = 44.0
RULE = 16.0

TROUGH = -3.0
DAIS = 5.0
RESPONSE = 1.0
BAND = 2.0
DIFF_BAND = 1.5
SELECTION_RISE = 9.0
"""Elevations in CAM16-UCS J' from the canvas. A surface is the ground's material raised or
recessed at constant hue and saturation, so a lit surface reads as the same paint in more light
rather than as a different paint."""

BAND_TINT = 2.8
SELECTION_TINT = 9.0
"""Colorfulness a band adds toward its hue, as a vector in the appearance plane: a band is the
ground tinted, not a color laid over it. The selection bar keeps the colors of the row it
selects, so it is a lit band of the ground and never a light chip."""
DIFF_TINT = 7.0
"""Colorfulness of a diff band, which takes its ink's hue outright: a removed line must read as
the removed hue on every ground, not as that hue mixed with the page."""

LIVERY_RISE = 10.0
"""J_HK an identity may climb above the least lightness that clears the voice floor."""
LIVERY_LEAST = 10.0
"""Least dE between two identities."""
REPELLERS = {Figure.FAILURE: 1.6, Figure.CAUTION: 0.9, Figure.SUCCESS: 0.9}
"""How far, in multiples of LIVERY_LEAST, an identity keeps from each status color: an agent
painted near failure reads as an agent that failed."""

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

DIFF_HUES = {Figure.ADDED: 125.0, Figure.REMOVED: 345.0}
"""Lime against rose: the pair a protan or deutan still tells apart, ink and band alike, because
it runs along the blue-yellow axis that survives dichromacy. A spec may move either inside its
window."""


SURFACES = (
    Ground.CANVAS,
    Ground.TROUGH,
    Ground.DAIS,
    Ground.SELECTION,
    Ground.PROMPT,
    Ground.REASONING,
    Ground.ASIDE,
    Ground.SUBAGENT,
)


@dataclass(frozen=True, slots=True)
class Distinct:
    label: str
    roles: tuple[Paint, ...]
    least: float
    dichromat: float | None = None


DISTINCT = (
    Distinct("status", (Figure.SUCCESS, Figure.CAUTION, Figure.FAILURE, Figure.INFO), 14.0, 6.0),
    Distinct("accent against status", (Figure.ACCENT, Figure.FAILURE, Figure.CAUTION, Figure.SUCCESS), 10.0),
    Distinct("diff ink", (Figure.ADDED, Figure.REMOVED), 18.0, 9.0),
    Distinct("diff band", (Ground.ADDED, Ground.REMOVED), 6.0, 3.0),
    Distinct(
        "syntax",
        (Syntax.KEYWORD, Syntax.FUNCTION, Syntax.TYPE, Syntax.STRING, Syntax.NUMBER, Syntax.ATTRIBUTE),
        9.0,
    ),
    Distinct("code recession", (Syntax.COMMENT, Syntax.PUNCTUATION, Syntax.VARIABLE), 5.0),
    Distinct("surfaces", SURFACES, 2.0),
    Distinct("ink ladder", (Figure.INK, Figure.INK_MUTED, Figure.INK_FAINT), 8.0),
    Distinct("code against prose", (Figure.CODE, Figure.INK), 10.0),
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
BANDS = (Ground.PROMPT, Ground.REASONING, Ground.ASIDE, Ground.SUBAGENT)
AUTHORED = (*TEXT_RING, *SIGNAL_RING, Syntax.COMMENT, Syntax.PUNCTUATION, Syntax.VARIABLE, *BANDS, Figure.CODE)


def composition(ink: float) -> dict[Paint, Floors]:
    """Every ground the TUI paints each text role on, with the Lc it must reach there. The solver
    lifts each role to this table and the proof checks it, so the two cannot disagree."""
    body, secondary, tertiary = ink, ink * INK_MUTED, ink * INK_FAINT

    def ladder(tier: float) -> Floors:
        return (
            (Ground.CANVAS, tier),
            (Ground.TROUGH, tier - 2),
            (Ground.DAIS, tier - 4),
            (Ground.RESPONSE, tier - 3),
            (Ground.PROMPT, tier - 4),
            (Ground.REASONING, tier - 4),
            (Ground.ASIDE, tier - 4),
            (Ground.SUBAGENT, tier - 4),
            (Ground.SELECTION, tier - 10),
        )

    code: Floors = (
        (Ground.TROUGH, VOICE),
        (Ground.CANVAS, VOICE),
        (Ground.RESPONSE, VOICE - 2),
        (Ground.REASONING, VOICE - 4),
        (Ground.SUBAGENT, VOICE - 4),
        (Ground.ADDED, VOICE - 4),
        (Ground.REMOVED, VOICE - 4),
    )

    def banded(floors: Floors, floor: float) -> Floors:
        """`floors` and both diff bands at `floor`: highlighted code carries the role into diffs."""
        return (*floors, (Ground.ADDED, floor), (Ground.REMOVED, floor))

    return {
        Figure.INK: (*ladder(body - 1), (Ground.ADDED, body - 6), (Ground.REMOVED, body - 6)),
        Figure.INK_MUTED: (*ladder(secondary - 1), (Ground.ADDED, secondary - 6), (Ground.REMOVED, secondary - 6)),
        Figure.INK_FAINT: (*ladder(tertiary - 1), (Ground.ADDED, tertiary - 6), (Ground.REMOVED, tertiary - 6)),
        Figure.INK_SELECTED: ((Ground.SELECTION, body - 4),),
        Figure.RULE: (
            (Ground.CANVAS, RULE - 1),
            (Ground.DAIS, RULE - 8),
            (Ground.RESPONSE, RULE - 4),
            (Ground.REASONING, RULE - 5),
            (Ground.SUBAGENT, RULE - 5),
        ),
        Figure.ACCENT: ladder(VOICE),
        Figure.LINK: banded(ladder(VOICE), VOICE - 4),
        Figure.SUCCESS: ladder(SIGNAL),
        Figure.CAUTION: banded(ladder(SIGNAL), SIGNAL - 4),
        Figure.FAILURE: banded(ladder(SIGNAL), SIGNAL - 4),
        Figure.INFO: ladder(SIGNAL),
        Figure.ADDED: (
            (Ground.ADDED, VOICE),
            (Ground.CANVAS, VOICE),
            (Ground.TROUGH, VOICE),
            (Ground.RESPONSE, VOICE - 2),
            (Ground.REASONING, VOICE - 4),
            (Ground.SUBAGENT, VOICE - 4),
            (Ground.REMOVED, VOICE - 4),
        ),
        Figure.REMOVED: (
            (Ground.REMOVED, VOICE),
            (Ground.CANVAS, VOICE),
            (Ground.TROUGH, VOICE),
            (Ground.RESPONSE, VOICE - 2),
            (Ground.REASONING, VOICE - 4),
            (Ground.SUBAGENT, VOICE - 4),
            (Ground.ADDED, VOICE - 4),
        ),
        Figure.CODE: banded(
            (
                (Ground.CANVAS, body - 8),
                (Ground.TROUGH, body - 8),
                (Ground.DAIS, body - 12),
                (Ground.RESPONSE, body - 11),
                (Ground.PROMPT, body - 12),
                (Ground.REASONING, body - 12),
                (Ground.ASIDE, body - 12),
                (Ground.SUBAGENT, body - 12),
            ),
            body - 12,
        ),
        **{role: code for role in SYNTAX_VOICE},
        Syntax.COMMENT: tuple((g, COMMENT - (0 if g is Ground.TROUGH else 3)) for g, _ in code),
        Syntax.PUNCTUATION: tuple((g, secondary - 6) for g, _ in code),
        Syntax.VARIABLE: tuple((g, body - 10) for g, _ in code),
    }


RAMP_FLOORS: dict[str, Floors] = {
    "livery": ((Ground.CANVAS, VOICE), (Ground.TROUGH, VOICE - 2), (Ground.DAIS, VOICE - 4)),
    "heat": ((Ground.CANVAS, RULE - 1),),
    "terminal": (
        (Ground.CANVAS, SIGNAL),
        (Ground.RESPONSE, SIGNAL - 3),
        (Ground.PROMPT, SIGNAL - 4),
        (Ground.ASIDE, SIGNAL - 4),
        (Ground.SUBAGENT, SIGNAL - 4),
    ),
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
    iso-lightness ring. lift raises the solved rings by that much J_HK."""

    colorfulness: float
    lean: float
    naturalism: float
    lift: float


@dataclass(frozen=True, slots=True)
class Spec:
    name: str
    stance: str
    ground: GroundSpec
    ink: InkSpec
    voice: VoiceSpec
    hues: dict[Paint, float]
    livery: int
    arcs: tuple[Arc, ...]

    @classmethod
    def load(cls, path: Path) -> Spec:
        where = path.name
        raw = tomllib.loads(path.read_text())

        def table(key: str, fields: tuple[str, ...], required: tuple[str, ...]) -> dict:
            value = raw.get(key)
            if not isinstance(value, dict):
                raise Invalid(f"{where}: missing [{key}]")
            if unknown := value.keys() - set(fields):
                raise Invalid(f"{where}: [{key}] has unknown keys {sorted(unknown)}")
            if missing := set(required) - value.keys():
                raise Invalid(f"{where}: [{key}] omits {sorted(missing)}")
            return value

        if unknown := raw.keys() - {"stance", "ground", "ink", "voice", "hues", "livery"}:
            raise Invalid(f"{where}: unknown tables {sorted(unknown)}")
        ground = table("ground", ("hue", "colorfulness", "lightness"), ("hue", "colorfulness", "lightness"))
        ink = table("ink", ("hue", "colorfulness", "contrast"), ("colorfulness", "contrast"))
        voice = table("voice", ("colorfulness", "lean", "naturalism", "lift"), ("colorfulness",))
        optional = (
            Figure.ADDED,
            Figure.REMOVED,
            Syntax.CONSTANT,
            Syntax.COMMENT,
            Syntax.PUNCTUATION,
            Syntax.VARIABLE,
            Figure.CODE,
            Ground.REASONING,
        )
        hues = table(
            "hues",
            tuple(role.value for role in AUTHORED),
            tuple(role.value for role in AUTHORED if role not in optional),
        )
        livery = table("livery", ("count", "arcs"), ("count",))
        ink_hue = float(ink.get("hue", ground["hue"]))
        hued = {role.value: role for role in AUTHORED}
        authored = {hued[name]: float(hue) % 360 for name, hue in hues.items()}
        defaults = {
            **DIFF_HUES,
            Syntax.CONSTANT: authored[Syntax.NUMBER],
            Syntax.COMMENT: float(ground["hue"]),
            Syntax.PUNCTUATION: ink_hue,
            Syntax.VARIABLE: ink_hue,
            Figure.CODE: authored[Syntax.STRING],
        }
        return cls(
            name=path.stem,
            stance=str(raw["stance"]).strip(),
            ground=GroundSpec(float(ground["hue"]), float(ground["colorfulness"]), float(ground["lightness"])),
            ink=InkSpec(ink_hue, float(ink["colorfulness"]), float(ink["contrast"])),
            voice=VoiceSpec(
                float(voice["colorfulness"]),
                float(voice.get("lean", 0.0)),
                float(voice.get("naturalism", 0.0)),
                float(voice.get("lift", 0.0)),
            ),
            hues=defaults | authored,
            livery=int(livery["count"]),
            arcs=tuple((float(a), float(b)) for a, b in livery.get("arcs", [[0, 360]])),
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


def reasoning(band: Callable[[float], Swatch], surfaces: list[Swatch]) -> Swatch:
    """The reasoning band where its spec authors no hue: tinted toward the hue inside caution's
    window that stands farthest from every other surface, so disclosed reasoning keeps caution's
    hue family without merging into a band it must be told apart from."""
    low, high = WINDOWS[Figure.CAUTION]
    hues = (low + np.arange(0.0, (high - low) % 360 + 1.0)) % 360
    return max((band(hue) for hue in hues), key=lambda s: min(distance(s.rgb, other.rgb) for other in surfaces))


def smelt(spec: Spec) -> Pack:
    g, ink, voice, hues = spec.ground, spec.ink, spec.voice, spec.hues
    floors = composition(ink.contrast)
    roles: dict[Paint, Swatch] = {}
    canvas = roles[Ground.CANVAS] = paint(g.lightness, g.colorfulness, g.hue)
    accent = hues[Figure.ACCENT]

    for role, rise in ((Ground.TROUGH, TROUGH), (Ground.DAIS, DAIS), (Ground.RESPONSE, RESPONSE)):
        roles[role] = paint(*elevated(canvas, rise), g.hue)
    j, m = elevated(canvas, SELECTION_RISE)
    roles[Ground.SELECTION] = paint(j, *tinted(m, g.hue, accent, SELECTION_TINT))

    def band(toward: float) -> Swatch:
        j, m = elevated(canvas, BAND)
        return paint(j, *tinted(m, g.hue, toward, BAND_TINT))

    roles |= {role: band(hues[role]) for role in BANDS if role in hues}
    if Ground.REASONING not in roles:
        roles[Ground.REASONING] = reasoning(band, [roles[s] for s in SURFACES if s is not Ground.REASONING])
    for fill, figure in ((Ground.ADDED, Figure.ADDED), (Ground.REMOVED, Figure.REMOVED)):
        roles[fill] = paint(elevated(canvas, DIFF_BAND)[0], DIFF_TINT, hues[figure])

    def margin(role: Paint, rgb: np.ndarray) -> np.ndarray:
        """Least Lc surplus of each candidate color over every ground the role is composed on."""
        return np.min([apca(rgb, roles[ground].array) - floor for ground, floor in floors[role]], axis=0)

    def text(role: Paint, m: float, hue: float) -> Swatch:
        """The darkest color of this hue and colorfulness that clears every floor of the role."""
        j = least_lightness(lambda j: margin(role, yielding(j, m, hue)[0]), canvas.lightness, 100.0)
        return paint(j, m, hue)

    primary = roles[Figure.INK] = text(Figure.INK, ink.colorfulness, ink.hue)
    for role in (Figure.INK_MUTED, Figure.INK_FAINT):
        probe = text(role, ink.colorfulness, ink.hue)
        roles[role] = text(role, ink.colorfulness * probe.lightness / primary.lightness, ink.hue)
    roles[Figure.INK_SELECTED] = text(Figure.INK_SELECTED, ink.colorfulness, ink.hue)
    roles[Figure.RULE] = text(Figure.RULE, canvas.colorfulness * 1.6, g.hue)

    home = cusp(accent)
    rings = []
    for label, members in (("text", TEXT_RING), ("signal", SIGNAL_RING)):
        ring_hues = np.array([hues[role] for role in members])
        leaning = np.array([role not in UNLEANED for role in members])
        asked = voice.colorfulness * (1 + leaning * voice.lean * np.cos(np.radians(ring_hues - accent)))
        offset = voice.naturalism * (np.array([cusp(h) for h in ring_hues]) - home)

        def surplus(levels: np.ndarray, members=members, ring_hues=ring_hues, asked=asked, offset=offset) -> np.ndarray:
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

        level = least_lightness(lambda j: surplus(j).min(-1) - 1.0, canvas.lightness, 100.0 - float(offset.max()))
        pairs = [(role, ground) for role in members for ground, _ in floors[role]]
        held_by, over = pairs[int(np.argmin(surplus(np.array([level]))[0]))]
        level += voice.lift
        rings.append(Ring(label, level, held_by, over))
        for role, rgb, m in zip(members, yielding(level + offset, asked, ring_hues)[0], asked, strict=True):
            roles[role] = Swatch.of(rgb, m)

    roles[Syntax.COMMENT] = text(Syntax.COMMENT, voice.colorfulness * 0.4, hues[Syntax.COMMENT])
    roles[Syntax.PUNCTUATION] = text(Syntax.PUNCTUATION, ink.colorfulness * 1.5, hues[Syntax.PUNCTUATION])
    roles[Syntax.VARIABLE] = text(Syntax.VARIABLE, max(ink.colorfulness * 2, 6.0), hues[Syntax.VARIABLE])
    roles[Figure.CODE] = text(Figure.CODE, voice.colorfulness * 0.6, hues[Figure.CODE])
    terminal_ring, terminal_ramp = terminal(spec, roles)
    rings.append(terminal_ring)
    ramp = livery(spec, roles, rings[0].level)
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
    repel = np.min(np.linalg.norm(grid[:, :, None] - repellers[None, None], axis=-1) / reach, axis=-1)
    count = spec.livery

    def score(h: np.ndarray, r: np.ndarray) -> tuple[float, float]:
        if not allowed[h].all():
            return (-1.0, -1.0)
        points = grid[h, r]
        d = np.linalg.norm(points[:, None] - points[None], axis=-1)[np.triu_indices(count, 1)]
        ordered = np.sort(np.concatenate([d, repel[h, r]]))
        return (float(ordered[0]), float(ordered[1]))

    best: tuple[tuple[float, float], np.ndarray, np.ndarray] | None = None
    spacing = 360 / count
    for start in range(int(spacing)):
        h = np.round(start + spacing * np.arange(count)).astype(int) % 360
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


def heat(roles: dict[Paint, Swatch]) -> tuple[Swatch, ...]:
    """Activity, from the faintest trace to full intensity. The first step is the darkest at a
    quarter of the accent's M that clears the rule's floor over the canvas, the last is the accent,
    and the steps between divide J_HK evenly while M rises by quarters."""
    canvas, accent = roles[Ground.CANVAS], roles[Figure.ACCENT]
    first = accent.colorfulness / HEAT_STEPS
    floor = dict(RAMP_FLOORS["heat"])[Ground.CANVAS]
    j1 = least_lightness(
        lambda j: apca(yielding(j, first, accent.hue)[0], canvas.array) - floor, canvas.lightness, 100.0
    )
    rising = (
        paint(j1 + (k - 1) / (HEAT_STEPS - 1) * (accent.lightness - j1), k / HEAT_STEPS * accent.colorfulness, accent.hue)
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
        [spec.hues[role] for role in (Figure.FAILURE, Figure.SUCCESS, Figure.CAUTION, Figure.INFO)]
        + list(TERMINAL_HUES)
    )
    asked = np.full(len(hues), voice.colorfulness)
    offset = voice.naturalism * (np.array([cusp(h) for h in hues]) - cusp(accent))

    def surplus(levels: np.ndarray) -> np.ndarray:
        rgb = yielding(levels[:, None] + offset, asked, hues)[0]
        return np.stack(
            [apca(rgb[:, i], roles[ground].array) - floor for i in range(len(hues)) for ground, floor in floors],
            -1,
        )

    level = least_lightness(lambda j: surplus(j).min(-1) - 1.0, roles[Ground.CANVAS].lightness, 100.0 - float(offset.max()))
    pairs = [(f"slot {slot}", ground) for slot in range(1, len(hues) + 1) for ground, _ in floors]
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
        raise Invalid(f"{spec.name}: the terminal ramp has {len(ramp)} slots, the vocabulary {TERMINAL_SLOTS}")
    return Ring("terminal", level, held_by, over), ramp
