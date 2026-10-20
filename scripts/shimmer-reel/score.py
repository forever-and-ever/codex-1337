"""The score: a synthwave cue on the reel's beat grid. Each run of shots at one energy is a
section that opens on the tonic; energy arranges the bars under it, the bar before the energy
peaks swells into it, and every cut carries its own sound, so music and picture move together.
Everything is synthesized here; nothing is sampled."""

from __future__ import annotations

from itertools import accumulate
from pathlib import Path

import numpy as np
import pyloudnorm
from scipy.io import wavfile
from scipy.ndimage import minimum_filter1d, uniform_filter1d
from scipy.signal import butter, fftconvolve, resample_poly, sosfilt

from edit import Edit, Kind

SR = 48_000
LUFS = -16.0
# Peaks stay 1.2 dB under full scale, checked at four times the rate.
CEILING = 10 ** (-1.2 / 20)
# The changes, two bars each: a bass note under an upper voicing, in A minor.
CHANGES = (
    (45, (57, 60, 64, 67, 71)),  # Am9
    (41, (57, 60, 64, 67)),  # Fmaj9
    (48, (55, 59, 62, 64)),  # Cmaj9
    (40, (55, 59, 62, 64)),  # Em7
)
# The arpeggio's walk over a voicing's tones and their octave.
WALK = (0, 2, 4, 1, 3, 5, 2, 6)
# Per energy: pad gain, arpeggio steps a beat, and arpeggio gain.
PADS = (1.7, 0.9, 0.9, 0.75)
STEPS = (1, 2, 4, 4)
PLUCKS = (0.22, 0.5, 0.6, 0.6)

type Stereo = np.ndarray


def hz(note: float) -> float:
    return 440 * 2 ** ((note - 69) / 12)


def filt(x: np.ndarray, kind: str, cutoff: float | tuple[float, float]) -> np.ndarray:
    return sosfilt(butter(2, cutoff, kind, fs=SR, output="sos"), x, axis=0)


def saw(freq: np.ndarray, phase: float) -> np.ndarray:
    """A band-limited sawtooth following the instantaneous frequency `freq`."""
    dt = freq / SR
    t = (phase + np.cumsum(dt)) % 1.0
    y = 2 * t - 1
    low = t < dt
    x = t[low] / dt[low]
    y[low] -= x + x - x * x - 1
    high = t > 1 - dt
    x = (t[high] - 1) / dt[high]
    y[high] -= x * x + x + x + 1
    return y


def envelope(n: int, attack: float, release: float, hold: int) -> np.ndarray:
    """Rises over `attack` seconds, holds until sample `hold`, then dies over `release`."""
    rise = np.clip(np.arange(n) / (attack * SR), 0, 1)
    return rise * np.exp(-np.maximum(np.arange(n) - hold, 0) / (release * SR))


def pan(x: np.ndarray, position: float) -> Stereo:
    """`x` placed at `position` in [-1, 1] with equal power."""
    angle = (position + 1) * np.pi / 4
    return np.stack([x * np.cos(angle), x * np.sin(angle)], -1)


def layered(*parts: Stereo) -> Stereo:
    out = np.zeros((max(len(part) for part in parts), 2))
    for part in parts:
        out[: len(part)] += part
    return out


class Track:
    """A stereo bus of fixed length that sounds are laid into."""

    def __init__(self, n: int) -> None:
        self.x = np.zeros((n, 2))

    def lay(self, at: float, sound: np.ndarray) -> None:
        start = round(at * SR)
        sound = sound if sound.ndim == 2 else np.stack([sound, sound], -1)
        if start < 0:
            sound, start = sound[-start:], 0
        end = min(start + len(sound), len(self.x))
        if end > start:
            self.x[start:end] += sound[: end - start]


def pad(chord: tuple[int, ...], seconds: float, rng: np.random.Generator) -> Stereo:
    """Detuned saws per tone, spread across the field."""
    n = int((seconds + 1.6) * SR)
    out = np.zeros((n, 2))
    for tone in chord:
        for k, cents in enumerate((-15, -6, 0, 7, 16)):
            wave = saw(np.full(n, hz(tone) * 2 ** (cents / 1200)), rng.random())
            out += pan(wave, (k - 2) / 2.2)
    return out * envelope(n, 0.7, 0.55, int(seconds * SR))[:, None] / (5 * len(chord))


