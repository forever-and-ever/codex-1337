# Shimmer reel

A film of codex-tui's stock shimmers, rebuilt from `reel.toml` on demand. The shimmers are the real
ones: the ignored test `shimmers::reel::export` drives each through codex-tui's own engine and live
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
exports are cached by cue sheet and shimmer source in `~/.cache/codex-1337/shimmer-reel/`, newest
four kept. Turing and Cambrian draw from `rand`, so a fresh export of either is a different take.

| Module | Owns |
|---|---|
| `turn.py` | Synthetic turns paced as `live.rs` paces a thread; `reach` lands a token milestone on a beat |
| `edit.py` | `reel.toml` as shots on a beat grid, validated |
| `cells.py` | Exported clips as cell planes; the terminal that paints them |
| `cuts.py` | Camera and transitions |
| `score.py` | The synthesized score and each cut's sound |
| `reel.py` | Export, timeline, lettering, encoding and the command line |
