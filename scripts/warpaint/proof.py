"""The proof: every declared pair, distinctness rule and ramp law checked against a smelted
pack, written out as the pack's proof page. A fault fails the gate."""

from __future__ import annotations

import math
import re
from collections.abc import Callable
from pathlib import Path

import numpy as np

from appearance import Rgb, Swatch, Vision, _decode, apca, distance
from laws import (
    BRACKET_HUES,
    BRIGHT_LEAST,
    DISTINCT,
    GROUND_CEILING,
    LABEL_CEILING,
    HEAT_LEAST,
    LIVERY_LEAST,
    RAMP_FLOORS,
    REPELLERS,
    RUNNING,
    SIGNAL_RING,
    TEXT_RING,
    VALUE_STEP,
    WINDOWS,
    Distinct,
    Gain,
    Invalid,
    Pack,
    Spec,
    Surfaces,
    Terminal,
    composition,
    within,
)
from laws import label_law, tab_law
from vocabulary import PAIRS, RAMP_PAIRS, Bracket, Figure, Ground, Syntax

TUI = Path(__file__).resolve().parent / "../../codex-rs/tui/src"


def _cie_lab(rgb: np.ndarray) -> np.ndarray:
    """CIE Lab under D65, coefficient for coefficient as color.rs::perceptual_distance."""
    r, g, b = np.moveaxis(_decode(rgb), -1, 0)
    xyz = np.stack(
        [
            (r * 0.4124 + g * 0.3576 + b * 0.1805) / 0.95047,
            r * 0.2126 + g * 0.7152 + b * 0.0722,
            (r * 0.0193 + g * 0.1192 + b * 0.9505) / 1.08883,
        ],
        -1,
    )
    f = np.where(xyz > 0.008856, np.cbrt(xyz), 7.787 * xyz + 16 / 116)
    return np.stack(
        [
            116 * f[..., 1] - 16,
            500 * (f[..., 0] - f[..., 1]),
            200 * (f[..., 1] - f[..., 2]),
        ],
        -1,
    )


def _xterm_fixed() -> np.ndarray:
    """Lab of indices 16..255 of the TUI's own table, parsed so the two cannot drift."""
    source = (TUI / "terminal_palette.rs").read_text()
    block = source.split("pub const XTERM_COLORS: [(u8, u8, u8); 256] = [", 1)[1].split(
        "\n];", 1
    )[0]
    table = re.findall(r"\(\s*(\d+),\s*(\d+),\s*(\d+)\s*\)", block)
    if len(table) != 256:
        raise Invalid(f"parsed {len(table)} xterm colors, expected 256")
    return _cie_lab(np.array(table[16:], int))


XTERM = _xterm_fixed()


def xterm(rgb: Rgb) -> int:
    """The index the TUI's 256-color quantizer chooses."""
    return int(np.argmin(np.linalg.norm(XTERM - _cie_lab(np.array(rgb)), axis=-1))) + 16


def _row(label: object, s: Swatch) -> str:
    return f"| {label} | `{s.hex}` | {s.lightness:.1f} | {s.colorfulness:.1f} | {s.asked:.1f} | {s.hue:.0f} | {xterm(s.rgb)} |"


def _apart(
    label: str,
    members: dict[str, Swatch],
    least: float,
    faults: list[str],
    dichromat: float | None = None,
    pairs: list[tuple[str, str]] | None = None,
    value: Callable[[str, str], float] | None = None,
    told: str = "",
) -> str:
    """Distinctness of every pair of members, or of `pairs`; returns the report line. Where `value`
    is given, a pair nearer than `least` still stands apart when `value` gives it a step of
    VALUE_STEP Lc or more, as `told` names it."""
    names = list(members)
    if pairs is None:
        pairs = [
            (left, right) for i, left in enumerate(names) for right in names[i + 1 :]
        ]
    nearest = math.inf
    steps: list[float] = []
    for left, right in pairs:
        d = distance(members[left].rgb, members[right].rgb)
        nearest = min(nearest, d)
        if d < least:
            step = None if value is None else value(left, right)
            if step is None:
                faults.append(
                    f"{label}: {left} and {right} are dE {d:.1f} apart, below {least:.0f}"
                )
            elif step < VALUE_STEP:
                faults.append(
                    f"{label}: {left} and {right} are dE {d:.1f} apart, below {least:.0f}, "
                    f"and {told} by {step:.1f} Lc, below {VALUE_STEP:.0f}"
                )
            else:
                steps.append(step)
        if dichromat is None:
            continue
        for vision in (Vision.PROTAN, Vision.DEUTAN):
            seen = distance(members[left].rgb, members[right].rgb, vision)
            if seen < dichromat:
                faults.append(
                    f"{label}: {left} and {right} are dE {seen:.1f} apart for a {vision}, below {dichromat:.0f}"
                )
    line = f"- {label}: least dE {nearest:.1f}, floor {least:.0f}"
    if steps:
        line += f"; nearer pairs stand {told} by {min(steps):.1f} Lc or more, floor {VALUE_STEP:.0f}"
    return line


