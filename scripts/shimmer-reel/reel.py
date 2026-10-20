#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = ["fonttools", "numpy", "opencv-python-headless", "pillow", "pyloudnorm", "scipy"]
# ///
"""The shimmer reel: stock shimmers playing synthetic turns, exported from codex-tui itself, cut
together on a beat grid and scored.

`reel.toml` is the edit. `reel.py render` films it; `reel.py still` paints chosen instants of it;
`reel.py score` writes its music alone; `reel.py sheet` paints contact sheets of stock shimmers
for choosing among them.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import multiprocessing
import os
import re
import shutil
import subprocess
import tempfile
from collections.abc import Sequence
from dataclasses import dataclass
from functools import cache
from itertools import accumulate
from pathlib import Path

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont

import score
from cells import FACES, Clip, Grid, Rushes, Terminal, face
from cuts import CUTS, Camera, Side, Stage, ease
from edit import Card, Edit, Framing, Roll, Shot, Single, Take, Title
from turn import TICK, Busy, Segment, Wait, ticks

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
SHIMMERS = ROOT / "codex-rs/tui/src/shimmers"
CACHE = (
    Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    / "codex-1337/shimmer-reel"
)
# Exports kept in the cache, newest first.
KEEP = 4
# Raster pixels per plate pixel: the terminal paints at twice the film's resolution.
SS = 2
# The contact sheets' turn: work, then wait on a tool whose result drains in.
SURVEY = (
    Segment("Working", 2.5, Busy(60)),
    Segment("Tracing shimmer cadence", 3.5, Busy(80)),
    Segment("Drafting the fix", 4.0, Busy(50)),
    Segment("Waiting for background terminal", 7.0, Wait(600)),
)


def roster() -> tuple[str, ...]:
    """Stock shimmer names in `/shimmer` order, as `mod.rs` lists them and their recipes name
    them."""
    names = []
    for module in re.findall(
        r"credited\((\w+)::RECIPE", (SHIMMERS / "mod.rs").read_text()
    ):
        recipe = re.search(
            r'Recipe \{\s*name: "([^"]+)"', (SHIMMERS / f"{module}.rs").read_text()
        )
        assert recipe, f"{module}.rs declares no recipe name"
        names.append(recipe[1])
    return tuple(names)


def export(cues: dict[str, object], cols: int) -> Rushes:
    """The clips codex-tui draws for `cues`, exported once per cue sheet and shimmer source."""
    blob = json.dumps(cues, separators=(",", ":"))
    digest = hashlib.sha256(blob.encode())
    for source in sorted(SHIMMERS.glob("*.rs")):
        digest.update(source.read_bytes())
    entry = CACHE / digest.hexdigest()[:20]
    clips = entry / "clips.json"
    if not clips.exists():
        entry.mkdir(parents=True, exist_ok=True)
        (entry / "cues.json").write_text(blob)
        subprocess.run(
            ["just", "test", "-p", "codex-tui", "--lib", "--run-ignored", "only",
             "-E", "test(=shimmers::reel::export)"],
            cwd=ROOT,
            env=os.environ | {"SHIMMER_REEL": str(entry)},
            check=True,
        )  # fmt: skip
    entry.touch()
    for stale in sorted(CACHE.iterdir(), key=lambda p: p.stat().st_mtime, reverse=True)[
        KEEP:
    ]:
        shutil.rmtree(stale)
    return Rushes.load(clips, cols)


def cue(take: Take, count: int) -> dict[str, object]:
    live = take.rows
    rows = (
        "status"
        if live is None
        else {"live": {"command": live.command, "output": live.output}}
    )
    return {
        "shimmer": take.shimmer,
        "rows": rows,
        "ticks": ticks(take.turn, count, take.timer),
    }


@cache
def typeface(family: str, weight: int, size: float) -> ImageFont.FreeTypeFont:
    """`family` at `weight`, set on its weight axis when the font is variable."""
    file = face(family, weight >= 600)
    assert file is not None, f"fontconfig holds no {family}"
    font = ImageFont.truetype(file, size)
    try:
        axes = font.get_variation_axes()
    except OSError:
        return font
    weights = [
        weight if axis["name"] in (b"Weight", "Weight") else axis["default"]
        for axis in axes
    ]
    assert None not in weights, f"{family} has an axis without a default"
    font.set_variation_by_axes([float(w) for w in weights if w is not None])
    return font


@dataclass(frozen=True, slots=True)
class Line:
    """A line of lettering, its baseline starting or centered at `at`, tracked by `tracking`
    ems."""

    text: str
    family: str
    weight: int
    size: float
    color: tuple[int, int, int]
    at: tuple[float, float]
    centered: bool = False
    tracking: float = 0.0


@cache
def lettering(lines: tuple[Line, ...], size: tuple[int, int]) -> np.ndarray:
    """`lines` drawn as a premultiplied RGBA layer of the plate's size."""
    layer = Image.new("RGBA", size)
    draw = ImageDraw.Draw(layer)
    for line in lines:
        font = typeface(line.family, line.weight, line.size)
        gap = line.tracking * line.size
        x, y = line.at
        x -= (
            (font.getlength(line.text) + gap * (len(line.text) - 1)) / 2
            if line.centered
            else 0
        )
        for char in line.text:
            draw.text((x, y), char, (*line.color, 255), font, anchor="ls")
            x += font.getlength(char) + gap
    rgba = np.asarray(layer, np.float32) / 255
    return np.concatenate([rgba[..., :3] * rgba[..., 3:], rgba[..., 3:]], -1)


