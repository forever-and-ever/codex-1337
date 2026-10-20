"""Cuts: how one shot gives way to the next. A cut's progress `p` runs over [0, 1] and the shots
meet at p = 1/2, on the beat. Picture cuts mix the two shots' plates; cell cuts mix their cells
before the terminal paints them."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

import cv2
import numpy as np

from cells import DEFAULT, Grid, Terminal
from edit import Kind

type Image = np.ndarray
type Point = tuple[float, float]


def ease(x: float) -> float:
    return float(0.5 - 0.5 * np.cos(np.pi * np.clip(x, 0, 1)))


def bump(x: float, width: float) -> float:
    return float(0.5 + 0.5 * np.cos(np.pi * x / width)) if abs(x) < width else 0.0


@dataclass(frozen=True, slots=True)
class Camera:
    """Maps raster pixel `(x, y)` to plate pixel `((x, y) - focus) * scale + anchor`."""

    scale: float
    focus: Point
    anchor: Point

    def __call__(
        self, raster: Image, size: tuple[int, int], canvas: np.ndarray
    ) -> Image:
        s = self.scale
        m = np.array(
            [
                [s, 0, self.anchor[0] - s * self.focus[0]],
                [0, s, self.anchor[1] - s * self.focus[1]],
            ],
            np.float32,
        )
        if s < 1:
            raster = cv2.GaussianBlur(raster, (0, 0), 0.42 / s)
        return cv2.warpAffine(raster - canvas, m, size, flags=cv2.INTER_LINEAR) + canvas

    def place(self, point: Point) -> Point:
        return (
            (point[0] - self.focus[0]) * self.scale + self.anchor[0],
            (point[1] - self.focus[1]) * self.scale + self.anchor[1],
        )

    def mix(self, other: Camera, p: float, size: tuple[int, int]) -> Camera:
        """The camera `p` of the way to `other`: the raster point at the plate's center moves
        straight and the scale changes at a constant ratio, as a dolly sees it."""
        center = (size[0] / 2, size[1] / 2)

        def view(camera: Camera) -> np.ndarray:
            return (
                np.array(camera.focus)
                + (np.array(center) - camera.anchor) / camera.scale
            )

        point = view(self) + (view(other) - view(self)) * p
        return Camera(
            self.scale * (other.scale / self.scale) ** p, (point[0], point[1]), center
        )


@dataclass(frozen=True, slots=True)
class Side:
    """One shot as a cut sees it at an instant: its plate, its camera, where its status row's
    mark lands, and for single takes its cells."""

    plate: Image
    grid: Grid | None
    camera: Camera
    mark: Point


@dataclass(frozen=True, slots=True)
class Stage:
    """What every cut may draw with: the terminal, the plate size, the canvas, a seed that
    changes every frame, and the glyphs a scramble may show."""

    terminal: Terminal
    size: tuple[int, int]
    canvas: np.ndarray
    seed: int
    scramble: np.ndarray


type Cutter = Callable[[Side, Side, float, Stage], Image]


def fade(a: Side, b: Side, p: float, stage: Stage) -> Image:
    return a.plate + (b.plate - a.plate) * ease(p)


def crt(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """A tube switching off into a line and a dot, and the next one switching on from it."""
    side = a if p < 0.5 else b
    q = abs(2 * p - 1)
    sy, sx = ease((q - 0.4) / 0.6), ease(q / 0.4)
    w, h = stage.size
    x0, y0 = side.mark[0] + 0.36 * w, side.mark[1]
    image = side.plate - stage.canvas
    plate = np.zeros_like(image)
    if sy > 0.004:
        m = np.array([[1, 0, 0], [0, sy, y0 * (1 - sy)]], np.float32)
        plate = cv2.warpAffine(image, m, (w, h), flags=cv2.INTER_AREA) * (
            1 + 2.5 * (1 - sy)
        )
    # The beam takes the hue of the brightest paint.
    small = cv2.resize(image, (w // 16, h // 16), interpolation=cv2.INTER_AREA).reshape(
        -1, 3
    )
    tone = small[small.sum(1).argmax()]
    tone = (tone / max(float(tone.max()), 1e-3) * 0.4 + 0.6).astype(np.float32)
    ys, xs = np.mgrid[0:h, 0:w].astype(np.float32)
    beam = np.exp(-(((ys - y0) / (1.5 + 5 * sy)) ** 2)) * (
        np.abs(xs - x0) < sx * 0.5 * w + 2
    )
    dot = (
        np.exp(-((xs - x0) ** 2 + (ys - y0) ** 2) / 18.0)
        * (1 - sx)
        * min(1.0, q / 0.08)
    )
    light = beam * (1 - sy) * (0.25 + 0.75 * min(1.0, q / 0.4)) + dot
    return plate + light[..., None] * tone + stage.canvas


def glitch(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """Torn scanlines, shifted channels and stray bands of the other shot."""
    rng = np.random.default_rng(stage.seed)
    i = bump(p - 0.5, 0.5)
    base, other = (a.plate, b.plate) if p < 0.5 else (b.plate, a.plate)
    out = base.copy()
    y = 0
    while y < out.shape[0]:
        band = int(rng.integers(3, 48))
        if rng.random() < 0.6 * i:
            source = other if rng.random() < 0.35 * i else base
            out[y : y + band] = np.roll(
                source[y : y + band], int(rng.normal(0, 140 * i)), axis=1
            )
        y += band
    shift = round(24 * i)
    out[..., 0] = np.roll(out[..., 0], shift, axis=1)
    out[..., 2] = np.roll(out[..., 2], -shift, axis=1)
    return out


def zoom(
    image: Image, focus: Point, scale: float, streak: float, canvas: np.ndarray
) -> Image:
    """`image` scaled about `focus`, smeared radially over `streak` of the scale."""
    h, w = image.shape[:2]
    taps = 9 if streak > 0.01 else 1
    acc = np.zeros_like(image)
    for k in range(taps):
        s = scale * (1 + streak * k / taps)
        m = np.array(
            [[s, 0, focus[0] * (1 - s)], [0, s, focus[1] * (1 - s)]], np.float32
        )
        acc += cv2.warpAffine(image - canvas, m, (w, h), flags=cv2.INTER_LINEAR)
    return acc / taps + canvas


def punch(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """The camera rushes through one shot into the next, with a flash where they meet."""
    side = a if p < 0.5 else b
    focus = (side.mark[0] + 0.3 * stage.size[0], side.mark[1])
    if p < 0.5:
        x = p / 0.5
        image = zoom(a.plate, focus, 1 + 5 * x**3, 0.35 * x**2, stage.canvas)
    else:
        x = (1 - p) / 0.5
        image = zoom(b.plate, focus, 1 - 0.7 * x**2, 0.3 * x**2, stage.canvas)
    return image + 0.5 * np.exp(-(((p - 0.5) / 0.045) ** 2))


def melt(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """The screen melt: columns of the outgoing shot slide down at ragged speeds."""
    w, h = stage.size
    width = 6
    walk = np.cumsum(np.random.default_rng(6).uniform(-1, 1, w // width + 1))
    delay = 0.3 * (walk - walk.min()) / max(float(np.ptp(walk)), 1e-6)
    fall = (np.clip((p - delay) / 0.62, 0, 1) ** 2 * h * 1.15).astype(np.int32)
    rows = np.arange(h)[:, None] - np.repeat(fall, width)[None, :w]
    melted = np.take_along_axis(a.plate, np.clip(rows, 0, h - 1)[..., None], axis=0)
    return np.where((rows >= 0)[..., None], melted, b.plate)


def scan(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """A bar of light sweeps across, leaving the next shot behind it."""
    w, _ = stage.size
    x0 = ease(p) * (w + 200) - 100
    xs = np.arange(w, dtype=np.float32)
    behind = np.clip((x0 - xs) / 24 + 0.5, 0, 1)[None, :, None]
    bar = np.exp(-(((xs - x0) / 5) ** 2)) + 0.3 * np.exp(-(((xs - x0) / 40) ** 2))
    light = (
        bar[None, :, None] * np.array([0.75, 0.9, 1.0], np.float32) * bump(p - 0.5, 0.5)
    )
    return a.plate + (b.plate - a.plate) * behind + light


def iris(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """The next shot opens in a ring from its status row's mark."""
    w, h = stage.size
    cx, cy = b.mark
    r = ease(p) * np.hypot(max(cx, w - cx), max(cy, h - cy)) * 1.05
    ys, xs = np.ogrid[0:h, 0:w]
    d = np.sqrt((xs - cx) ** 2 + (ys - cy) ** 2, dtype=np.float32)
    inside = np.clip((r - d) / 3 + 0.5, 0, 1)[..., None]
    ring = (np.exp(-(((d - r) / 2.5) ** 2)) * np.sin(np.pi * p))[..., None]
    return (
        a.plate
        + (b.plate - a.plate) * inside
        + ring * np.array([1.0, 0.85, 0.6], np.float32)
    )


