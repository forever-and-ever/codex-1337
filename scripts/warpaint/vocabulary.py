"""The paint vocabulary and every pair it is painted in, as the TUI declares them.

`roles.json` is generated from `codex-rs/tui/src/warpaint/paint.rs` by the TUI's
`roles_json_matches_the_declaration` test; this module restates nothing. A law that names a
member the TUI no longer declares fails here, at import.
"""

from __future__ import annotations

import json
from enum import Enum
from pathlib import Path


class Paint(Enum):
    """A member of a paint family. Its value is its key in packs, specs and roles.json; it prints
    qualified by family, because grounds and figures share keys."""

    def __str__(self) -> str:
        return f"{type(self).__name__.lower()}.{self.value}"


_DECLARED = json.loads((Path(__file__).resolve().parent / "roles.json").read_text())


def _family(name: str) -> type[Paint]:
    return Paint(name.capitalize(), [(key.upper(), key) for key in _DECLARED[name]])


Ground = _family("ground")
Figure = _family("figure")
Syntax = _family("syntax")
Bracket = _family("bracket")

HEAT_STEPS: int = _DECLARED["ramps"]["heat"]["len"]
TERMINAL_SLOTS: int = _DECLARED["ramps"]["terminal"]["len"]
LIVERY_COUNT: tuple[int, int] = (
    _DECLARED["ramps"]["livery"]["min"],
    _DECLARED["ramps"]["livery"]["max"],
)

PAIRS: dict[Paint, tuple[Paint, ...]] = {
    family(key): tuple(Ground(ground) for ground in grounds)
    for family, table in (
        (Figure, _DECLARED["pairs"]["figure"]),
        (Syntax, _DECLARED["pairs"]["syntax"]),
        (Bracket, _DECLARED["pairs"]["bracket"]),
    )
    for key, grounds in table.items()
}
"""Every ground each figure, syntax class and bracket is painted on."""
RAMP_PAIRS: dict[str, tuple[Paint, ...]] = {
    ramp: tuple(Ground(ground) for ground in grounds)
    for ramp, grounds in _DECLARED["pairs"]["ramp"].items()
}
"""Every ground each ramp is painted on."""