def _ranked(
    rule: Distinct,
    spec: Spec,
    members: dict[str, Swatch],
    lc: dict[str, float],
    faults: list[str],
) -> list[str]:
    """The syntax rule as the spec's ranks partition it: within the voice, within each rank, and
    across levels."""
    if not spec.ranks:
        return [_apart(rule.label, members, spec.voice.least, faults)]
    ranks = [
        (
            f"in rank {rank.name}",
            rank.least,
            [str(m) for m in rank.members if m in rule.roles],
        )
        for rank in spec.ranks
    ]
    ranked = {name for _, _, names in ranks for name in names}
    levels = [
        (
            "in the voice",
            spec.voice.least,
            [name for name in members if name not in ranked],
        ),
        *ranks,
    ]
    lines = [
        _apart(
            f"{rule.label} {where}",
            {name: members[name] for name in names},
            least,
            faults,
        )
        for where, least, names in levels
        if len(names) > 1
    ]
    across = [
        (left, right)
        for i, (_, _, lower) in enumerate(levels)
        for _, _, upper in levels[i + 1 :]
        for left in lower
        for right in upper
    ]
    if across:
        lines.append(
            _apart(
                f"{rule.label} across levels",
                members,
                rule.least,
                faults,
                pairs=across,
                value=lambda left, right: abs(lc[left] - lc[right]),
                told="apart in level",
            )
        )
    return lines


def _brackets(pack: Pack) -> list[str]:
    """Each bracket's identity and the hue it is chosen nearest, in the order they go round the
    circle, and the prompt ground that leans toward the user's."""
    roles, canvas, surfaces = pack.roles, pack.roles[Ground.CANVAS], pack.spec.surfaces
    slot = {s.hex: index for index, s in enumerate(pack.livery)}
    lines = [
        "",
        "## brackets",
        "",
        "| bracket | identity | hex | M | h | sought |",
        "|---|---|---|---|---|---|",
    ]
    for bracket, sought in BRACKET_HUES.items():
        s = roles[bracket]
        lines.append(
            f"| {bracket} | {slot[s.hex]} | `{s.hex}` | {s.colorfulness:.1f} | {s.hue:.0f} | {sought:.0f} |"
        )
    prompt = roles[Ground.PROMPT]
    lines += [
        "",
        f"- the prompt ground rises {surfaces.prompt_rise:g} J' and leans M {surfaces.prompt_tint:g} toward h "
        f"{BRACKET_HUES[Bracket.PROMPT]:.0f}: `{prompt.hex}`, dE {distance(prompt.rgb, canvas.rgb):.1f} off the canvas",
    ]
    return lines


