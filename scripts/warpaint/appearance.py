"""The appearance frame: how the panel shows a color, how far apart two colors look, and how
legible text is over a ground.

Frame: the Hellwig-Fairchild 2022 revision of CAM16 with the Hellwig-Stolitzka-Fairchild
Helmholtz-Kohlrausch term, giving apparent lightness J_HK, colorfulness M and hue h under fixed
viewing conditions: a D65 panel at 100 cd/m2 in a dim room, a dark interface as the adapting
field, and an IPS panel's glow at black added to every color it shows. Distances are CAM16-UCS
dE; legibility is APCA-W3 0.98G Lc; dichromacy is Machado 2009 at full severity.
"""

from __future__ import annotations

import math
import warnings
from collections.abc import Callable
from dataclasses import dataclass
from enum import StrEnum, auto

import numpy as np

with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    import colour
    from colour.appearance.hellwig2022 import (
        VIEWING_CONDITIONS_HELLWIG2022,
        hue_angle_dependency_Hellwig2022,
    )

type Rgb = tuple[int, int, int]


WHITE = (
    colour.xy_to_XYZ(colour.CCS_ILLUMINANTS["CIE 1931 2 Degree Standard Observer"]["D65"])
    * 100
)
ADAPTING = 8.0
"""L_A in cd/m2: a dark interface filling most of a 100 cd/m2 panel."""
BACKGROUND = 8.0
"""Y_b in percent: the mean field of a dark interface, ground and text together."""
SURROUND = VIEWING_CONDITIONS_HELLWIG2022["Dim"]
FLARE = 0.0015
"""Fraction of white an IPS panel emits at black, with a dim room's reflection. Added before
the model sees a color, so dark grounds compress as they do on the panel and no role can be
solved below the glow."""

_SRGB_TO_XYZ = np.array(
    [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.1191920, 0.9503041],
    ]
)
_XYZ_TO_SRGB = np.linalg.inv(_SRGB_TO_XYZ)


def _decode(rgb: np.ndarray) -> np.ndarray:
    v = np.asarray(rgb, float) / 255
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)


def _encode(linear: np.ndarray) -> np.ndarray:
    v = np.clip(linear, 0, 1)
    return np.round(255 * np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1 / 2.4) - 0.055)).astype(int)


def _shown(rgb: np.ndarray) -> np.ndarray:
    return (1 - FLARE) * (_decode(rgb) @ _SRGB_TO_XYZ.T) * 100 + FLARE * WHITE


_PROBE = colour.XYZ_to_Hellwig2022(np.array([30.0, 20.0, 10.0]), WHITE, ADAPTING, BACKGROUND, SURROUND)
_C_PER_M = float(_PROBE.C / _PROBE.M)
"""C = 35 M / A_w is fixed by the frame. colour 0.4.7's inverse derives J from J_HK before it
derives C from M, so it treats J_HK as J whenever only M is given; render removes the HK term
itself."""


def appearance(rgb: np.ndarray) -> np.ndarray:
    """(..., 3) sRGB to (..., 3) [J_HK, M, h]."""
    s = colour.XYZ_to_Hellwig2022(_shown(rgb), WHITE, ADAPTING, BACKGROUND, SURROUND)
    return np.stack([s.J_HK, s.M, s.h], -1)


def ucs(rgb: np.ndarray) -> np.ndarray:
    """(..., 3) sRGB to CAM16-UCS J'a'b'."""
    s = colour.XYZ_to_CAM16(_shown(rgb), WHITE, ADAPTING, BACKGROUND, SURROUND)
    return colour.JMh_CAM16_to_CAM16UCS(np.stack([s.J, s.M, s.h], -1))


def render(lightness: object, colorfulness: object, hue: object) -> tuple[np.ndarray, np.ndarray]:
    """[J_HK, M, h] to sRGB, and whether the panel can show it."""
    j_hk, m, h = np.broadcast_arrays(*(np.asarray(v, float) for v in (lightness, colorfulness, hue)))
    j = j_hk - hue_angle_dependency_Hellwig2022(h) * (_C_PER_M * m) ** 0.587
    xyz = colour.Hellwig2022_to_XYZ(
        colour.CAM_Specification_Hellwig2022(J=j, M=m, h=h), WHITE, ADAPTING, BACKGROUND, SURROUND
    )
    linear = (xyz - FLARE * WHITE) / (1 - FLARE) / 100 @ _XYZ_TO_SRGB.T
    return _encode(linear), np.all((linear >= -1e-4) & (linear <= 1 + 1e-4), -1) & (j > 0)


