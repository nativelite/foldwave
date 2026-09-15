# foldwave

A GPU-rendered, high-throughput terminal for agentic work: many agents with heavy
output, steered by one human.

**Status: design, with M0 underway.** The design lives in [`docs/`](docs/); the
first code is the M0 measurement harness in [`crates/`](crates/). Building
happens with [atrium](https://github.com/nativelite/atrium) fleets.

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
| [`crates/resmon`](crates/resmon) | attribute CPU, RAM, disk I/O and GPU use to process trees (M0 experiment E3) | MIT |

## Develop

```
python dev.py check     # dependency guard + docs gate + cargo fmt --check + tests
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

[FSL-1.1-Apache-2.0](LICENSE) (founder decision, 2026-09-15). The engine crates
foldwave is built on will be MIT, like the rest of nativelite.
