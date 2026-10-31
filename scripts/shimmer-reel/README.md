# Shimmer reel

A film of codex-tui's stock shimmers, rebuilt from `reel.toml` on demand. The shimmers are the real
ones: the ignored test `shimmers::reel::export` drives each through libshimmer and codex-tui's live
rows, tick by tick, on a synthetic turn, and exports the cells it draws. Everything after that is
generated here: the terminal that paints those cells, the camera, the cuts, the lettering and the
score.

```sh
just shimmer-reel render                 # ~/media/video/codex-1337-shimmers.mp4 (XDG videos)
just shimmer-reel render --span 40 52    # a stretch of it, for checking a change
just shimmer-reel still 13 46.7          # PNGs of chosen instants
just shimmer-reel score --out score.wav  # the music alone
just shimmer-reel sheet Neon Rubik       # contact sheets, every stock shimmer by default
```

`reel.toml` is the edit: its header documents shots, turns, framings, titles, energy and cuts. Change
the shot list and render again. The film is 1080p60 H.264 with AAC; size, rate, tempo, cell width
and glow are `[film]` settings.

The first render after a cue or shimmer change runs the export, which compiles codex-tui's tests;
exports are cached by cue sheet, host source, libshimmer and libshimmer-ratatui identity in
`~/.cache/codex-1337/shimmer-reel/`, newest four kept. Each clip uses a fixed seed, so fresh exports remain deterministic.

| Module | Owns |
|---|---|
| `turn.py` | Raw context-inflow rates; `reach` plans a quantum milestone under libshimmer's pacing |
| `edit.py` | `reel.toml` as shots on a beat grid, validated |
| `cells.py` | Exported clips as cell planes; the terminal that paints them |
| `cuts.py` | Camera and transitions |
| `score.py` | The synthesized score and each cut's sound |
| `reel.py` | Export, timeline, lettering, encoding and the command line |
