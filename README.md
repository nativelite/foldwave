# foldwave

A GPU-rendered, high-throughput terminal for agentic work: many agents with heavy
output, steered by one human.

**Status: M5, the beta, under way.** The M4 alpha is met: every performance
target in the design is measured in `fwterm`, foldwave's terminal (scorecard in
[`docs/design.md`](docs/design.md) §10.1). Built for the beta so far: the Mirai
look, a settings sheet, a first run, a one-file installer, and the agent layer's
first slices (atrium's agents and what they need show in the window) and the
activity ribbon.
Building happens with
[atrium](https://github.com/nativelite/atrium) fleets.

One of three products that each own a key piece of agentic development:

| Product | Owns |
| --- | --- |
| [atrium](https://github.com/nativelite/atrium) | running and coordinating many agents |
| [rationale](https://github.com/nativelite/rationale) | the reasoning behind code, anchored to it |
| **foldwave** | seeing and scaling it all: the terminal |

## Design documents

| Document | What it covers |
| --- | --- |
| [`docs/design.md`](docs/design.md) | The design, plus founder decisions (§11) and milestones (§10) |
| [`docs/prior-art.md`](docs/prior-art.md) | Verified facts about 13 open-source terminals: licences, parsers, renderers, scrollback |
| [`docs/compute.md`](docs/compute.md) | GPU compute: CUDA and vendor-neutral options, and experiment E1 (GPU contention) |

The competitive landscape for the whole suite is in
[nativelite/ops `roadmap/competitive-landscape.md`](https://github.com/nativelite/ops/blob/main/roadmap/competitive-landscape.md).

## Crates

| Crate | Concern | License |
| --- | --- | --- |
| [`crates/fwterm`](crates/fwterm) | the terminal: split panes, logs, search, blocks, settings, first run, install | FSL-1.1-Apache-2.0 |
| [`crates/fwrender`](crates/fwrender) | the renderer: cell frames, glyph atlas, Vulkan and CPU backends, the Win32 window | MIT |
| [`crates/resmon`](crates/resmon) | CPU, RAM, disk I/O and GPU use by process tree; drives and CPU sets | MIT |
| [`crates/fwcuda`](crates/fwcuda) | a runtime-loaded binding to the CUDA driver, for the GPU search index | MIT |
| [`crates/fwbench`](crates/fwbench) | measurement harness: pane floods, key-echo latency, log checks | MIT |
| [`crates/fwprobe`](crates/fwprobe) | measures any terminal from outside: key-to-screen latency, throughput | MIT |

## Use

| Keys | Does |
| --- | --- |
| Ctrl+Comma | settings |
| F1 | help: the keys |
| Alt+Shift+Plus / Minus / D | split the focused pane right / down / along its longer side |
| Alt+Arrow, click | move focus between panes |
| Ctrl+Shift+Z | zoom the focused pane, and back |
| Ctrl+Shift+J | show the pane of an atrium agent that needs you: waiting on a decision, finished, or failed |
| Ctrl+Shift+W | close the focused pane |
| Ctrl+Shift+F | search every pane's log, and earlier sessions' |
| Ctrl+Up / Ctrl+Down | jump between commands' prompts |
| Ctrl+Shift+O | select a command's output |
| Ctrl+Shift+[ / ] | fold a command's output, unfold it |

While a program asks for the mouse (atrium with Ctrl+A m, vim, htop), clicks
and the wheel go to it; hold Shift to select or scroll in foldwave instead.

The instrument strip under the title bar shows the agents atrium runs in the
panes, one glyph each by pane (working, waiting on a decision, finished,
failed), so an agent behind a zoomed pane is still present. The activity
ribbon at each pane's right edge maps the lines the pane holds onto its rows:
where commands started, in red where one failed, and where the view is.
`instruments = off` and `ribbon = off` in the settings file (or the settings
sheet) hide them. The strip's SETTINGS and HELP buttons, at its right end,
open the settings sheet and this help.

`fwterm --help` lists every option and key.

## Install (Windows)

`python dev.py package` builds `dist\foldwave-setup-<version>.exe`, one file
that carries everything foldwave needs. Run it: it installs for you alone (no
administrator) into `%LOCALAPPDATA%\Programs\foldwave`, adds foldwave to the
Start menu, to Apps & features and to your PATH (so `fwterm` works in any
terminal opened afterwards), and opens it on its first run. Uninstall
from Apps & features, or with `fwterm --uninstall`; settings and logs are kept.

## Develop

```
python dev.py check     # dependency guard + docs gate + cargo fmt --check + tests
cargo build --release --bin fwterm && target\release\fwterm.exe   # run from source
python dev.py package   # dist\foldwave-setup-<version>.exe
```

There is no CI, so this local check is the sole gate and runs before every push.

## Principles (from the design)

- **Nothing slow on the input or render path.** Every blocking source gets its
  own thread.
- **The log is the truth.** History lives on disk; RAM is a cache; the screen is
  a view.
- **Use the whole machine in balance:** CPU, RAM, SSD and GPU, each for what it
  measurably wins.
- **A crash in one part never takes the ship down.**
- **Zero third-party dependencies.** Inspiration from open source, our own code.
- **Every performance claim measured,** on the reference machine.

## License

The foldwave app (`crates/fwterm`) is [FSL-1.1-Apache-2.0](LICENSE) (founder
decision, 2026-09-15): source-available, for any purpose except a competing
use (making it available in a commercial product or service that substitutes
for it or offers substantially similar functionality), and each release
becomes Apache-2.0 two years after it ships. The engine crates it is built
on, here and in the other nativelite repositories, are MIT, like the rest of
nativelite.