type Letters = tuple[np.ndarray, float]


class Cast:
    """A shot as filmed, on the timeline from the cut into it to the cut out of it."""

    def __init__(
        self, shot: Shot, start: float, end: float, cut_in: float, edit: Edit
    ) -> None:
        self.shot, self.edit, self.start, self.end = shot, edit, start, end
        self.opens, self.settled = start - cut_in / 2, start + cut_in / 2
        self.closes = (
            end if shot is edit.shots[-1] else end + shot.cut.beats * edit.beat / 2
        )
        self.canvas = np.asarray(edit.canvas, np.float32) / 255

    def side(self, t: float) -> Side:
        raise NotImplementedError

    def letters(self, t: float) -> Letters | None:
        return None

    def presence(self, t: float, delay: float) -> float:
        """Lettering's opacity: rising `delay` beats after the cut into the shot settles, gone
        half a beat before the cut out of it."""
        beat = self.edit.beat
        rise = (t - self.settled - delay * beat) / 0.15
        fall = (self.end - (self.shot.cut.beats / 2 + 0.5) * beat - t) / 0.35
        return float(np.clip(min(rise, fall), 0, 1))

    def tick(self, take: Take, t: float) -> int:
        return math.floor((t - self.start + take.preroll * self.edit.beat) / TICK) - 1


