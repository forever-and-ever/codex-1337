"""Synthetic turns: each tick's raw context inflow, before libshimmer paces it."""

from __future__ import annotations

import math
from collections.abc import Sequence
from dataclasses import dataclass
from itertools import accumulate

# Model libshimmer's clock and reservoir and codex's BRISK; only Reach planning uses pacing.
TICK, DRAIN, BRISK = 0.032, 4.0, 100.0


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
    """The model works at the steady rate that brings the quantum total to `quanta` by the segment's
    end, so a shimmer's milestone lands on a chosen beat."""

    quanta: int


type Work = Busy | Wait | Reach


@dataclass(frozen=True, slots=True)
class Segment:
    caption: str
    seconds: float
    work: Work


type Tick = tuple[float, str, str, int]


def pace(rate: float) -> float:
    """Libshimmer pacing, used only to plan a Reach milestone."""
    intensity = rate / BRISK
    return BRISK * (intensity if intensity <= 1.0 else 1.0 + math.log(intensity))


def ticks(segments: Sequence[Segment], count: int, timer: float) -> list[Tick]:
    """The first `count` ticks of `segments`, the last segment holding past its end, as
    `[rate, spell, caption, timer]` with the status timer starting at `timer` seconds. Each `Reach`
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
                    Segment(segment.caption, segment.seconds, Busy(rate)),
                ]
                total, carry = 0, 0.0
                for arrival, *_ in inflow(trial, through, timer):
                    carry += pace(arrival) * TICK
                    whole = math.floor(carry)
                    total += whole
                    carry -= whole
                low, high = (rate, high) if total < segment.work.quanta else (low, rate)
            segments[k] = Segment(segment.caption, segment.seconds, Busy(high))
    return inflow(segments, count, timer)


def inflow(segments: Sequence[Segment], count: int, timer: float) -> list[Tick]:
    reservoir = 0.0
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
        out.append(
            ((forecast + drained) / TICK, spell, segment.caption, int(timer + t))
        )
    return out