def pluck(note: float, rng: np.random.Generator) -> np.ndarray:
    n = int(0.45 * SR)
    f = np.full(n, hz(note))
    wave = 0.6 * saw(f, rng.random()) + 0.4 * saw(f * 1.004, rng.random())
    return wave * envelope(n, 0.003, 0.13, 0)


def bass(note: float, seconds: float, rng: np.random.Generator) -> np.ndarray:
    n = int((seconds + 0.08) * SR)
    f = np.full(n, hz(note))
    sub = np.sin(2 * np.pi * np.cumsum(f) / SR)
    body = filt(saw(f, rng.random()), "lowpass", 380)
    return (0.75 * sub + 0.45 * body) * envelope(n, 0.004, 0.06, int(seconds * SR))


def kick() -> np.ndarray:
    n = int(0.42 * SR)
    t = np.arange(n) / SR
    body = np.sin(2 * np.pi * np.cumsum(44 + 110 * np.exp(-t / 0.04)) / SR) * np.exp(
        -t / 0.26
    )
    click = filt(
        np.random.default_rng(1).standard_normal(n), "highpass", 2500
    ) * np.exp(-t / 0.003)
    return np.tanh(1.6 * (body + 0.25 * click))


def clap(rng: np.random.Generator) -> np.ndarray:
    n = int(0.35 * SR)
    t = np.arange(n) / SR
    burst = sum(
        np.exp(-np.maximum(t - d, 0) / 0.006) * (t >= d) for d in (0, 0.011, 0.023)
    )
    tail = np.exp(-np.maximum(t - 0.023, 0) / 0.11) * (t >= 0.023)
    return filt(rng.standard_normal(n), "bandpass", (900, 3200)) * (burst + 0.6 * tail)


def hat(rng: np.random.Generator, decay: float) -> np.ndarray:
    n = int((decay * 6 + 0.01) * SR)
    return filt(rng.standard_normal(n), "highpass", 7500) * np.exp(
        -np.arange(n) / (decay * SR)
    )


def crash(rng: np.random.Generator) -> Stereo:
    n = int(2.6 * SR)
    wash = filt(rng.standard_normal((n, 2)), "highpass", 3800)
    return wash * np.exp(-np.arange(n) / (0.9 * SR))[:, None] * 0.6