def yielding(lightness: object, colorfulness: object, hue: object) -> tuple[np.ndarray, np.ndarray]:
    """sRGB and the M granted: colorfulness yields to the gamut; lightness and hue are intent."""
    j, asked, h = np.broadcast_arrays(*(np.asarray(v, float) for v in (lightness, colorfulness, hue)))
    whole = render(j, asked, h)[1]
    low, high = np.zeros_like(asked), asked.copy()
    for _ in range(20):
        mid = (low + high) / 2
        ok = render(j, mid, h)[1]
        low, high = np.where(ok, mid, low), np.where(ok, high, mid)
    granted = np.where(whole, asked, low)
    return render(j, granted, h)[0], granted


def apca(text: np.ndarray, ground: np.ndarray) -> np.ndarray:
    """Absolute Lc of text over ground."""

    def luminance(rgb: np.ndarray) -> np.ndarray:
        v = np.asarray(rgb, float) / 255
        y = v[..., 0] ** 2.4 * 0.2126729 + v[..., 1] ** 2.4 * 0.7151522 + v[..., 2] ** 2.4 * 0.0721750
        return np.where(y > 0.022, y, y + np.clip(0.022 - y, 0, None) ** 1.414)

    t, g = luminance(text), luminance(ground)
    normal = (g**0.56 - t**0.57) * 1.14
    reverse = (g**0.65 - t**0.62) * 1.14
    lc = np.where(
        g > t,
        np.where(normal < 0.1, 0.0, normal - 0.027),
        np.where(reverse > -0.1, 0.0, reverse + 0.027),
    )
    return np.where(np.abs(g - t) < 0.0005, 0.0, np.abs(lc) * 100)


class Vision(StrEnum):
    TRICHROMAT = auto()
    PROTAN = auto()
    DEUTAN = auto()


_DICHROMACY = {
    Vision.PROTAN: colour.blindness.matrix_cvd_Machado2009("Protanomaly", 1.0),
    Vision.DEUTAN: colour.blindness.matrix_cvd_Machado2009("Deuteranomaly", 1.0),
}


def distance(left: Rgb, right: Rgb, vision: Vision = Vision.TRICHROMAT) -> float:
    pair = np.array([left, right])
    if vision is not Vision.TRICHROMAT:
        pair = _encode(_decode(pair) @ _DICHROMACY[vision].T)
    a, b = ucs(pair)
    return float(np.linalg.norm(a - b))


def least_lightness(reach: Callable[[np.ndarray], np.ndarray], low: float, high: float) -> float:
    """Least lightness in [low, high] at which reach, rising with lightness, turns non-negative."""
    for _ in range(3):
        grid = np.linspace(low, high, 41)
        ok = reach(grid) >= 0
        first = int(np.argmax(ok)) if ok.any() else len(grid) - 1
        low, high = float(grid[max(first - 1, 0)]), float(grid[first])
    return high


# -- Swatches -------------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Swatch:
    rgb: Rgb
    lightness: float
    colorfulness: float
    hue: float
    asked: float

    @classmethod
    def of(cls, rgb: np.ndarray, asked: float) -> Swatch:
        j, m, h = appearance(rgb)
        return cls((int(rgb[0]), int(rgb[1]), int(rgb[2])), float(j), float(m), float(h), float(asked))

    @property
    def hex(self) -> str:
        return "#%02x%02x%02x" % self.rgb

    @property
    def array(self) -> np.ndarray:
        return np.array(self.rgb)


def paint(lightness: float, colorfulness: float, hue: float) -> Swatch:
    return Swatch.of(yielding(lightness, colorfulness, hue)[0], colorfulness)


def elevated(ground: Swatch, rise: float) -> tuple[float, float]:
    """J_HK and M of the ground's material moved rise CAM16-UCS J' units at constant saturation."""
    jp = 1.7 * ground.lightness / (1 + 0.007 * ground.lightness) + rise
    j = jp / (1.7 - 0.007 * jp)
    return j, ground.colorfulness * j / ground.lightness


def tinted(m: float, hue: float, toward: float, strength: float) -> tuple[float, float]:
    """M and h of a surface's own appearance vector plus strength colorfulness toward a hue."""
    a = m * math.cos(math.radians(hue)) + strength * math.cos(math.radians(toward))
    b = m * math.sin(math.radians(hue)) + strength * math.sin(math.radians(toward))
    return math.hypot(a, b), math.degrees(math.atan2(b, a)) % 360


_CUSP_GRID = np.arange(20.0, 99.0, 0.5)


def cusp(hue: float) -> float:
    """J_HK at which the panel shows the most colorfulness for this hue: its natural lightness."""
    _, granted = yielding(_CUSP_GRID, 200.0, np.full_like(_CUSP_GRID, hue))
    return float(_CUSP_GRID[int(np.argmax(granted))])
