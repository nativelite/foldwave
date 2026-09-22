# fwbench

The foldwave **M0 harness**: drive N pty panes of agent-like output and measure
what the machine does under it.

Covers three of the [`docs/design.md`](../../docs/design.md) §4 targets:

| Measured | How |
| --- | --- |
| **Throughput** | N pty panes flooding at once, each drained on its own reader thread |
| **Input latency** | key echo, timestamped the instant a blocking read returns |
| **Resource attribution** | CPU, RAM, disk and GPU across the run, via [`resmon`](../resmon) (experiment E3) |
| **Parse budget + GPU fit** | tokenize vs screen split, thread scaling, hidden-pane scan, start-state convergence (experiment E4) |
| **Host↔GPU copy cost** | synchronous copies through the CUDA Driver API, via [`fwcuda`](../fwcuda) |

Frame timing is deliberately absent: it needs a renderer, which is M3.

**Zero third-party dependencies** — nativelite org crates (`nativelite-pty`,
`nativelite-resmon`, `nativelite-ansi`, `nativelite-vterm`, `nativelite-fwcuda`)
and std only.

## Use

```
cargo run --release --bin fwbench -- combined --panes 8 --mib 32 --samples 120
```

| Mode | What it does |
| --- | --- |
| `flood` | N panes flooding; throughput only |
| `echo` | key-echo latency on an idle machine |
| `combined` | echo latency measured **while** the panes flood — the §4 target |
| `disk` | durable write rate of the log drive, three ways: the old buffered+fsync probe as control, then unbuffered write-through at queue depth 1 and N |
| `parse` | experiment E4: `vterm` full emulation vs `ansi` tokenizing vs the bare state machine, thread scaling, the structural scan, and how fast every start state converges (the GPU fit) |
| `gpu` | host→GPU and GPU→host copy cost, pinned and pageable, 64 KiB to 64 MiB, after a 2 s warm-up |

Options: `--panes N`, `--mib N`, `--samples N`, `--gap-ms N`, `--shell NAME`,
`--no-attrib`; for `disk`, `--path DIR` (the reference log drive is `E:\`),
`--io-kib N` (default 1024) and `--qd N` (default 8), with `--mib` defaulting
to 2048 per pattern. Every mode ends with a **noise** line: CPU used by processes
outside fwbench's own tree during the run, the heaviest programs by name, and
NOISY when it exceeds `--noise-limit` cores (default 0.05, i.e. 5% of one core).
For `parse`, `--mib` (default 64, per pane), `--reps N`
(default 5) and `--threads 1,2,4,...` (`none` skips scaling). Core placement is
set from outside, so the harness stays free of `unsafe`:
`cmd /c start /b /wait /affinity 555 fwbench parse` runs on one thread per
P-core of the reference i5. For `gpu`, `--samples N` copies per cell (default
200).

```
cargo run --release --bin fwbench -- disk --path 'E:\'
cmd /c start /b /wait /affinity 555 target\release\fwbench.exe parse
cargo run --release --bin fwbench -- gpu
```

`disk` exists because the first E: figure in design §4.1 (479 MiB/s) measured
a flush round-trip loop, not the drive: buffered appends through the page
cache, an `fsync` every 16 MiB, one write in flight. Write-through
(`FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH`) makes every write durable
on return with no page cache and no flush, and a queue keeps the device busy.
The three rows side by side say how much of the gap was the method. Each
write is timed, so the report carries p50/p90/p99, not just a rate. Numbers
inside a drive's SLC cache (2 GiB is) flatter it; a longer `--mib` shows the
sustained rate past it.

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

Key echo p50 is comfortably inside the §4 1 ms target even under the flood;
**p99 is not**, for a reason that is not ours to fix — see the coalescing tick
below. For where the throughput figure sits against §4, see the transport table
next: this run used ConPTY, which is the slow path.

## Transport: ConPTY vs headless pipes

The comparison that decides where throughput comes from. Panes of synthetic
agent output, 32 MiB each, release build, reference machine, 2026-09-15:

| Panes | ConPTY | per pane | Headless pipe | per pane | Pipe advantage |
| --- | --- | --- | --- | --- | --- |
| 1 | 17.2 MiB/s | 17.2 | 238.9 MiB/s | 238.9 | 13.9× |
| 2 | 32.7 MiB/s | 16.4 | 459.0 MiB/s | 229.5 | 14.0× |
| 4 | 65.4 MiB/s | 16.4 | 818.2 MiB/s | 204.6 | 12.5× |
| 8 | 96.6 MiB/s | 12.1 | 1338.7 MiB/s | 167.3 | 13.9× |
| 16 | 139.4 MiB/s | 8.7 | 1854.3 MiB/s | 115.9 | 13.3× |

Against §4 (16 panes × 1 GB/min = 17.07 MiB/s per pane, 273 MiB/s aggregate):
ConPTY **misses by ~2×** and degrades with concurrency; the headless pipe
**exceeds by ~6.8×**.

The pipe figures are a **floor, not a ceiling**: this harness's own generator
produces only ~266 MiB/s per core (measured with output discarded), and the
one-pane pipe result is 88% of that. The pipe path is generator-bound here, so
its real limit has not been found. ConPTY at one pane, by contrast, reaches just
6.5% of what the generator can produce — there the console host is clearly the
limit.

Run both with `--transport pty` (default) and `--transport pipe`.

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
