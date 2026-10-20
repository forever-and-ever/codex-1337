# /// script
# requires-python = ">=3.12"
# ///
"""Build a standalone page for flipping one capture between packs.

Scrolling compares two images; flipping compares two impressions. Every capture is
pre-rendered under every pack into a stacked layer, so switching changes only the paint:
no reflow, no load, no movement of a single glyph.
"""

from __future__ import annotations

import argparse
import html
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True  # sibling modules stay uncompiled in the worktree

from packs import Pack as PackFile


class Pack(PackFile):
    def background(self, color: str) -> str:
        return self.resolve(color, foreground=False)


def mix(fore: str, back: str, alpha: float) -> str:
    a = tuple(int(fore[i : i + 2], 16) for i in (1, 3, 5))
    b = tuple(int(back[i : i + 2], 16) for i in (1, 3, 5))
    return "#" + "".join(
        f"{round(x * (1 - alpha) + y * alpha):02x}" for x, y in zip(a, b, strict=True)
    )


def layer(capture: dict, pack: Pack) -> str:
    rows = []
    for row in capture["rows"]:
        spans = []
        for run in row:
            mods = set(run.get("mods", [])) | pack.style.get(run["fg"], frozenset())
            fore = pack.resolve(run["fg"], foreground=True)
            back = pack.background(run["bg"])
            if "reverse" in mods:
                fore, back = back, fore
            if "dim" in mods:
                fore = mix(fore, back, 0.55)
            style = [f"color:{fore}", f"background:{back}"]
            if "bold" in mods:
                style.append("font-weight:700")
            if "italic" in mods:
                style.append("font-style:italic")
            decoration = " ".join(
                d
                for d, m in (("underline", "underline"), ("line-through", "strike"))
                if m in mods
            )
            if decoration:
                style.append(f"text-decoration:{decoration}")
            spans.append(
                f'<span style="{";".join(style)}">{html.escape(run["text"])}</span>'
            )
        rows.append("".join(spans))
    return "\n".join(rows)


def chips(pack: Pack) -> str:
    cells = [
        f'<div class="chip"><i style="background:{hex_}"></i>{html.escape(role)}<b>{hex_}</b></div>'
        for role, hex_ in pack.colors.items()
    ]
    cells += [
        f'<div class="chip"><i style="background:{hex_}"></i>{ramp} {index}<b>{hex_}</b></div>'
        for ramp, ramp_colors in pack.ramps.items()
        for index, hex_ in enumerate(ramp_colors)
    ]
    return "".join(cells)