def shatter(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """The outgoing cells burst from the mark and tumble away as the next shot rises behind."""
    assert a.grid is not None
    w, h = stage.size
    rows, cols = a.grid.shape
    cw, ch = stage.terminal.w * a.camera.scale, stage.terminal.h * a.camera.scale
    out = stage.canvas + (b.plate - stage.canvas) * ease((p - 0.3) / 0.5)
    t = ease(p / 0.8) * 1.6
    fading = max(0.0, 1 - t / 1.2)
    rng = np.random.default_rng(1729)
    origin = np.array(a.mark)
    for r in range(rows):
        for c in range(cols):
            speed, angle, spin = rng.random(3)
            if fading == 0 or not a.grid.inked[r, c]:
                continue
            x, y = a.camera.place((c * stage.terminal.w, r * stage.terminal.h))
            ix, iy = round(x), round(y)
            tile = (
                a.plate[max(iy, 0) : iy + int(ch) + 1, max(ix, 0) : ix + int(cw) + 1]
                - stage.canvas
            )
            if tile.size == 0:
                continue
            centre = np.array([x + cw / 2, y + ch / 2])
            away = (centre - origin) / (np.linalg.norm(centre - origin) + 1e-3)
            turn = (angle - 0.5) * 2.2
            rotate = np.array(
                [[np.cos(turn), -np.sin(turn)], [np.sin(turn), np.cos(turn)]]
            )
            v = rotate @ away * (350 + 1000 * speed) + np.array([0, -380])
            centre = centre + v * t + np.array([0, 520 * t * t])
            m = cv2.getRotationMatrix2D(
                (tile.shape[1] / 2, tile.shape[0] / 2),
                (spin - 0.5) * 1440 * t,
                1 + 0.6 * t,
            )
            size = int(max(tile.shape[:2]) * (1.8 + 0.6 * t)) + 2
            m[:, 2] += (size / 2 - tile.shape[1] / 2, size / 2 - tile.shape[0] / 2)
            spun = cv2.warpAffine(tile, m, (size, size), flags=cv2.INTER_LINEAR)
            x0, y0 = int(centre[0] - size / 2), int(centre[1] - size / 2)
            x1, y1, x2, y2 = (
                max(x0, 0),
                max(y0, 0),
                min(x0 + size, w),
                min(y0 + size, h),
            )
            if x2 > x1 and y2 > y1:
                out[y1:y2, x1:x2] += spun[y1 - y0 : y2 - y0, x1 - x0 : x2 - x0] * fading
    return out


def tiles(raster: Image, rows: int, cols: int, terminal: Terminal) -> Image:
    return raster.reshape(rows, terminal.h, cols, terminal.w, 3).transpose(
        0, 2, 1, 3, 4
    )


def untile(cells: Image) -> Image:
    rows, cols, h, w, _ = cells.shape
    return cells.transpose(0, 2, 1, 3, 4).reshape(rows * h, cols * w, 3)


def stagger(rows: int, cols: int, p: float, span: float, seed: int) -> np.ndarray:
    """Each cell's progress through its own turn, sweeping left to right with a little noise."""
    noise = np.random.default_rng(seed).random((rows, cols))
    delay = (1 - span) * (0.8 * np.arange(cols)[None, :] / cols + 0.2 * noise)
    return np.clip((p - delay) / span, 0, 1)


def flap(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """A split-flap board: every cell flips over to the next shot's."""
    assert a.grid is not None and b.grid is not None
    term = stage.terminal
    rows, cols = a.grid.shape
    q = stagger(rows, cols, p, 0.3, 17)
    s = np.abs(np.cos(np.pi * q))
    face = np.where(
        (q < 0.5)[..., None, None, None],
        tiles(term.paint(a.grid), rows, cols, term),
        tiles(term.paint(b.grid), rows, cols, term),
    )
    yy = np.arange(term.h, dtype=np.float32)[None, None, :] - term.h / 2
    src = np.clip(
        np.round(term.h / 2 + yy / np.maximum(s, 1e-3)[..., None]), 0, term.h - 1
    )
    flipped = np.take_along_axis(face, src.astype(np.intp)[..., None, None], axis=2)
    seen = (np.abs(yy) <= s[..., None] * term.h / 2)[..., None, None]
    shade = (0.45 + 0.55 * s)[..., None, None, None]
    canvas = stage.canvas
    cells = np.where(seen, canvas + (flipped - canvas) * shade, canvas)
    return a.camera.mix(b.camera, p, stage.size)(
        untile(cells).astype(np.float32), stage.size, canvas
    )


def decrypt(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """Cells scramble through random glyphs and settle into the next shot's."""
    assert a.grid is not None and b.grid is not None
    rows, cols = a.grid.shape
    q = stagger(rows, cols, p, 0.28, 29)
    rng = np.random.default_rng(stage.seed)
    scrambling = (q > 0) & (q < 1) & (a.grid.inked | b.grid.inked)
    noise = Grid(
        stage.scramble[rng.integers(0, len(stage.scramble), (rows, cols))].astype(
            np.uint16
        ),
        np.where(b.grid.fg != DEFAULT, b.grid.fg, 0xE8FFF4).astype(np.int32),
        np.full((rows, cols), DEFAULT, np.int32),
        np.full((rows, cols), DEFAULT, np.int32),
        np.zeros((rows, cols), np.uint16),
    )
    grid = a.grid.where(q >= 1, b.grid).where(scrambling, noise)
    return a.camera.mix(b.camera, p, stage.size)(
        stage.terminal.paint(grid), stage.size, stage.canvas
    )


def shred(a: Side, b: Side, p: float, stage: Stage) -> Image:
    """Each terminal row slides away, alternating sides, the next shot's row riding in behind."""
    assert a.grid is not None
    w, h = stage.size
    rows = a.grid.shape[0]
    out = np.broadcast_to(stage.canvas, a.plate.shape).copy()
    edges = [
        round(a.camera.place((0, r * stage.terminal.h))[1]) for r in range(rows + 1)
    ]
    edges[0], edges[-1] = 0, h
    for r in range(rows):
        top, bottom = edges[r], edges[r + 1]
        direction = 1 if r % 2 else -1
        u = np.clip((p - 0.07 * r) / (1 - 0.07 * (rows - 1)), 0, 1)
        x, speed = ease(u), float(np.pi * np.sin(np.pi * u))
        for plate, offset in ((a.plate, direction * x), (b.plate, direction * (x - 1))):
            shift = round(offset * w * 1.05)
            band = np.roll(plate[top:bottom] - stage.canvas, shift, axis=1)
            if shift > 0:
                band[:, :shift] = 0
            elif shift < 0:
                band[:, shift:] = 0
            blur = int(1 + 60 * speed)
            out[top:bottom] += cv2.blur(band, (blur, 1)) if blur > 1 else band
    return out


CUTS: dict[Kind, Cutter] = {
    Kind.FADE: fade,
    Kind.CRT: crt,
    Kind.GLITCH: glitch,
    Kind.PUNCH: punch,
    Kind.MELT: melt,
    Kind.SCAN: scan,
    Kind.IRIS: iris,
    Kind.SHATTER: shatter,
    Kind.FLAP: flap,
    Kind.DECRYPT: decrypt,
    Kind.SHRED: shred,
}
