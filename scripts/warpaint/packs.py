"""Packs as the renderers read them, and the colors a capture may name.

A capture names colors the way the TUI's cells hold them before binding: `ground:<key>`,
`figure:<key>` and `syntax:<key>` for tokens, `heat:<step>` and `livery:<slot>` for ramps,
`terminal:<index>` for tool output's sixteen colors, `xterm:<index>` for its extended ones,
`rgb:#rrggbb` for literals, and `reset` for the default ink or canvas.

A pack's `[style]` gives syntax classes modifiers, which the TUI adds wherever a cell's foreground
is the class; captures, taken before binding, never carry them.
"""

from __future__ import annotations

import json
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path

_DECLARED = json.loads((Path(__file__).resolve().parent / "roles.json").read_text())
FAMILIES: dict[str, tuple[str, ...]] = {
    family: tuple(_DECLARED[family])
    for family in ("ground", "figure", "syntax", "bracket")
}
RAMPS = ("livery", "heat", "terminal")
STYLES = ("bold", "italic")


def _xterm(index: int) -> str:
    if index >= 232:
        gray = 8 + 10 * (index - 232)
        return f"#{gray:02x}{gray:02x}{gray:02x}"
    levels = (0, 95, 135, 175, 215, 255)
    n = index - 16
    return "#" + "".join(f"{levels[c]:02x}" for c in (n // 36, n // 6 % 6, n % 6))


@dataclass(frozen=True, slots=True)
class Pack:
    name: str
    colors: dict[str, str]
    """`family:key` to `#rrggbb`."""
    ramps: dict[str, tuple[str, ...]]
    style: dict[str, frozenset[str]]
    """`syntax:key` to the modifiers the TUI adds to that class."""

    @classmethod
    def load(cls, path: Path) -> Pack:
        raw = tomllib.loads(path.read_text())
        style = raw.pop("style", {})
        if set(raw) != {*FAMILIES, *RAMPS}:
            raise SystemExit(
                f"{path}: expected {sorted({*FAMILIES, *RAMPS})}, found {sorted(raw)}"
            )
        for family, keys in FAMILIES.items():
            if set(raw[family]) != set(keys):
                raise SystemExit(f"{path}: [{family}] differs from roles.json")
        for key, modifiers in style.items():
            if (
                key not in FAMILIES["syntax"]
                or not modifiers
                or not set(modifiers) <= set(STYLES)
            ):
                raise SystemExit(
                    f"{path}: [style] {key} must be a syntax class given some of {list(STYLES)}"
                )
        return cls(
            path.stem,
            {
                f"{family}:{key}": raw[family][key]
                for family, keys in FAMILIES.items()
                for key in keys
            },
            {ramp: tuple(raw[ramp]) for ramp in RAMPS},
            {f"syntax:{key}": frozenset(modifiers) for key, modifiers in style.items()},
        )

    def resolve(self, color: str, foreground: bool) -> str:
        if color == "reset":
            return self.colors["figure:ink" if foreground else "ground:canvas"]
        if color in self.colors:
            return self.colors[color]
        kind, _, value = color.partition(":")
        if kind == "rgb" and re.fullmatch(r"#[0-9a-fA-F]{6}", value):
            return value.lower()
        if kind == "xterm" and value.isdigit() and 16 <= int(value) <= 255:
            return _xterm(int(value))
        if kind in RAMPS and value.isdigit():
            ramp = self.ramps[kind]
            index = int(value) % len(ramp) if kind == "livery" else int(value)
            if index < len(ramp):
                return ramp[index]
        raise SystemExit(f"{self.name}: cannot resolve color {color!r}")