PAGE = """<!doctype html>
<meta charset="utf-8">
<title>warpaint</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin: 0; background: var(--canvas); color: var(--faint);
         font: 13px/1.4 "JetBrains Mono", ui-monospace, monospace; }}
  header {{ display: flex; flex-wrap: wrap; gap: .5rem 2rem; align-items: baseline;
            padding: .6rem 1rem; border-bottom: 1px solid var(--rule); position: sticky; top: 0;
            background: var(--prompt); }}
  .group {{ display: flex; gap: .75rem; }}
  .tab {{ cursor: pointer; padding: .1rem .4rem; border-radius: 2px; }}
  .tab[data-on="1"] {{ background: var(--selection); color: var(--ink); }}
  select {{ font: inherit; color: var(--ink); background: var(--selection); border: 0;
            border-radius: 2px; padding: .1rem .4rem; cursor: pointer; }}
  option {{ color: var(--ink); background: var(--prompt); }}
  .hint {{ margin-left: auto; opacity: .55; font-size: 12px; }}
  main {{ position: relative; padding: 1.5rem; display: flex; }}
  .stack {{ position: relative; margin: 0 auto; }}
  pre {{ margin: 0; position: absolute; top: 0; left: 0; visibility: hidden;
         font: inherit; font-size: var(--cell, 15px); line-height: 1.32; white-space: pre; }}
  pre[data-on="1"] {{ position: relative; visibility: visible; }}
  #chips {{ display: none; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
            gap: .35rem; padding: 1rem 1.5rem 2rem; }}
  #chips[data-on="1"] {{ display: grid; }}
  .chip {{ display: flex; gap: .5rem; align-items: center; font-size: 12px; }}
  .chip i {{ width: 1.1rem; height: 1.1rem; border-radius: 2px; outline: 1px solid var(--rule); }}
  .chip b {{ margin-left: auto; opacity: .5; font-weight: 400; }}
</style>
<header>
  <div class="group" id="captures"></div>
  <select id="packs" aria-label="pack"></select>
  <div class="hint">&larr;&rarr; pack &middot; &uarr;&darr; capture &middot; b blink &middot; c chips &middot; +/- size</div>
</header>
<main><div class="stack" id="stack"></div></main>
<div id="chips"></div>
<script>
const CAPTURES = {captures};
const PACKS = {packs};
const CHIPS = {chips};
const LAYERS = {layers};
const SURFACES = {surfaces};
let capture = 0, pack = 0, previous = 1, blink = null, cell = 15;

const stack = document.getElementById("stack");
stack.innerHTML = CAPTURES.flatMap((name, c) =>
  PACKS.map((p, k) => `<pre data-c="${{c}}" data-k="${{k}}">${{LAYERS[name][p]}}</pre>`)).join("");

const captureTabs = document.getElementById("captures");
captureTabs.innerHTML = CAPTURES
  .map((n, i) => `<span class="tab" data-i="${{i}}">${{n}}</span>`).join("");
captureTabs.onclick = (e) => {{
  if (e.target.dataset.i !== undefined) {{ capture = +e.target.dataset.i; paint(); }}
}};

const picker = document.getElementById("packs");
picker.innerHTML = PACKS.map((p, k) => `<option value="${{k}}">${{p}}</option>`).join("");
picker.onfocus = () => {{ clearInterval(blink); blink = null; }};
picker.onchange = () => {{ previous = pack; pack = picker.selectedIndex; picker.blur(); paint(); }};

function paint() {{
  for (const pre of stack.children) {{
    pre.dataset.on = (+pre.dataset.c === capture && +pre.dataset.k === pack) ? "1" : "0";
  }}
  for (const tab of captureTabs.children) {{
    tab.dataset.on = +tab.dataset.i === capture ? "1" : "0";
  }}
  picker.selectedIndex = pack;
  document.getElementById("chips").innerHTML = CHIPS[PACKS[pack]];
  document.documentElement.style.setProperty("--cell", cell + "px");
  for (const [key, value] of Object.entries(SURFACES[PACKS[pack]])) {{
    document.documentElement.style.setProperty("--" + key, value);
  }}
  document.title = CAPTURES[capture] + " · " + PACKS[pack];
}}

addEventListener("keydown", (e) => {{
  const keys = {{ ArrowRight: 1, ArrowLeft: -1, ArrowDown: 1, ArrowUp: -1 }};
  if (e.key === "ArrowRight" || e.key === "ArrowLeft") {{
    previous = pack; pack = (pack + keys[e.key] + PACKS.length) % PACKS.length;
  }} else if (e.key === "ArrowDown" || e.key === "ArrowUp") {{
    capture = (capture + keys[e.key] + CAPTURES.length) % CAPTURES.length;
  }} else if (/^[1-9]$/.test(e.key) && +e.key <= PACKS.length) {{
    previous = pack; pack = +e.key - 1;
  }} else if (e.key === "b") {{
    if (blink) {{ clearInterval(blink); blink = null; }}
    else {{ blink = setInterval(() => {{ const p = pack; pack = previous; previous = p; paint(); }}, 650); }}
  }} else if (e.key === "c") {{
    const chips = document.getElementById("chips");
    chips.dataset.on = chips.dataset.on === "1" ? "0" : "1";
  }} else if (e.key === "+" || e.key === "=") {{ cell += 1; }}
  else if (e.key === "-") {{ cell = Math.max(8, cell - 1); }}
  else {{ return; }}
  e.preventDefault();
  paint();
}});
paint();
</script>
"""


def main() -> int:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--packs", type=Path, default=here / "../../codex-rs/tui/src/warpaint/packs"
    )
    parser.add_argument("--captures", type=Path, default=here / "captures")
    parser.add_argument("--out", type=Path, default=here / "out/flip.html")
    args = parser.parse_args()

    packs = [Pack.load(path) for path in sorted(args.packs.glob("*.toml"))]
    order = ("conversation", "detail", "selection", "picker")
    paths = sorted(
        args.captures.glob("*.json"),
        key=lambda p: (order.index(p.stem) if p.stem in order else len(order), p.stem),
    )
    captures = {path.stem: json.loads(path.read_text()) for path in paths}
    layers = {
        name: {pack.name: layer(capture, pack) for pack in packs}
        for name, capture in captures.items()
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        PAGE.format(
            captures=json.dumps(list(captures)),
            packs=json.dumps([pack.name for pack in packs]),
            chips=json.dumps({pack.name: chips(pack) for pack in packs}),
            layers=json.dumps(layers),
            surfaces=json.dumps(
                {
                    pack.name: {
                        name: pack.colors[key]
                        for name, key in (
                            ("canvas", "ground:canvas"),
                            ("prompt", "ground:prompt"),
                            ("rule", "figure:rule"),
                            ("selection", "ground:selection"),
                            ("ink", "figure:ink"),
                            ("faint", "figure:ink_faint"),
                        )
                    }
                    for pack in packs
                }
            ),
        )
    )
    print(f"{args.out}: {len(captures)} captures x {len(packs)} packs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
