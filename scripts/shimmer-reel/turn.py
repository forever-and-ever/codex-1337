"""Synthetic turns: the ticks the live feed would hand a shimmer while an agent works through a
sequence of segments, reckoned as `live.rs` reckons them: output forecast at the model's rate, each
tool result draining into the stage, the sum paced and carried in whole tokens."""

from __future__ import annotations

import math
import re
from collections.abc import Sequence
from itertools import accumulate
from dataclasses import dataclass
from pathlib import Path

STAGE = Path(__file__).resolve().parents[2] / "codex-rs/tui/src/shimmers/stage.rs"


def constant(name: str) -> float:
    found = re.search(rf"const {name}: f64 = ([0-9.]+);", STAGE.read_text())
    assert found, f"stage.rs declares no {name}"
    return float(found[1])


TICK, DRAIN, KNEE = (constant(name) for name in ("TICK", "DRAIN", "KNEE"))


@dataclass(frozen=True, slots=True)
class Busy:
    """The model works, its output entering the context at `rate` tokens a second."""

    rate: float


@dataclass(frozen=True, slots=True)
class Wait:
    """A tool runs; its result's `tokens` enter the context as the wait ends."""

    tokens: float


@dataclass(frozen=True, slots=True)
class Reach:
    """The model works at the steady rate that brings the paced total to `tokens` by the segment's
    end, so a shimmer's milestone lands on a chosen beat."""

    tokens: int


type Work = Busy | Wait | Reach


@dataclass(frozen=True, slots=True)
class Segment:
    header: str
    seconds: float
    work: Work


type Tick = tuple[int, str, str, int]


def pace(rate: float) -> float:
    """`stage::pace`: the rate itself up to the knee, logarithmic beyond."""
    return rate if rate <= KNEE else KNEE * (1 + math.log(rate / KNEE))


def ticks(segments: Sequence[Segment], count: int, timer: float) -> list[Tick]:
    """The first `count` ticks of `segments`, the last segment holding past its end, as
    `[tokens, spell, header, timer]` with the status timer starting at `timer` seconds. Each `Reach`
    becomes the steady rate that meets it."""
    segments = list(segments)
    ends = list(accumulate(segment.seconds for segment in segments))
    for k, segment in enumerate(segments):
        if isinstance(segment.work, Reach):
            through = round(ends[k] / TICK)
            low, high = 0.0, 20_000.0
            for _ in range(48):
                rate = (low + high) / 2
                trial = [
                    *segments[:k],
                    Segment(segment.header, segment.seconds, Busy(rate)),
                ]
                total = sum(tick[0] for tick in paced(trial, through, timer))
                low, high = (rate, high) if total < segment.work.tokens else (low, rate)
            segments[k] = Segment(segment.header, segment.seconds, Busy(high))
    return paced(segments, count, timer)


def paced(segments: Sequence[Segment], count: int, timer: float) -> list[Tick]:
    reservoir = carry = 0.0
    ends = list(accumulate(segment.seconds for segment in segments))
    out: list[Tick] = []
    current = 0
    for tick in range(1, count + 1):
        t = tick * TICK
        while current < len(segments) - 1 and t >= ends[current]:
            if isinstance(work := segments[current].work, Wait):
                reservoir += work.tokens
            current += 1
        segment = segments[current]
        drained = reservoir * -math.expm1(-TICK / DRAIN)
        reservoir -= drained
        match segment.work:
            case Busy(rate):
                forecast, spell = rate * TICK, "busy"
            case Wait():
                forecast, spell = 0.0, "wait"
            case Reach():
                raise AssertionError("ticks() resolves every Reach before pacing")
        carry += pace((forecast + drained) / TICK) * TICK
        whole = math.floor(carry)
        carry -= whole
        out.append((whole, spell, segment.header, int(timer + t)))
    return out
