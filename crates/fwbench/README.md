# fwbench

The foldwave **M0 harness**: drive N pty panes of agent-like output and measure
what the machine does under it.

Covers three of the [`docs/design.md`](../../docs/design.md) §4 targets:

| Measured | How |
| --- | --- |
| **Throughput** | N pty panes flooding at once, each drained on its own reader thread |
| **Input latency** | key echo, timestamped the instant a blocking read returns |
| **Resource attribution** | CPU, RAM, disk and GPU across the run, via [`resmon`](../resmon) (experiment E3) |

Frame timing is deliberately absent: it needs a renderer, which is M3.

**Zero third-party dependencies** — nativelite org crates (`nativelite-pty`,
`nativelite-resmon`) and std only.

## Use

```
cargo run --release --bin fwbench -- combined --panes 8 --mib 32 --samples 120
```

| Mode | What it does |
| --- | --- |
| `flood` | N panes flooding; throughput only |
| `echo` | key-echo latency on an idle machine |
| `combined` | echo latency measured **while** the panes flood — the §4 target |

Options: `--panes N`, `--mib N`, `--samples N`, `--gap-ms N`, `--shell NAME`,
`--no-attrib`.

## First numbers on the reference machine

Windows 11, 16 logical CPUs, 64 GB RAM, RTX 3070, 2026-09-15, release build,
not elevated. `combined --panes 8 --mib 32 --samples 120`:

```
throughput: 8 panes, 32.0 MiB requested each, over 2.78 s
  aggregate   94.9 MiB/s  (263.9 MiB read in total)
  per pane    slowest 11.9 MiB/s, fastest 12.8 MiB/s
  observed/requested ratio 1.03

key echo through `cmd`, 25 ms between keys
  n=120 p50=0.06ms p90=0.09ms p99=11.74ms (min 0.02ms max 11.81ms mean 0.26ms)
```

Against §4: per-pane throughput is **12 MiB/s versus the 17.1 MiB/s** a
1 GB/min pane needs, so ConPTY is roughly 70% of target. Key echo p50 is
comfortably inside the 1 ms target even under the flood; **p99 is not**, for a
reason that is not ours to fix — see below.

## The ConPTY coalescing tick

The most consequential thing this harness has found (design §8.9). The Windows
console coalesces output on a **≈15.2 ms tick**, so key-echo latency through
`cmd` follows `p50 ≈ 15.16 ms − gap` until the gap clears the tick:

| Gap between keys | p50 | p90 |
| --- | --- | --- |
| 0 ms | 15.52 ms | 16.12 ms |
| 5 ms | 10.19 ms | 10.65 ms |
| 10 ms | 5.16 ms | 5.70 ms |
| 13 ms | 2.16 ms | 2.65 ms |
| 15 ms | 0.14 ms | 0.42 ms |
| 16 ms | 0.06 ms | 0.17 ms |

n=60 per row. Two consequences: the 0.1 ms "bare `cmd`" baseline is real but
only at human typing cadence, and the §4 p99 target is **unreachable through
ConPTY** — the wait happens before a terminal ever sees the bytes.

So `--gap-ms` defaults to 25 ms, and any run below 16 ms prints a warning that
the figure is the tick, not the echo.

## Honesty notes

- **Throughput is terminal-observed**, not child output. ConPTY re-renders
  rather than passing bytes through; the report prints the
  observed/requested ratio and explains itself when it strays from 1.0.
- **Attribution undercounts short-lived work.** It differences two snapshots,
  so a process that starts *and* exits inside the window is missing from the
  closing one and contributes nothing. The flood panes themselves are the
  clearest example. Per-pane job objects would fix it; not built yet.
- **Latency is a round trip** through pty write, the console host, the shell's
  line editor and back. It is not foldwave's own input path, which does not
  exist yet.
- The shell under test **never executes anything**: no newline is sent, the
  keys accumulate on an abandoned command line, and the process is killed.

## Develop

From the repository root:

```
python dev.py check
```

## License

MIT, like the other engine crates. The foldwave application is
FSL-1.1-Apache-2.0.