class SingleCast(Cast):
    def __init__(
        self, shot: Shot, single: Single, clip: Clip, timing: tuple[float, float, float], edit: Edit,
        terminal: Terminal, live: Camera,
    ) -> None:  # fmt: skip
        super().__init__(shot, *timing, edit)
        self.single, self.clip, self.terminal = single, clip, terminal
        w, h = edit.size
        tw, th = terminal.w, terminal.h
        self.status = clip.ticks.shape[1] - 1
        self.close = Camera(0.82, (0, (self.status + 0.5) * th), (w * 0.09, h * 0.47))
        if single.take.rows is None:
            self.base = Camera(1.25, (clip.used * tw / 2, th / 2), (w / 2, h * 0.47))
        else:
            self.base = self.close if single.framing is Framing.CLOSE else live

    def camera(self, t: float) -> Camera:
        u = (t - self.opens) / (self.closes - self.opens)
        match self.single.framing:
            case Framing.PUSH:
                return Camera(
                    self.base.scale * (1 + 0.05 * u), self.base.focus, self.base.anchor
                )
            case Framing.DOLLY:
                return self.base.mix(self.close, ease(u), self.edit.size)
            case Framing.STILL | Framing.CLOSE:
                return self.base

    def grid(self, t: float) -> Grid:
        return self.clip[self.tick(self.single.take, t)]

    def side(self, t: float) -> Side:
        camera = self.camera(t)
        grid = self.grid(t)
        plate = camera(self.terminal.paint(grid), self.edit.size, self.canvas)
        mark = camera.place(
            (self.terminal.w / 2, (self.status + 0.5) * self.terminal.h)
        )
        return Side(plate, grid, camera, mark)

    def letters(self, t: float) -> Letters | None:
        w, h = self.edit.size
        beat = self.edit.beat
        clip = self.clip
        match self.single:
            case Single(_, _, Title.NONE, ()):
                return None
            case Single(_, _, Title.NONE, caption):
                y = (
                    self.base.place((0, (self.status + 1) * self.terminal.h))[1]
                    + 0.085 * h
                )
                lines = tuple(
                    Line(text.upper(), "Adwaita Sans", 300, 0.026 * h, (150, 150, 150),
                         (w / 2, y + 0.05 * h * i), centered=True, tracking=0.42)
                    for i, text in enumerate(caption)
                )  # fmt: skip
                return lettering(lines, (w, h)), self.presence(t, 2)
            case Single(_, _, title, _):
                x, y = self.base.place((0, (self.status + 1) * self.terminal.h))
                y = max(y + 0.11 * h, h * 0.66)
                shown = t - self.settled - 0.25 * beat
                name = clip.shimmer[: max(0, int(shown * 26))]
                lines = [
                    Line(name, "Adwaita Sans", 600, 0.052 * h, (240, 240, 240), (x, y))
                ]
                if title is Title.FULL and shown > 0.55:
                    lines.append(
                        Line(
                            clip.blurb,
                            "Adwaita Sans",
                            300,
                            0.03 * h,
                            (150, 150, 150),
                            (x, y + 0.05 * h),
                        )
                    )
                if title is Title.FULL and shown > 0.9:
                    credit = f"by {clip.credit}"
                    lines.append(
                        Line(
                            credit,
                            "JetBrains Mono",
                            400,
                            0.021 * h,
                            (112, 112, 112),
                            (x, y + 0.094 * h),
                        )
                    )
                return lettering(tuple(lines), (w, h)), self.presence(t, 0.25)


