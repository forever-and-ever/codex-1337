"""The terminal: exported shimmer clips as cell planes, painted the way a GPU terminal paints
them.

Text is JetBrains Mono, falling back through fonts that hold the shimmers' symbols. Block
elements, octants, shades and the box-drawing strokes the shimmers use are drawn from their
geometry, as GPU terminals draw them, so neighboring cells join without seams.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass, fields
from functools import cache
from pathlib import Path

import numpy as np
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

# ratatui's modifier bits; a clip carrying any other modifier is refused.
BOLD = 1
DIM = 2
UNDERLINED = 8
# A color plane's entry for the terminal's default color.
DEFAULT = -1
# Font families in fallback order, as fontconfig names them.
FACES = (
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Noto Sans Symbols 2",
    "DejaVu Sans",
    "Noto Sans Math",
    "Noto Sans Symbols",
)
# Fractions of a cell each shade covers.
SHADES = {"░": 0.25, "▒": 0.5, "▓": 0.75}


@dataclass(frozen=True, slots=True, eq=False)
class Grid:
    """Cells as parallel planes: glyph indices into the rushes' symbols, colors as `0xRRGGBB` or
    `DEFAULT`, and ratatui modifier bits. A clip's planes carry a leading tick axis."""

    glyph: np.ndarray
    fg: np.ndarray
    bg: np.ndarray
    underline: np.ndarray
    modifier: np.ndarray

    def __iter__(self) -> Iterator[np.ndarray]:
        return (getattr(self, field.name) for field in fields(self))

    def __getitem__(self, index: int | list[int] | tuple[slice, slice]) -> Grid:
        return Grid(*(plane[index] for plane in self))

    @property
    def shape(self) -> tuple[int, ...]:
        return self.glyph.shape

    @property
    def inked(self) -> np.ndarray:
        """Where a cell shows anything but the canvas."""
        return (self.glyph != 0) | (self.bg != DEFAULT)

    def where(self, mask: np.ndarray, other: Grid) -> Grid:
        """This grid with `other`'s cells wherever `mask` holds."""
        return Grid(*(np.where(mask, b, a) for a, b in zip(self, other, strict=True)))


@dataclass(frozen=True, slots=True, eq=False)
class Clip:
    """A stock shimmer's exported frames, one per tick."""

    shimmer: str
    blurb: str
    credit: str
    ticks: Grid

    def __len__(self) -> int:
        return self.ticks.shape[0]

    def __getitem__(self, tick: int) -> Grid:
        return self.ticks[min(max(tick, 0), len(self) - 1)]

    @property
    def used(self) -> int:
        """Columns any frame inks, counted from the left."""
        return int(np.flatnonzero(self.ticks.inked.any(axis=(0, 1)))[-1]) + 1


@dataclass(frozen=True, slots=True)
class Rushes:
    """Every clip of one export, over one symbol table."""

    symbols: tuple[str, ...]
    octants: Mapping[str, int]
    clips: tuple[Clip, ...]

    @classmethod
    def load(cls, path: Path, cols: int) -> Rushes:
        raw = json.loads(path.read_text())
        index = {" ": 0}
        clips = []
        for clip in raw["clips"]:
            frames = clip["frames"]
            shape = (len(frames), len(frames[0]), cols)
            ticks = Grid(
                np.zeros(shape, np.uint16),
                np.full(shape, DEFAULT, np.int32),
                np.full(shape, DEFAULT, np.int32),
                np.full(shape, DEFAULT, np.int32),
                np.zeros(shape, np.uint16),
            )
            for t, frame in enumerate(frames):
                for y, row in enumerate(frame):
                    for x, (symbol, fg, bg, modifier, underline) in enumerate(row):
                        if modifier & ~(BOLD | DIM | UNDERLINED):
                            raise ValueError(
                                f"{clip['shimmer']} paints modifiers {modifier:#x}"
                            )
                        ticks.glyph[t, y, x] = index.setdefault(symbol, len(index))
                        ticks.fg[t, y, x] = DEFAULT if fg is None else fg
                        ticks.bg[t, y, x] = DEFAULT if bg is None else bg
                        ticks.underline[t, y, x] = (
                            DEFAULT if underline is None else underline
                        )
                        ticks.modifier[t, y, x] = modifier
            clips.append(Clip(clip["shimmer"], clip["blurb"], clip["credit"], ticks))
        octants = {symbol: bits for bits, symbol in enumerate(raw["octants"])}
        return cls(tuple(index), octants, tuple(clips))