def prove(pack: Pack) -> tuple[str, list[str]]:
    spec, roles, canvas = pack.spec, pack.roles, pack.roles[Ground.CANVAS]
    faults: list[str] = []
    advisories: list[str] = []
    if canvas.colorfulness > GROUND_CEILING:
        faults.append(
            f"canvas carries M {canvas.colorfulness:.1f}, above the ground ceiling {GROUND_CEILING:.0f}"
        )
    for role, arc in WINDOWS.items():
        if not within(spec.hues[role], arc):
            faults.append(
                f"{role} at h {spec.hues[role]:.0f} leaves its window {arc[0]:.0f}-{arc[1]:.0f}"
            )
    for role, heading in ((Figure.LABEL, spec.label), (Figure.TAB, spec.tab)):
        if within(heading.hue, RUNNING):
            faults.append(
                f"{role} at h {heading.hue:.0f} wears running work's green, {RUNNING[0]:.0f}-{RUNNING[1]:.0f}"
            )
    label = float(apca(roles[Figure.LABEL].array, canvas.array))
    if label > LABEL_CEILING:
        faults.append(
            f"the label reads at Lc {label:.1f} over the canvas, above the ceiling {LABEL_CEILING:.0f}"
        )
    tab, faint = (
        float(apca(roles[role].array, canvas.array))
        for role in (Figure.TAB, Figure.INK_FAINT)
    )
    if not faint < tab < label:
        faults.append(
            f"an open tab reads at Lc {tab:.1f} over the canvas, outside the faint ink's {faint:.1f} and the label's {label:.1f}"
        )
    lines = [
        f"# {spec.name}",
        "",
        spec.stance,
        "",
        f"Canvas `{canvas.hex}`: J_HK {canvas.lightness:.1f}, M {canvas.colorfulness:.1f}, h {canvas.hue:.0f}.",
        *(
            f"The {ring.label} ring stands at J_HK {ring.level:.1f}, held there by {ring.held_by} over {ring.over}."
            for ring in pack.rings
        ),
        f"The label reads at Lc {label:.1f} over the canvas, ceiling {LABEL_CEILING:.0f}; an open tab at Lc {tab:.1f}, above the faint ink's {faint:.1f}.",
        "",
        "| role | hex | J_HK | M | asked | h | xterm |",
        "|---|---|---|---|---|---|---|",
    ]
    for role in (*Ground, *Figure, *Syntax):
        s = roles[role]
        lines.append(_row(role, s))
        if role in (*TEXT_RING, *SIGNAL_RING) and s.colorfulness < s.asked - 1.0:
            advisories.append(
                f"{role} yields M {s.asked:.1f} to {s.colorfulness:.1f} at the edge of the gamut"
            )

    if spec.ranks:
        voiced = next(rule for rule in DISTINCT if rule.ranked).roles
        lines += [
            "",
            "## ranks",
            "",
            "| rank | classes | Lc | gain | naturalism | least dE |",
            "|---|---|---|---|---|---|",
        ]
        lines += [
            f"| {r.name} | {', '.join(m.value for m in r.members)} | {r.lc:g} | {r.gain:g} | {r.naturalism:g} | "
            f"{f'{r.least:g}' if sum(m in voiced for m in r.members) > 1 else '-'} |"
            for r in spec.ranks
        ]
    for title, column, stated, doctrine in (
        ("gain", "family", spec.gain, Gain()),
        ("surfaces", "surface", spec.surfaces, Surfaces()),
        ("terminal", "law", spec.terminal, Terminal()),
    ):
        laws = {f: getattr(doctrine, f) for f in doctrine.__dataclass_fields__}
        if moved := [
            (f, getattr(stated, f), law)
            for f, law in laws.items()
            if getattr(stated, f) != law
        ]:
            lines += [
                "",
                f"## {title}",
                "",
                f"| {column} | spec | doctrine |",
                "|---|---|---|",
            ]
            lines += [
                f"| {f.replace('_', ' ')} | {mine:g} | {law:g} |"
                for f, mine, law in moved
            ]
    accent = spec.hues[Figure.ACCENT]
    for title, stated, law in (
        ("label", spec.label, label_law(spec.ink_contrast, accent)),
        ("tab", spec.tab, tab_law(spec.ink_contrast, spec.label)),
    ):
        if moved := [
            f for f in law.__dataclass_fields__ if getattr(stated, f) != getattr(law, f)
        ]:
            lines += [
                "",
                f"## {title}",
                "",
                "| property | spec | doctrine |",
                "|---|---|---|",
            ]
            lines += [
                f"| {f} | {getattr(stated, f):g} | {getattr(law, f):g} |" for f in moved
            ]
    if spec.style:
        lines += ["", "## style", "", "| class | modifiers |", "|---|---|"]
        lines += [
            f"| {syntax} | {', '.join(modifiers)} |"
            for syntax, modifiers in spec.style.items()
        ]

    lines += _brackets(pack)

    lines += [
        "",
        "## contrast",
        "",
        "| text | ground | Lc | floor |",
        "|---|---|---|---|",
    ]
    floors = composition(spec)

    def contrast(
        text: object, swatch: Swatch, ground: object, floor: float | None
    ) -> None:
        lc = float(apca(swatch.array, roles[ground].array))
        if floor is None:
            lines.append(f"| {text} | {ground} | {lc:.1f} | none |")
            faults.append(f"{text} is painted on {ground}, which no law gives a floor")
            return
        lines.append(f"| {text} | {ground} | {lc:.1f} | {floor:.0f} |")
        if lc < floor - 0.5:
            faults.append(
                f"{text} over {ground} reaches Lc {lc:.1f}, below {floor:.0f}"
            )

    for text, grounds in PAIRS.items():
        held = dict(floors.get(text, ()))
        for ground in grounds:
            contrast(text, roles[text], ground, held.get(ground))
        for ground in held.keys() - set(grounds):
            advisories.append(
                f"{text} is solved over {ground}, where the TUI never paints it"
            )
    for ramp, members in (
        ("livery", dict(enumerate(pack.livery))),
        ("heat", {k + 1: s for k, s in enumerate(pack.heat)}),
        (
            "terminal",
            {slot: pack.terminal[slot] for slot in (*range(1, 7), *range(9, 15))},
        ),
    ):
        held = dict(RAMP_FLOORS[ramp])
        for ground in RAMP_PAIRS[ramp]:
            for index, swatch in members.items():
                contrast(f"{ramp} {index}", swatch, ground, held.get(ground))

    lines += ["", "## distinctness", ""]
    lc = {
        str(role): float(apca(swatch.array, canvas.array))
        for role, swatch in roles.items()
    }
    ink = str(Figure.INK)

    def beneath(left: str, right: str) -> float:
        """How far the nearer to prose of two roles sits below the ink, in Lc over the canvas."""
        return min(lc[ink] - lc[name] for name in (left, right) if name != ink)

    for rule in DISTINCT:
        members = {str(role): roles[role] for role in rule.roles}
        if rule.ranked:
            lines += _ranked(rule, spec, members, lc, faults)
            continue
        value = beneath if rule.beneath else None
        lines.append(
            _apart(
                rule.label,
                members,
                rule.least,
                faults,
                rule.dichromat,
                value=value,
                told="beneath prose",
            )
        )
    normal = {f"terminal {slot}": pack.terminal[slot] for slot in range(1, 7)}
    bright = {f"terminal {slot}": pack.terminal[slot] for slot in range(9, 15)}
    lines.append(_apart("terminal", normal, spec.terminal.least, faults))
    lines.append(_apart("terminal brights", bright, spec.terminal.least, faults))
    step = min(
        (distance(pack.terminal[slot].rgb, pack.terminal[slot + 8].rgb), slot)
        for slot in range(1, 7)
    )
    lines.append(
        f"- terminal brights from their slots: least dE {step[0]:.1f} (slot {step[1]}), floor {BRIGHT_LEAST:.0f}"
    )
    if step[0] < BRIGHT_LEAST:
        faults.append(
            f"terminal slot {step[1]} is dE {step[0]:.1f} from its bright, below {BRIGHT_LEAST:.0f}"
        )
    for ground in RAMP_PAIRS["heat"]:
        chain = [roles[ground], *pack.heat]
        rise = min(
            (distance(chain[k].rgb, chain[k + 1].rgb), k) for k in range(len(chain) - 1)
        )
        lines.append(
            f"- heat over {ground}: least step dE {rise[0]:.1f} (into step {rise[1] + 1}), floor {HEAT_LEAST:.0f}"
        )
        if rise[0] < HEAT_LEAST:
            faults.append(
                f"heat step {rise[1] + 1} over {ground} is dE {rise[0]:.1f} from the last, below {HEAT_LEAST:.0f}"
            )
    ramp = pack.livery
    pair = min(
        (distance(ramp[i].rgb, ramp[j].rgb), i, j)
        for i in range(len(ramp))
        for j in range(i + 1, len(ramp))
    )
    near = min(
        (distance(s.rgb, roles[r].rgb) / REPELLERS[r], i, r)
        for i, s in enumerate(ramp)
        for r in REPELLERS
    )
    lines.append(
        f"- livery: least dE {pair[0]:.1f} (entries {pair[1]} and {pair[2]}); nearest status "
        f"{near[0]:.1f} reach-scaled dE (entry {near[1]}, {near[2]})"
    )
    if pair[0] < LIVERY_LEAST:
        faults.append(
            f"livery entries {pair[1]} and {pair[2]} are dE {pair[0]:.1f} apart"
        )
    if near[0] < LIVERY_LEAST * 0.9:
        faults.append(
            f"livery entry {near[1]} stands {near[0]:.1f} reach-scaled dE from {near[2]}"
        )

    for title, ramp_rows in (
        ("livery", list(enumerate(pack.livery))),
        ("heat", [(k + 1, s) for k, s in enumerate(pack.heat)]),
        ("terminal", list(enumerate(pack.terminal))),
    ):
        lines += [
            "",
            f"| {title} | hex | J_HK | M | asked | h | xterm |",
            "|---|---|---|---|---|---|---|",
        ]
        lines += [_row(index, s) for index, s in ramp_rows]
    lines += ["", "## faults", "", *([f"- {f}" for f in faults] or ["- none"])]
    lines += ["", "## advisories", "", *([f"- {a}" for a in advisories] or ["- none"])]
    return "\n".join(lines) + "\n", faults