class RollCast(Cast):
    """Status rows in columns, entering a sixteenth apart from the beat before the shot."""

    def __init__(
        self, shot: Shot, roll: Roll, clips: Sequence[Clip], timing: tuple[float, float, float], edit: Edit,
        terminal: Terminal,
    ) -> None:  # fmt: skip
        super().__init__(shot, *timing, edit)
        self.roll, self.clips, self.terminal = roll, clips, terminal
        w, h = edit.size
        tw, th = terminal.w, terminal.h
        self.used = max(clip.used for clip in clips)
        self.depth = -(-len(clips) // roll.columns)
        self.width = roll.columns * self.used * tw + (roll.columns - 1) * 4 * tw
        self.height = round((self.depth * 1.5 - 0.5) * th)
        scale = min(0.84 * h / self.height, 0.84 * w / self.width, 0.62)
        self.camera = Camera(scale, (self.width / 2, self.height / 2), (w / 2, h / 2))

    def side(self, t: float) -> Side:
        tw, th = self.terminal.w, self.terminal.h
        beat = self.edit.beat
        raster = np.empty((self.height, self.width, 3), np.float32)
        raster[:] = self.canvas
        for i, (take, clip) in enumerate(zip(self.roll.takes, self.clips, strict=True)):
            shown = float(np.clip((t - self.start + beat - i * beat / 4) / 0.09, 0, 1))
            if shown == 0:
                continue
            column, row = divmod(i, self.depth)
            x, y = column * (self.used + 4) * tw, round(row * 1.5 * th)
            paint = self.terminal.paint(clip[self.tick(take, t)][:, : self.used])
            raster[y : y + th, x : x + self.used * tw] = (
                self.canvas + (paint - self.canvas) * shown
            )
        plate = self.camera(raster, self.edit.size, self.canvas)
        return Side(plate, None, self.camera, self.camera.place((tw / 2, th / 2)))


class CardCast(Cast):
    def __init__(
        self, shot: Shot, card: Card, timing: tuple[float, float, float], edit: Edit
    ) -> None:
        super().__init__(shot, *timing, edit)
        self.card = card

    def side(self, t: float) -> Side:
        w, h = self.edit.size
        plate = np.broadcast_to(self.canvas, (h, w, 3)).copy()
        return Side(plate, None, Camera(1, (0, 0), (0, 0)), (w / 2, h / 2))

    def letters(self, t: float) -> Letters | None:
        w, h = self.edit.size
        first, *rest = self.card.lines
        lines = [
            Line(
                first,
                "Adwaita Sans",
                250,
                0.11 * h,
                (245, 245, 245),
                (w / 2, h * 0.5),
                centered=True,
            )
        ]
        lines += [
            Line(
                text,
                "Adwaita Sans",
                300,
                0.026 * h,
                (140, 140, 140),
                (w / 2, h * (0.585 + 0.045 * i)),
                centered=True,
            )
            for i, text in enumerate(rest)
        ]
        return lettering(tuple(lines), (w, h)), float(
            np.clip((t - self.opens) / (2 * self.edit.beat), 0, 1)
        )


class Reel:
    def __init__(self, edit: Edit) -> None:
        self.edit = edit
        beat = edit.beat
        self.bounds = [0.0, *accumulate(shot.beats * beat for shot in edit.shots)]
        self.duration = self.bounds[-1]
        cues = [
            cue(
                take,
                math.ceil(
                    (self.bounds[k + 1] - self.bounds[k]) / TICK
                    + (shot.cut.beats + take.preroll) * beat / TICK
                )
                + 2,
            )
            for k, shot in enumerate(edit.shots)
            for take in shot.takes
        ]
        rushes = export(
            {"fg": edit.ink, "bg": edit.canvas, "width": edit.cols, "clips": cues},
            edit.cols,
        )
        self.terminal = terminal = Terminal(
            rushes.symbols, rushes.octants, edit.cell * SS, edit.ink, edit.canvas
        )
        self.canvas = np.asarray(edit.canvas, np.float32) / 255
        clips = iter(rushes.clips)
        filmed = [[next(clips) for _ in shot.takes] for shot in edit.shots]
        # Every live shot shares one framing, so the mark holds still across cuts; the widest is
        # centered and fills at most nine tenths of the width.
        w, h = edit.size
        widest = max(
            clip.used
            for shot, mine in zip(edit.shots, filmed, strict=True)
            for clip in mine
            if isinstance(shot.picture, Single)
            and shot.picture.take.rows
            and shot.picture.framing is not Framing.CLOSE
        )
        live = Camera(
            min(1 / SS, 0.9 * w / (widest * terminal.w)),
            (widest * terminal.w / 2, 2 * terminal.h),
            (w / 2, h * 0.45),
        )
        self.casts: list[Cast] = []
        for k, (shot, mine) in enumerate(zip(edit.shots, filmed, strict=True)):
            timing = (
                self.bounds[k],
                self.bounds[k + 1],
                edit.shots[k - 1].cut.beats * beat if k else 0.0,
            )
            match shot.picture:
                case Single() as single:
                    self.casts.append(
                        SingleCast(shot, single, mine[0], timing, edit, terminal, live)
                    )
                case Roll() as roll:
                    self.casts.append(
                        RollCast(shot, roll, mine, timing, edit, terminal)
                    )
                case Card() as card:
                    self.casts.append(CardCast(shot, card, timing, edit))
        self.scramble = np.array(
            [
                i
                for i, symbol in enumerate(rushes.symbols)
                if symbol in rushes.octants or symbol.isalnum()
            ]
        )
        rng = np.random.default_rng(0)
        self.dither = [
            rng.triangular(-0.5, 0, 0.5, (h, w, 1)).astype(np.float32) / 255
            for _ in range(4)
        ]

    @property
    def frames(self) -> int:
        return round(self.duration * self.edit.fps)

    def at(self, t: float) -> int:
        return min(
            max(int(np.searchsorted(self.bounds, t, side="right")) - 1, 0),
            len(self.casts) - 1,
        )

    def picture(self, t: float, seed: int) -> np.ndarray:
        k = self.at(t)
        for j in (k, k + 1):
            if 0 < j < len(self.casts):
                half = self.edit.shots[j - 1].cut.beats * self.edit.beat / 2
                if abs(t - self.bounds[j]) < half:
                    stage = Stage(
                        self.terminal, self.edit.size, self.canvas, seed, self.scramble
                    )
                    a, b = self.casts[j - 1].side(t), self.casts[j].side(t)
                    return CUTS[self.edit.shots[j - 1].cut.kind](
                        a, b, (t - self.bounds[j] + half) / (2 * half), stage
                    )
        return self.casts[k].side(t).plate

    def frame(self, n: int) -> np.ndarray:
        t = n / self.edit.fps
        image = bloom(self.picture(t, n), self.canvas, self.edit.glow)
        k = self.at(t)
        for cast in self.casts[max(k - 1, 0) : k + 2]:
            if (
                cast.opens <= t < cast.closes
                and (letters := cast.letters(t)) is not None
            ):
                layer, alpha = letters
                if alpha > 0:
                    image = (
                        image * (1 - alpha * layer[..., 3:]) + alpha * layer[..., :3]
                    )
        beat = self.edit.beat
        level = min(
            1.0,
            t / (self.edit.fade_in * beat),
            (self.duration - t) / (self.edit.shots[-1].cut.beats * beat),
        )
        image = (
            self.canvas
            + (image - self.canvas) * max(level, 0.0)
            + self.dither[n % len(self.dither)]
        )
        return np.clip(image * 255 + 0.5, 0, 255).astype(np.uint8)


def bloom(image: np.ndarray, canvas: np.ndarray, strength: float) -> np.ndarray:
    """A soft halo around bright paint, as light spills on a dark panel."""
    h, w = image.shape[:2]
    light = cv2.resize(image - canvas, (w // 4, h // 4), interpolation=cv2.INTER_AREA)
    halo = (
        cv2.GaussianBlur(light, (0, 0), 2.5) * 0.55
        + cv2.GaussianBlur(light, (0, 0), 10) * 0.45
    )
    return image + strength * cv2.resize(halo, (w, h), interpolation=cv2.INTER_LINEAR)


# The reel a render's forked workers share.
REEL: Reel | None = None


def frame(n: int) -> bytes:
    assert REEL is not None
    return REEL.frame(n).tobytes()


def render(edit: Edit, out: Path, span: range | None) -> None:
    global REEL
    REEL = reel = Reel(edit)
    frames = span or range(reel.frames)
    w, h = edit.size
    with tempfile.TemporaryDirectory(prefix="shimmer-reel-") as scratch:
        track = Path(scratch) / "score.wav"
        score.write(edit, track, frames.start / edit.fps, len(frames) / edit.fps)
        encoder = subprocess.Popen(
            ["ffmpeg", "-loglevel", "error", "-y",
             "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{w}x{h}", "-r", str(edit.fps), "-i", "-",
             "-i", str(track),
             "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
             "-c:v", "libx264", "-preset", "slow", "-crf", "14", "-tune", "animation",
             "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
             "-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart", "-shortest", str(out)],
            stdin=subprocess.PIPE,
        )  # fmt: skip
        assert encoder.stdin is not None
        try:
            with multiprocessing.get_context("fork").Pool() as pool:
                for done, data in enumerate(pool.imap(frame, frames, chunksize=4)):
                    encoder.stdin.write(data)
                    if done % (edit.fps * 4) == 0:
                        print(f"\r{done / len(frames):6.1%}", end="", flush=True)
            encoder.stdin.close()
        except BrokenPipeError as closed:
            raise RuntimeError(
                f"ffmpeg quit writing {out}; its error is above"
            ) from closed
        if encoder.wait():
            raise RuntimeError(f"ffmpeg failed writing {out}")
    print(f"\r{out}")


def still(edit: Edit, seconds: Sequence[float], out: Path) -> None:
    reel = Reel(edit)
    out.mkdir(parents=True, exist_ok=True)
    for t in seconds:
        path = out / f"still-{t:07.3f}.png"
        Image.fromarray(reel.frame(round(t * edit.fps))).save(path)
        print(path)


def sheet(names: Sequence[str], seconds: float, every: float, out: Path) -> None:
    """One contact sheet per shimmer: its tool and status rows every `every` seconds of a turn
    that works, then waits."""
    live = {
        "live": {
            "command": "cargo test -p codex-tui --lib",
            "output": "Compiling codex-tui v0.0.0",
        }
    }
    survey = ticks(SURVEY, round(seconds / TICK), 63)
    rushes = export(
        {
            "fg": [214] * 3,
            "bg": [0] * 3,
            "width": 64,
            "clips": [
                {"shimmer": name, "rows": live, "ticks": survey} for name in names
            ],
        },
        64,
    )
    terminal = Terminal(rushes.symbols, rushes.octants, 12, (214, 214, 214), (0, 0, 0))
    label = typeface(FACES[0], 700, 18)
    out.mkdir(parents=True, exist_ok=True)
    for clip in rushes.clips:
        stills = [
            terminal.paint(clip[round(t / TICK)][[0, 3]])
            for t in np.arange(every, seconds, every)
        ]
        strip = np.concatenate([np.pad(s, ((0, 6), (0, 0), (0, 0))) for s in stills])
        image = Image.fromarray((np.clip(strip, 0, 1) * 255).astype(np.uint8))
        canvas = Image.new("RGB", (image.width, image.height + 30))
        canvas.paste(image, (0, 30))
        ImageDraw.Draw(canvas).text(
            (4, 4), f"{clip.shimmer} — {clip.blurb}", "#ffffff", label
        )
        path = out / f"{clip.shimmer.lower().replace(' ', '-')}.png"
        canvas.save(path)
        print(path)


def videos() -> Path:
    found = subprocess.run(
        ["xdg-user-dir", "VIDEOS"], capture_output=True, text=True, check=True
    )
    return Path(found.stdout.strip())


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--edit", type=Path, default=HERE / "reel.toml")
    commands = parser.add_subparsers(dest="command", required=True)
    film = commands.add_parser("render", help="film the edit")
    film.add_argument(
        "--out",
        type=Path,
        help="the film; codex-1337-shimmers.mp4 in the videos directory by default",
    )
    film.add_argument(
        "--span", type=float, nargs=2, metavar=("FROM", "TO"), help="seconds to film"
    )
    stills = commands.add_parser("still", help="paint instants of the edit")
    stills.add_argument("seconds", type=float, nargs="+")
    stills.add_argument(
        "--out", type=Path, default=Path(tempfile.gettempdir()) / "shimmer-reel/stills"
    )
    music = commands.add_parser("score", help="write the score alone")
    music.add_argument("--out", type=Path, default=Path("shimmer-reel.wav"))
    survey = commands.add_parser("sheet", help="contact sheets of stock shimmers")
    survey.add_argument("names", nargs="*")
    survey.add_argument("--seconds", type=float, default=17.0)
    survey.add_argument("--every", type=float, default=0.85)
    survey.add_argument(
        "--out", type=Path, default=Path(tempfile.gettempdir()) / "shimmer-reel/sheets"
    )
    args = parser.parse_args()
    match args.command:
        case "render":
            edit = Edit.read(args.edit)
            span = (
                range(round(args.span[0] * edit.fps), round(args.span[1] * edit.fps))
                if args.span
                else None
            )
            render(edit, args.out or videos() / "codex-1337-shimmers.mp4", span)
        case "still":
            still(Edit.read(args.edit), args.seconds, args.out)
        case "score":
            edit = Edit.read(args.edit)
            score.write(edit, args.out, 0, edit.beats * edit.beat)
        case "sheet":
            sheet(args.names or roster(), args.seconds, args.every, args.out)


if __name__ == "__main__":
    main()