@cache
def face(family: str, bold: bool) -> Path | None:
    """The font file fontconfig holds for `family`, if it holds that family at all."""
    weight = "bold" if bold else "regular"
    found = subprocess.run(
        ["fc-match", "--format=%{file}\t%{family}", f"{family}:weight={weight}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    file, families = found.split("\t")
    return Path(file) if family in families.split(",") else None


@cache
def coverage(file: Path) -> frozenset[int]:
    cmap = TTFont(file, lazy=True).getBestCmap()
    assert cmap is not None, f"{file} maps no Unicode"
    return frozenset(cmap)


def geometry(
    symbol: str, octants: Mapping[str, int], w: int, h: int
) -> np.ndarray | None:
    """Coverage of a glyph terminals draw from geometry, or None for a font's glyph."""
    mask = np.zeros((h, w), np.float32)
    code = ord(symbol) if len(symbol) == 1 else -1
    stroke = max(1, round(w / 9))
    x0, y0 = (w - stroke) // 2, (h - stroke) // 2
    match symbol:
        case _ if symbol in octants:
            bits = octants[symbol]
            for pixel in range(8):
                if bits >> pixel & 1:
                    y, x = divmod(pixel, 2)
                    mask[
                        y * h // 4 : (y + 1) * h // 4, x * w // 2 : (x + 1) * w // 2
                    ] = 1
        case _ if 0x2581 <= code <= 0x2588:
            mask[h - round((code - 0x2580) * h / 8) :] = 1
        case _ if 0x2589 <= code <= 0x258F:
            mask[:, : round((0x2590 - code) * w / 8)] = 1
        case "▔":
            mask[: round(h / 8)] = 1
        case "▕":
            mask[:, w - round(w / 8) :] = 1
        case _ if symbol in SHADES:
            mask[:] = SHADES[symbol]
        case "│":
            mask[:, x0 : x0 + stroke] = 1
        case "─":
            mask[y0 : y0 + stroke] = 1
        case "┼":
            mask[:, x0 : x0 + stroke] = 1
            mask[y0 : y0 + stroke] = 1
        case "┆":
            for dash in range(3):
                top = dash * h // 3 + h // 12
                mask[top : top + h // 6, x0 : x0 + stroke] = 1
        case "╫":
            gap = max(stroke, round(w / 6))
            mask[:, x0 - gap : x0 - gap + stroke] = 1
            mask[:, x0 + gap : x0 + gap + stroke] = 1
            mask[y0 : y0 + stroke] = 1
        case _:
            return None
    return mask


def rgb(plane: np.ndarray, default: np.ndarray) -> np.ndarray:
    """A color plane as channels in [0, 1], `default` wherever it holds `DEFAULT`."""
    channels = np.stack(
        [plane >> 16 & 255, plane >> 8 & 255, plane & 255], -1
    ) / np.float32(255)
    return np.where((plane == DEFAULT)[..., None], default, channels).astype(np.float32)


class Terminal:
    """Paints grids at `cell` pixels a column. A cell is twice as tall as it is wide, so octant
    pixels are square, as the shimmers assume."""

    def __init__(
        self,
        symbols: Sequence[str],
        octants: Mapping[str, int],
        cell: int,
        ink: Sequence[float],
        canvas: Sequence[float],
    ) -> None:
        self.w, self.h = cell, 2 * cell
        self.ink = np.asarray(ink, np.float32) / 255
        self.canvas = np.asarray(canvas, np.float32) / 255
        primary = face(FACES[0], False)
        assert primary is not None, "fontconfig holds no JetBrains Mono"
        self.size = self.w * 1000 / ImageFont.truetype(primary, 1000).getlength("0")
        ascent, descent = ImageFont.truetype(primary, self.size).getmetrics()
        self.baseline = (self.h - ascent - descent) / 2 + ascent
        self.atlas = np.stack(
            [
                [self.glyph(symbol, octants, bold) for bold in (False, True)]
                for symbol in symbols
            ]
        )
        self.rule = np.zeros((self.h, self.w), np.float32)
        top = round(self.baseline + 0.12 * self.size)
        self.rule[top : top + max(1, round(0.06 * self.size))] = 1

    def glyph(self, symbol: str, octants: Mapping[str, int], bold: bool) -> np.ndarray:
        drawn = geometry(symbol, octants, self.w, self.h)
        if drawn is not None:
            return drawn
        image = Image.new("L", (self.w, self.h))
        if symbol.strip():
            for family in FACES:
                file = face(family, bold) or face(family, False)
                if file is not None and all(ord(c) in coverage(file) for c in symbol):
                    break
            else:
                raise LookupError(f"no font holds {symbol!r}")
            font = ImageFont.truetype(file, self.size)
            advance = font.getlength(symbol)
            if advance > self.w:
                font = ImageFont.truetype(file, self.size * self.w / advance)
                advance = self.w
            ImageDraw.Draw(image).text(
                ((self.w - advance) / 2, self.baseline), symbol, 255, font, anchor="ls"
            )
        return np.asarray(image, np.float32) / 255

    def paint(self, grid: Grid) -> np.ndarray:
        """`grid` as an sRGB image of `rows * h` by `cols * w` pixels, channels in [0, 1]."""
        rows, cols = grid.shape
        mask = self.atlas[grid.glyph, (grid.modifier & BOLD).astype(np.intp)]
        bg = rgb(grid.bg, self.canvas)
        fg = rgb(grid.fg, self.ink)
        fg = np.where((grid.modifier & DIM).astype(bool)[..., None], (fg + bg) / 2, fg)
        image = bg[:, :, None, None] + (fg - bg)[:, :, None, None] * mask[..., None]
        underlined = (grid.modifier & UNDERLINED).astype(bool)
        if underlined.any():
            rule = self.rule * underlined[:, :, None, None]
            color = rgb(grid.underline, fg)
            image += (color[:, :, None, None] - image) * rule[..., None]
        return image.transpose(0, 2, 1, 3, 4).reshape(rows * self.h, cols * self.w, 3)
