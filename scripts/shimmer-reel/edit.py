"""The edit: `reel.toml` parsed into shots on the score's beat grid.

A shot shows one take (a stock shimmer playing a synthetic turn in the live rows or the status
row alone), a roll of status rows, or a card of text. Each shot ends in a cut into the next,
centered on the beat where the shots meet; the last shot's cut fades the film out.
"""

from __future__ import annotations

import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum
from itertools import pairwise
from pathlib import Path

from turn import Busy, Reach, Segment, Wait, Work


class Framing(StrEnum):
    """How a take in the live rows is framed: still, pushing in slowly, close on the status
    row, or moving from still to close over the shot."""

    STILL = "still"
    PUSH = "push"
    CLOSE = "close"
    DOLLY = "dolly"


class Title(StrEnum):
    """What a single take's lower third names: nothing, the shimmer, or the shimmer, its blurb
    and its author."""

    NONE = "none"
    NAME = "name"
    FULL = "full"


class Kind(StrEnum):
    """A cut's look. Cell cuts join two single takes of the same rows cell by cell."""

    FADE = "fade"
    CRT = "crt"
    GLITCH = "glitch"
    PUNCH = "punch"
    MELT = "melt"
    SCAN = "scan"
    IRIS = "iris"
    SHATTER = "shatter"
    FLAP = "flap"
    DECRYPT = "decrypt"
    SHRED = "shred"


CELL_CUTS = frozenset({Kind.FLAP, Kind.DECRYPT, Kind.SHRED})


@dataclass(frozen=True, slots=True)
class Live:
    """The live rows around a running tool: its command and its output line."""

    command: str
    output: str


@dataclass(frozen=True, slots=True)
class Take:
    """A stock shimmer playing `turn` from `preroll` beats before its shot begins, in the live
    rows or, without them, in the status row alone; the status timer starts at `timer`."""

    shimmer: str
    rows: Live | None
    turn: tuple[Segment, ...]
    preroll: float
    timer: int


@dataclass(frozen=True, slots=True)
class Single:
    """One take, named in a lower third or captioned beneath."""

    take: Take
    framing: Framing
    title: Title
    caption: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Roll:
    """Status rows in `columns` columns, each a shimmer working under its own name."""

    takes: tuple[Take, ...]
    columns: int


@dataclass(frozen=True, slots=True)
class Card:
    """Lines of text, the first the largest."""

    lines: tuple[str, ...]


type Picture = Single | Roll | Card


@dataclass(frozen=True, slots=True)
class Cut:
    kind: Kind
    beats: float


@dataclass(frozen=True, slots=True)
class Shot:
    picture: Picture
    beats: float
    energy: int
    cut: Cut

    @property
    def takes(self) -> tuple[Take, ...]:
        match self.picture:
            case Single(take):
                return (take,)
            case Roll(takes):
                return takes
            case Card():
                return ()


@dataclass(frozen=True, slots=True)
class Edit:
    size: tuple[int, int]
    fps: int
    bpm: float
    cols: int
    cell: int
    ink: tuple[int, int, int]
    canvas: tuple[int, int, int]
    glow: float
    fade_in: float
    shots: tuple[Shot, ...]

    @property
    def beat(self) -> float:
        """Seconds a beat."""
        return 60 / self.bpm

    @property
    def beats(self) -> float:
        return sum(shot.beats for shot in self.shots)

    @classmethod
    def read(cls, path: Path) -> Edit:
        raw = tomllib.loads(path.read_text())
        film = raw["film"]
        beat = 60 / film["bpm"]
        shots = tuple(shot(entry, beat) for entry in raw["shot"])
        for before, after in pairwise(shots):
            for take in after.takes:
                if take.preroll < before.cut.beats / 2:
                    raise ValueError(
                        f"{take.shimmer} needs a preroll of half the cut into it"
                    )
            if before.cut.kind in CELL_CUTS and not (
                isinstance(before.picture, Single)
                and isinstance(after.picture, Single)
                and (before.picture.take.rows is None)
                == (after.picture.take.rows is None)
            ):
                raise ValueError(
                    f"a {before.cut.kind} cut joins single takes of the same rows"
                )
        return cls(
            size=(film["size"][0], film["size"][1]),
            fps=film["fps"],
            bpm=film["bpm"],
            cols=film["cols"],
            cell=film["cell"],
            ink=(film["ink"][0], film["ink"][1], film["ink"][2]),
            canvas=(film["canvas"][0], film["canvas"][1], film["canvas"][2]),
            glow=film["glow"],
            fade_in=film["fade_in"],
            shots=shots,
        )


def number(
    entry: Mapping[str, object], key: str, default: float | None = None
) -> float:
    match entry.get(key, default):
        case float() | int() as value:
            return float(value)
        case value:
            raise ValueError(f"{key} is a number, not {value!r}: {entry}")


def words(entry: Mapping[str, object], key: str) -> tuple[str, ...]:
    match entry.get(key, []):
        case list(values) if all(isinstance(value, str) for value in values):
            return tuple(map(str, values))
        case value:
            raise ValueError(f"{key} is a list of strings, not {value!r}: {entry}")


def turn(
    entries: list[object], beat: float, header: str | None = None
) -> tuple[Segment, ...]:
    """Segments from `{header, beats, busy | wait | reach}` tables; `header` stands in for a
    missing one."""
    segments = []
    for entry in entries:
        match entry:
            case {"busy": float() | int() as rate}:
                work: Work = Busy(float(rate))
            case {"wait": float() | int() as tokens}:
                work = Wait(float(tokens))
            case {"reach": int(tokens)}:
                work = Reach(tokens)
            case _:
                raise ValueError(f"a segment works, waits or reaches: {entry}")
        match entry:
            case {"header": str(title), "beats": float() | int() as beats}:
                segments.append(Segment(title, beats * beat, work))
            case {"beats": float() | int() as beats} if header is not None:
                segments.append(Segment(header, beats * beat, work))
            case _:
                raise ValueError(f"a segment needs a header and beats: {entry}")
    return tuple(segments)


def shot(entry: dict[str, object], beat: float) -> Shot:
    preroll = number(entry, "preroll", 0)
    match entry:
        case {"shimmer": str(name), "turn": list(entries)}:
            match entry:
                case {"rows": "status"}:
                    rows = None
                case {"command": str(command), "output": str(output)}:
                    rows = Live(command, output)
                case _:
                    raise ValueError(
                        f"live rows need a command and its output: {entry}"
                    )
            take = Take(
                name,
                rows,
                turn(entries, beat),
                preroll,
                int(number(entry, "timer", 63)),
            )
            picture: Picture = Single(
                take,
                Framing(entry.get("framing", Framing.STILL)),
                Title(entry.get("title", Title.FULL)),
                words(entry, "caption"),
            )
        case {"roll": list(), "turn": list(entries)}:
            picture = Roll(
                tuple(
                    # Staggered starts keep the rows from moving in lockstep.
                    Take(name, None, turn(entries, beat, name), preroll + 0.37 * i, 63)
                    for i, name in enumerate(words(entry, "roll"))
                ),
                int(number(entry, "columns", 1)),
            )
        case {"card": list()}:
            picture = Card(words(entry, "card"))
        case _:
            raise ValueError(
                f"a shot is a shimmer, a roll or a card, with a turn: {entry}"
            )
    match entry.get("cut"):
        case {"kind": str(kind), "beats": float() | int() as cut}:
            return Shot(
                picture,
                number(entry, "beats"),
                int(number(entry, "energy", 1)),
                Cut(Kind(kind), float(cut)),
            )
        case _:
            raise ValueError(f"a shot needs a cut: {entry}")
