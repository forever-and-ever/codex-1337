#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["colour-science==0.4.7", "numpy>=2.3", "pillow"]
# ///
"""Render live terminal captures; audit counts are display cells per source."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import unicodedata
from collections import Counter
from collections.abc import Iterator
from dataclasses import dataclass
from enum import StrEnum
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

sys.dont_write_bytecode = True  # sibling modules stay uncompiled in the worktree

import appearance
import packs
from laws import Invalid
from proof import xterm

type RGB = tuple[int, int, int]

FAMILIES = {
    **{family: tuple(f"{family}:{key}" for key in keys) for family, keys in packs.FAMILIES.items()},
}


class Mod(StrEnum):
    BOLD = "bold"
    DIM = "dim"
    ITALIC = "italic"
    UNDERLINE = "underline"
    REVERSE = "reverse"
    STRIKE = "strike"


def rgb(value: object, where: str) -> RGB:
    if not isinstance(value, str) or not re.fullmatch(r"#[0-9a-fA-F]{6}", value):
        raise Invalid(f"{where}: expected #rrggbb, got {value!r}")
    return (int(value[1:3], 16), int(value[3:5], 16), int(value[5:7], 16))


def hex_color(color: RGB) -> str:
    return "#{:02x}{:02x}{:02x}".format(*color)


def apca(text: RGB, background: RGB) -> float:
    return float(appearance.apca(np.array(text), np.array(background)))


@dataclass(frozen=True, slots=True)
class Color:
    source: str
    value: RGB


@dataclass(frozen=True, slots=True)
class Pack:
    name: str
    roles: dict[str, RGB]
    """Every color a capture may name that the pack defines, `family:key` and ramps alike."""
    livery: tuple[RGB, ...]
    source: packs.Pack

    @classmethod
    def load(cls, path: Path) -> Pack:
        source = packs.Pack.load(path)
        return cls(
            source.name,
            {name: rgb(value, f"{path}: {name}") for name, value in source.colors.items()},
            tuple(rgb(value, f"{path}: livery {i}") for i, value in enumerate(source.ramps["livery"])),
            source,
        )

    def resolve(self, value: object, foreground: bool, where: str) -> Color:
        if not isinstance(value, str):
            raise Invalid(f"{where}: expected color string, got {value!r}")
        return Color(value, rgb(self.source.resolve(value, foreground), where))


def glyphs(text: str) -> Iterator[tuple[str, int]]:
    """Terminal clusters: marks, emoji selectors/ZWJ, modifiers and flag pairs."""
    cluster = ""
    width = 0
    regional = 0
    for char in text:
        code = ord(char)
        category = unicodedata.category(char)
        if category in {"Cc", "Cs"} or (category == "Cf" and char != "\u200d"):
            raise Invalid(f"unsupported terminal control U+{code:04X}")
        mark = category.startswith("M") or 0x1F3FB <= code <= 0x1F3FF
        flag = 0x1F1E6 <= code <= 0x1F1FF
        joined = cluster.endswith("\u200d")
        if cluster and not (
            mark or char == "\u200d" or joined or flag and regional == 1
        ):
            if width == 0:
                raise Invalid("combining mark has no base glyph in its style run")
            yield cluster, width
            cluster, width, regional = "", 0, 0
        cluster += char
        if char == "\ufe0f" or char == "\u20e3" or flag or joined:
            width = max(width, 2)
        elif not mark and char != "\u200d":
            width = max(
                width, 2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1
            )
        if flag:
            regional += 1
    if cluster:
        if width == 0:
            raise Invalid("combining mark has no base glyph in its style run")
        yield cluster, width


@dataclass(frozen=True, slots=True)
class Run:
    glyphs: tuple[tuple[str, int], ...]
    fg: Color
    bg: Color
    mods: frozenset[Mod]

    @property
    def width(self) -> int:
        return sum(width for _, width in self.glyphs)


@dataclass(frozen=True, slots=True)
class Capture:
    name: str
    width: int
    rows: tuple[tuple[Run, ...], ...]

    @classmethod
    def load(cls, path: Path, pack: Pack) -> Capture:
        data: object = json.loads(path.read_text())
        if not isinstance(data, dict) or set(data) != {
            "name",
            "width",
            "height",
            "rows",
        }:
            raise Invalid(f"{path}: expected name, width, height and rows")
        name, width, height, rows = (
            data["name"],
            data["width"],
            data["height"],
            data["rows"],
        )
        if not isinstance(name, str) or not name.strip():
            raise Invalid(f"{path}: name must be a nonempty string")
        if type(width) is not int or type(height) is not int or width < 1 or height < 1:
            raise Invalid(f"{path}: width and height must be positive integers")
        if not isinstance(rows, list) or len(rows) != height:
            raise Invalid(f"{path}: expected exactly {height} rows")
        parsed: list[tuple[Run, ...]] = []
        for row_index, row in enumerate(rows):
            where = f"{path}: row {row_index + 1}"
            if not isinstance(row, list):
                raise Invalid(f"{where}: expected list of style runs")
            runs: list[Run] = []
            for run_index, raw in enumerate(row):
                at = f"{where}, run {run_index + 1}"
                if not isinstance(raw, dict) or set(raw) != {
                    "text",
                    "fg",
                    "bg",
                    "mods",
                }:
                    raise Invalid(f"{at}: expected text, fg, bg and mods")
                if not isinstance(raw["text"], str) or not isinstance(
                    raw["mods"], list
                ):
                    raise Invalid(f"{at}: text must be a string and mods a list")
                if any(
                    not isinstance(m, str) or m not in Mod for m in raw["mods"]
                ) or len(set(raw["mods"])) != len(raw["mods"]):
                    raise Invalid(
                        f"{at}: mods must be a subset of {[m.value for m in Mod]}"
                    )
                try:
                    cells = tuple(glyphs(raw["text"]))
                except Invalid as error:
                    raise Invalid(f"{at}: {error}") from error
                runs.append(
                    Run(
                        cells,
                        pack.resolve(raw["fg"], True, f"{at} fg"),
                        pack.resolve(raw["bg"], False, f"{at} bg"),
                        frozenset(Mod(m) for m in (*raw["mods"], *pack.source.style.get(raw["fg"], ()))),
                    )
                )
            actual = sum(run.width for run in runs)
            if actual != width:
                raise Invalid(
                    f"{where}: expected {width} display columns, found {actual}; pad rows with spaces"
                )
            parsed.append(tuple(runs))
        return cls(name, width, tuple(parsed))


@dataclass(frozen=True, slots=True)
class Fonts:
    faces: dict[tuple[bool, bool], ImageFont.FreeTypeFont]
    coverage: tuple[tuple[int, int], ...]
    fallbacks: dict[tuple[str, bool, bool], ImageFont.FreeTypeFont]
    width: int
    height: int
    baseline: int

    @classmethod
    def load(cls) -> Fonts:
        faces: dict[tuple[bool, bool], ImageFont.FreeTypeFont] = {}
        for bold, italic, style in (
            (False, False, "Regular"),
            (True, False, "Bold"),
            (False, True, "Italic"),
            (True, True, "Bold Italic"),
        ):
            found = subprocess.run(
                [
                    "fc-match",
                    "-f",
                    "%{family}\n%{file}\n%{charset}",
                    f"JetBrains Mono:style={style}",
                ],
                check=True,
                text=True,
                capture_output=True,
            ).stdout
            family, filename, charset = found.split("\n", 2)
            if "JetBrains Mono" not in family:
                raise Invalid(
                    f"JetBrains Mono {style} not installed (fc-match returned {family!r})"
                )
            faces[bold, italic] = ImageFont.truetype(filename, 18)
        normal = faces[False, False]
        ascent, descent = normal.getmetrics()
        return cls(
            faces,
            cls.ranges(charset),
            {},
            round(normal.getlength("M")),
            ascent + descent + 2,
            ascent + 1,
        )

    @staticmethod
    def ranges(charset: str) -> tuple[tuple[int, int], ...]:
        result: list[tuple[int, int]] = []
        for interval in charset.split():
            bounds = interval.split("-")
            result.append((int(bounds[0], 16), int(bounds[-1], 16)))
        return tuple(result)

    def face(self, text: str, mods: frozenset[Mod]) -> ImageFont.FreeTypeFont:
        bold, italic = Mod.BOLD in mods, Mod.ITALIC in mods
        codes = [ord(c) for c in text if c not in {"\u200d", "\ufe0f", "\ufe0e"}]
        if all(
            any(low <= code <= high for low, high in self.coverage) for code in codes
        ):
            return self.faces[bold, italic]
        key = (text, bold, italic)
        if key not in self.fallbacks:
            style = (
                "Bold Italic"
                if bold and italic
                else "Bold"
                if bold
                else "Italic"
                if italic
                else "Regular"
            )
            charset = " ".join(f"{code:x}" for code in codes)
            found = subprocess.run(
                [
                    "fc-match",
                    "-f",
                    "%{file}\n%{pixelsize}\n%{charset}",
                    f"JetBrains Mono:style={style}:charset={charset}",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            filename, pixelsize, coverage = found.split("\n", 2)
            ranges = self.ranges(coverage)
            if not all(
                any(low <= code <= high for low, high in ranges) for code in codes
            ):
                raise Invalid(f"no installed font covers glyph {text!r}")
            try:
                font = ImageFont.truetype(filename, 18)
            except OSError:
                # Color emoji fonts expose a fixed bitmap strike, not scalable outlines.
                font = ImageFont.truetype(filename, round(float(pixelsize)))
            self.fallbacks[key] = font
        return self.fallbacks[key]


def paint(
    capture: Capture, pack: Pack, fonts: Fonts, audit: bool = False
) -> tuple[Image.Image, Counter[str]]:
    image = Image.new(
        "RGB",
        (capture.width * fonts.width, len(capture.rows) * fonts.height),
        pack.roles["ground:canvas"],
    )
    counts: Counter[str] = Counter()
    for row_index, row in enumerate(capture.rows):
        x, y = 0, row_index * fonts.height
        for run in row:
            fg, bg = run.fg.value, run.bg.value
            if Mod.REVERSE in run.mods:
                fg, bg = bg, fg
            if Mod.DIM in run.mods:
                fg = tuple(
                    round(f * 0.45 + b * 0.55) for f, b in zip(fg, bg, strict=True)
                )
            sources = {
                c.source
                for c in (run.fg, run.bg)
                if c.source.startswith(("rgb:", "xterm:"))
            }
            for source in sources:
                counts[source] += run.width
            for text, columns in run.glyphs:
                font = fonts.face(text, run.mods)
                # Clip each cluster to its owned columns; italic overhang cannot leak.
                tile = Image.new("RGB", (columns * fonts.width, fonts.height), bg)
                draw = ImageDraw.Draw(tile)
                if font.size <= 18:
                    draw.text(
                        (0, fonts.baseline), text, font=font, fill=fg, anchor="ls"
                    )
                else:
                    left, top, right, bottom = (round(c) for c in font.getbbox(text))
                    glyph = Image.new("RGBA", (right - left, bottom - top))
                    ImageDraw.Draw(glyph).text(
                        (-left, -top), text, font=font, fill=fg, embedded_color=True
                    )
                    glyph.thumbnail(
                        (tile.width, fonts.height - 4), Image.Resampling.LANCZOS
                    )
                    tile.paste(
                        glyph,
                        (
                            (tile.width - glyph.width) // 2,
                            (tile.height - glyph.height) // 2,
                        ),
                        glyph,
                    )
                if Mod.UNDERLINE in run.mods:
                    draw.line(
                        (0, fonts.baseline, tile.width - 1, fonts.baseline), fill=fg
                    )
                if Mod.STRIKE in run.mods:
                    strike = fonts.baseline - round(
                        fonts.faces[False, False].size * 0.35
                    )
                    draw.line((0, strike, tile.width - 1, strike), fill=fg)
                if audit and sources:
                    for column in range(columns):
                        right = (column + 1) * fonts.width - 1
                        draw.line(
                            (right - 2, 0, right, 0, right, 2), fill="#ff8c42", width=1
                        )
                image.paste(tile, (x, y))
                x += tile.width
    return image, counts


def chips(pack: Pack, fonts: Fonts) -> Image.Image:
    row_height = fonts.height + 10
    column_width = fonts.width * 53
    groups = [
        *FAMILIES.items(),
        *((ramp, tuple(f"{ramp}:{i}" for i in range(len(pack.source.ramps[ramp])))) for ramp in packs.RAMPS),
    ]
    height = (max(len(roles) for _, roles in groups) + 3) * row_height
    image = Image.new("RGB", (column_width * 4, height * 2), pack.roles["ground:canvas"])
    draw = ImageDraw.Draw(image)
    for index, (family, roles) in enumerate(groups):
        x, y = index % 4 * column_width + 18, index // 4 * height + 15
        draw.text(
            (x, y),
            family.upper(),
            font=fonts.faces[True, False],
            fill=pack.roles["figure:ink"],
        )
        draw.text(
            (x, y + row_height),
            "role / sRGB / APCA Lc / xterm-256",
            font=fonts.faces[False, False],
            fill=pack.roles["figure:ink_muted"],
        )
        for n, role in enumerate(roles):
            color = pack.resolve(role, True, "chip").value
            top = y + (n + 2) * row_height
            draw.rectangle(
                (x, top, x + 35, top + fonts.height - 1),
                fill=color,
                outline=pack.roles["figure:rule"],
            )
            label = f"{role:<15} {hex_color(color)} {apca(color, pack.roles['ground:canvas']):>6.1f} {xterm(color):>3}"
            draw.text(
                (x + 46, top),
                label,
                font=fonts.faces[False, False],
                fill=pack.roles["figure:ink"],
            )
    return image


@dataclass(frozen=True, slots=True)
class Arguments:
    command: str
    pack: Path | None = None
    packs: list[Path] | None = None
    capture: Path | None = None
    out: Path = Path("swatch.png")
    audit: bool = False
    scale: int = 1


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="command", required=True)
    render = subs.add_parser("render", help="render a terminal capture")
    render.add_argument(
        "--audit",
        action="store_true",
        help="orange corner marks identify literal-color cells; count each source once per cell",
    )
    render.add_argument("--scale", type=int, default=1)
    contact = subs.add_parser("contact", help="compare packs side by side")
    contact.add_argument("--packs", type=Path, nargs="+", required=True)
    chip = subs.add_parser("chips", help="show every semantic role")
    for sub in (render, chip):
        sub.add_argument("--pack", type=Path, required=True)
    for sub in (render, contact):
        sub.add_argument("--capture", type=Path, required=True)
    for sub in (render, chip, contact):
        sub.add_argument("--out", type=Path, required=True)
    args = Arguments(**vars(parser.parse_args()))
    if args.scale < 1:
        parser.error("--scale must be a positive integer")
    try:
        fonts = Fonts.load()
        if args.command == "contact":
            assert args.packs is not None and args.capture is not None
            panels: list[tuple[Pack, Image.Image]] = []
            for path in args.packs:
                pack = Pack.load(path)
                panel, _ = paint(Capture.load(args.capture, pack), pack, fonts)
                panels.append((pack, panel))
            width = sum(panel.width for _, panel in panels) + 16 * (len(panels) - 1)
            result = Image.new(
                "RGB",
                (width, panels[0][1].height + fonts.height * 2),
                panels[0][0].roles["ground:canvas"],
            )
            x = 0
            for pack, panel in panels:
                result.paste(panel, (x, fonts.height * 2))
                ImageDraw.Draw(result).text(
                    (x + 8, 4),
                    pack.name,
                    font=fonts.faces[True, False],
                    fill=pack.roles["figure:ink"],
                )
                x += panel.width + 16
        else:
            assert args.pack is not None
            pack = Pack.load(args.pack)
            if args.command == "chips":
                result = chips(pack, fonts)
            else:
                assert args.capture is not None
                capture = Capture.load(args.capture, pack)
                result, counts = paint(capture, pack, fonts, args.audit)
                if args.audit:
                    print("Literal color cells (each source counted once per cell):")
                    for source, count in sorted(counts.items()):
                        print(f"  {source}: {count}")
                    if not counts:
                        print("  none")
                if args.scale != 1:
                    result = result.resize(
                        (result.width * args.scale, result.height * args.scale),
                        Image.Resampling.NEAREST,
                    )
        args.out.parent.mkdir(parents=True, exist_ok=True)
        result.save(args.out, format="PNG")
        print(f"{args.out}: {result.width}×{result.height}")
    except (Invalid, OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(2, f"swatch: {error}\n")


if __name__ == "__main__":
    main()