def sweep(n: int, centers: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    """Noise whose band follows `centers` (one per sample), by crossfading fixed bands."""
    bands = np.geomspace(150, 9000, 10)
    noise = rng.standard_normal(n)
    stack = np.stack([filt(noise, "bandpass", (c / 1.35, c * 1.35)) for c in bands])
    pos = np.interp(np.log(centers), np.log(bands), np.arange(len(bands)))
    low = np.floor(pos).astype(int)
    high = np.minimum(low + 1, len(bands) - 1)
    frac = pos - low
    idx = np.arange(n)
    return stack[low, idx] * (1 - frac) + stack[high, idx] * frac


def whoosh(seconds: float, rng: np.random.Generator, rise: bool = True) -> Stereo:
    n = int(seconds * SR)
    u = np.linspace(0, 1, n)
    centers = 300 * 20 ** (np.sin(np.pi * u) if rise else 1 - u)
    x = sweep(n, centers, rng) * np.sin(np.pi * u) ** 2
    return np.stack([x * (1 - 0.6 * u), x * (0.4 + 0.6 * u)], -1)


def boom(rng: np.random.Generator) -> np.ndarray:
    n = int(1.4 * SR)
    t = np.arange(n) / SR
    body = np.sin(2 * np.pi * np.cumsum(38 + 70 * np.exp(-t / 0.07)) / SR) * np.exp(
        -t / 0.5
    )
    grit = filt(rng.standard_normal(n), "lowpass", 900) * np.exp(-t / 0.06)
    return np.tanh(1.3 * (body + 0.35 * grit))


def pings(
    seconds: float,
    count: int,
    band: tuple[float, float],
    decay: float,
    rng: np.random.Generator,
) -> Stereo:
    """Short sine pings scattered over `seconds`, densest in the middle."""
    n = int(decay * 8 * SR)
    t = np.arange(n) / SR
    track = Track(int(seconds * SR) + n)
    for _ in range(count):
        at = float(np.clip(rng.normal(0.5, 0.2), 0, 1)) * seconds
        tone = np.sin(2 * np.pi * rng.uniform(*band) * t) * np.exp(-t / decay)
        track.lay(at, pan(tone * np.minimum(t / 0.001, 1), rng.uniform(-0.8, 0.8)))
    return track.x


def clicks(seconds: float, count: int, rng: np.random.Generator) -> Stereo:
    """The rattle of a split-flap board turning over."""
    n = int(0.012 * SR)
    decay = np.exp(-np.arange(n) / (0.0016 * SR))
    track = Track(int(seconds * SR) + n)
    for _ in range(count):
        tick = filt(rng.standard_normal(n), "bandpass", (1800, 5200)) * decay
        track.lay(rng.beta(2.2, 2.2) * seconds, pan(tick, rng.uniform(-0.9, 0.9)))
    return track.x


def tube(seconds: float, rng: np.random.Generator) -> Stereo:
    """A tube switching off with a falling whine, then on with a thump and hum."""
    n = int(seconds * SR)
    t = np.arange(n) / SR
    after = np.maximum(t - seconds / 2, 0)
    off, on = t < seconds / 2, t >= seconds / 2
    whine = (
        np.sin(2 * np.pi * np.cumsum(60 + 1400 * np.exp(-t / 0.06)) / SR)
        * np.exp(-t / 0.12)
        * off
    )
    thump = np.sin(2 * np.pi * 70 * after) * np.exp(-after / 0.08) * on
    hum = np.sin(2 * np.pi * 120 * t) * 0.2 * np.exp(-after / 0.25) * on
    crackle = filt(
        rng.standard_normal(n) * (rng.random(n) < 0.004), "bandpass", (800, 6000)
    )
    return pan(0.5 * whine + 0.6 * thump + hum + 2.5 * crackle, 0)


def stutter(seconds: float, rng: np.random.Generator) -> Stereo:
    """Digital debris: crushed square bleeps, densest where the cut tears."""
    track = Track(int((seconds + 0.06) * SR))
    for _ in range(int(seconds * 34)):
        n = int(rng.uniform(0.012, 0.045) * SR)
        t = np.arange(n) / SR
        square = (
            np.round(np.sign(np.sin(2 * np.pi * rng.uniform(90, 1400) * t)) * 4) / 4
        )
        square *= np.minimum(1, (n - np.arange(n)) / (0.003 * SR))
        at = float(np.clip(rng.normal(0.5, 0.16), 0, 1)) * seconds
        track.lay(at, pan(square * 0.35, rng.uniform(-0.7, 0.7)))
    return filt(track.x, "lowpass", 6000)


def chime(rng: np.random.Generator) -> Stereo:
    n = int(2.4 * SR)
    t = np.arange(n) / SR
    tone = sum(
        a * np.sin(2 * np.pi * hz(note) * r * t) * np.exp(-t / d)
        for note in (81, 88, 95)
        for a, r, d in ((1, 1, 1.1), (0.4, 2.76, 0.4), (0.2, 5.4, 0.2))
    )
    return pan(tone * np.minimum(t / 0.004, 1), rng.uniform(-0.3, 0.3)) / 3


def sound(
    kind: Kind, seconds: float, rng: np.random.Generator
) -> tuple[float, Stereo] | None:
    """A cut's sound and its start relative to the cut's middle."""
    lead = -seconds / 2
    match kind:
        case Kind.FADE:
            return None
        case Kind.CRT:
            return lead, tube(seconds, rng) * 0.5
        case Kind.GLITCH:
            return lead, stutter(seconds, rng) * 0.6
        case Kind.PUNCH:
            punch = Track(int((seconds / 2 + 1.4) * SR))
            punch.lay(0, whoosh(seconds / 2, rng) * 0.7)
            punch.lay(seconds / 2, pan(boom(rng), 0) * 0.9)
            return lead, punch.x
        case Kind.MELT:
            return lead, whoosh(seconds, rng, rise=False) * 0.5
        case Kind.SCAN:
            return lead, layered(
                whoosh(seconds, rng) * 0.35,
                pings(seconds, 6, (1800, 2600), 0.12, rng) * 0.25,
            )
        case Kind.IRIS:
            return 0.0, chime(rng) * 0.45
        case Kind.SHATTER:
            return -0.05, layered(
                pings(seconds, 46, (2400, 8000), 0.07, rng) * 0.4,
                pan(boom(rng), 0) * 0.5,
            )
        case Kind.FLAP:
            return lead, clicks(seconds, int(seconds * 90), rng) * 0.6
        case Kind.DECRYPT:
            return lead, pings(
                seconds, int(seconds * 40), (1500, 4200), 0.02, rng
            ) * 0.3
        case Kind.SHRED:
            shred = Track(int(seconds * 1.5 * SR))
            shred.lay(0, whoosh(seconds * 0.8, rng) * 0.45)
            shred.lay(seconds * 0.25, whoosh(seconds * 0.8, rng)[:, ::-1] * 0.45)
            return lead, shred.x


def hall(seconds: float, rng: np.random.Generator) -> Stereo:
    """A synthetic hall: decorrelated noise decaying fastest in the highs."""
    n = int(seconds * SR)
    t = (np.arange(n) / SR)[:, None]
    noise = rng.standard_normal((n, 2))
    bands = (
        ("lowpass", 500.0, 1.0),
        ("bandpass", (500.0, 3500.0), 0.75),
        ("highpass", 3500.0, 0.38),
    )
    ir = sum(
        filt(noise, kind, cut) * np.exp(-6.91 * t / (seconds * rt))
        for kind, cut, rt in bands
    )
    ir = np.concatenate([np.zeros((int(0.018 * SR), 2)), ir])
    return ir / np.sqrt((ir**2).sum() / 2)


def limit(x: Stereo) -> Stereo:
    """A lookahead peak limiter: gain falls 4 ms before a peak and holds for 60 ms after it."""
    need = np.minimum(1.0, CEILING / np.maximum(np.abs(x).max(axis=1), 1e-9))
    hold, ahead = int(0.06 * SR), int(0.004 * SR)
    gain = minimum_filter1d(need, hold, origin=hold // 2 - ahead)
    gain = uniform_filter1d(gain, ahead * 2 + 1)
    return x * np.minimum(gain, minimum_filter1d(need, ahead * 2 + 1))[:, None]


def compose(edit: Edit) -> Stereo:
    beat = edit.beat
    n = int((edit.beats * beat + 0.5) * SR)
    rng = np.random.default_rng(1337)
    energy = np.concatenate(
        [np.full(round(shot.beats), shot.energy) for shot in edit.shots]
    )
    starts = [0, *accumulate(round(shot.beats) for shot in edit.shots)]
    # Sections: runs of shots at one energy, each opening on the tonic.
    opens = [
        k
        for k in range(len(edit.shots))
        if k == 0 or edit.shots[k].energy != edit.shots[k - 1].energy
    ]
    pads, plucks, lows, drums, swells, sfx = (Track(n) for _ in range(6))
    kicks: list[float] = []
    for b, e in enumerate(energy.tolist()):
        k = int(np.searchsorted(starts, b, side="right")) - 1
        section = max(o for o in opens if o <= k)
        first = starts[section]
        bar, into = divmod(b - first, 4)
        last = section == opens[-1]
        root, voicing = CHANGES[0 if last else (bar // 2) % 4]
        at = b * beat
        if into == 0 and (bar % 2 == 0 or b == first):
            pads.lay(at, pad(voicing, (24 if last else 8) * beat, rng) * PADS[e])
        tones = [*voicing, *(t + 12 for t in voicing)]
        for s in range(STEPS[e]):
            note = tones[WALK[(b * STEPS[e] + s) % len(WALK)] % len(tones)] + 12
            plucks.lay(at + s * beat / STEPS[e], pluck(note, rng) * PLUCKS[e])
        if e == 1 and into == 0:
            lows.lay(at, bass(root - 12, 3.8 * beat, rng) * 0.8)
        if e >= 2:
            for s in range(2):
                lows.lay(
                    at + s * beat / 2,
                    bass(root - 12 * (s == 0 or e < 3), 0.42 * beat, rng) * 0.9,
                )
            drums.lay(at, kick())
            kicks.append(at)
            if into in (1, 3):
                drums.lay(at, clap(rng) * 0.45)
            drums.lay(at + beat / 2, hat(rng, 0.03) * 0.28)
        if e >= 3:
            drums.lay(at + beat / 4, hat(rng, 0.02) * 0.14)
            drums.lay(at + 3 * beat / 4, hat(rng, 0.02) * 0.14)
            if into % 2:
                drums.lay(at + beat / 2, hat(rng, 0.16) * 0.12)
            if b == first:
                drums.lay(at, crash(rng) * 0.55)
        # The bar before the energy peaks swells into it, and the peak lands on a boom.
        if b >= 4 and e == energy.max() > energy[b - 1]:
            u = np.linspace(0, 1, int(4 * beat * SR))
            noise = sweep(len(u), 200 * 40**u, rng) * u**2.2
            tone = np.sin(2 * np.pi * np.cumsum(220 * 8**u) / SR) * u**3 * 0.15
            swells.lay(at - 4 * beat, pan(noise * 0.5 + tone, 0))
            drums.lay(at, pan(boom(rng), 0) * 0.8)
    for k, shot in enumerate(edit.shots[:-1]):
        made = sound(shot.cut.kind, shot.cut.beats * beat, rng)
        if made is not None:
            sfx.lay(starts[k + 1] * beat + made[0], made[1])
    # Everything melodic ducks under the kick.
    t = np.arange(n) / SR
    duck = np.ones(n)
    span = int(0.32 * SR)
    dip = 1 - 0.55 * np.exp(-t[:span] / 0.11) * np.minimum(t[:span] / 0.004, 1)
    for at in kicks:
        i = int(at * SR)
        duck[i : i + span] = np.minimum(duck[i : i + span], dip[: n - i])
    # The pads open with the energy, from a dark filter to a bright one.
    bright = np.interp(t, (np.arange(len(energy)) + 0.5) * beat, (energy + 0.6) / 3.6)
    bright = uniform_filter1d(bright, int(beat * SR))[:, None]
    dark = filt(pads.x, "lowpass", 900)
    pad_mix = filt(
        dark + (filt(pads.x, "lowpass", 5200) - dark) * bright, "highpass", 140
    )
    arp = filt(plucks.x, "lowpass", 3800)
    echo = int(0.75 * beat * SR)
    echoes = np.zeros_like(arp)
    for tap in range(1, 6):
        # Ping-pong: odd repeats on the far side.
        echoes[tap * echo :] += (
            arp[: -tap * echo, :: 1 if tap % 2 == 0 else -1] * 0.42**tap
        )
    arp = arp + filt(echoes, "lowpass", 2400)
    hall_ir = hall(3.2, rng)
    send = pad_mix * 0.3 + arp * 0.3 + drums.x * 0.06 + sfx.x * 0.25 + swells.x * 0.2
    wet = np.stack([fftconvolve(send[:, c], hall_ir[:, c])[:n] for c in range(2)], -1)
    melodic = (
        pad_mix * 0.5 + arp * 0.32 + filt(lows.x, "lowpass", 600) * 0.5 + wet * 0.22
    ) * duck[:, None]
    return filt(
        melodic + drums.x * 0.55 + sfx.x * 0.42 + swells.x * 0.35, "highpass", 28
    )


def master(mix: Stereo, edit: Edit) -> Stereo:
    """The mix faded with the picture, brought to the target loudness and held under the
    ceiling."""
    beat = edit.beat
    total = edit.beats * beat
    t = np.arange(len(mix)) / SR
    fade = np.clip(
        np.minimum(
            t / (edit.fade_in * beat), (total - t) / (edit.shots[-1].cut.beats * beat)
        ),
        0,
        1,
    )
    mix = mix * fade[:, None] ** 1.5
    mix = mix * 10 ** ((LUFS - pyloudnorm.Meter(SR).integrated_loudness(mix)) / 20)
    mix = np.tanh(mix * 1.1) / 1.1
    for _ in range(3):
        mix = limit(mix)
        over = np.abs(resample_poly(mix, 4, 1, axis=0)).max() / CEILING
        if over <= 1:
            break
        mix /= over
    return mix


def write(edit: Edit, path: Path, start: float, seconds: float) -> None:
    mix = master(compose(edit), edit)
    wavfile.write(
        path,
        SR,
        mix[round(start * SR) : round((start + seconds) * SR)].astype(np.float32),
    )
