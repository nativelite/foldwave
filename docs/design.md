# foldwave: an agentic terminal

> **Status:** design, 2026-09-15. Founder decisions are recorded in §11; anything
> not listed there is still a proposal. Numbers marked *measured* were
> measured this session in atrium on the founder's Windows machine. Everything
> else is a target or an assumption and says so. Facts about other projects live
> in [`prior-art.md`](prior-art.md), and about CUDA and GPU compute in
> [`compute.md`](compute.md), each with a source. Competitors across the whole
> suite (atrium + rationale + foldwave) are in
> [competitive-landscape.md](https://github.com/nativelite/ops/blob/main/roadmap/competitive-landscape.md) in nativelite/ops.

## 1. The idea

A terminal built for agentic work: many agents, each producing large, fast,
structured output, watched and steered by one human. It should use the whole
machine in balance, not just one CPU thread:

- **CPU** reads and parses.
- **RAM** holds only what is hot.
- **The SSD** keeps everything.
- **The GPU** draws.

## 2. The problem

Traditional terminals assume one human reading one stream at typing speed.
Agent workloads break those assumptions:

| Failure | Why it happens | What agents do to it |
| --- | --- | --- |
| A flood locks the UI | Read, parse and draw share one loop; a slow stage stalls the rest | A test run, a `cat` of a log or a build prints megabytes in seconds |
| History is capped or eats RAM | Scrollback lives in RAM, sized in lines | An agent session produces hours of output; the part you need scrolled out long ago |
| Search is slow or absent | A linear scan of RAM scrollback, one pane at a time | "Where did any agent hit this error?" spans many panes and hours |
| A crash loses everything | Nothing persists | Long agent runs are exactly when you want a replay |
| No structure | The terminal sees a character grid | Prompts, commands, tool calls, diffs and errors are distinct things to agents |
| One noisy pane degrades all | Shared loop, no per-pane budget | Ten agents at once is the normal case |

atrium already hit several of these (§7), and fixing them there is where this
design comes from.

**What the prior art shows** (verified, [`prior-art.md`](prior-art.md)):

| Solved elsewhere | Still missing |
| --- | --- |
| Fast parsing: vectorized control-byte scans (Ghostty, kitty, Windows Terminal, `vte`) | Every surveyed terminal keeps scrollback in RAM |
| GPU drawing: glyph atlas plus damage tracking (Alacritty, Windows Terminal, kitty, Warp) | Windows Terminal saves the buffer to disk only on exit |
| Shell integration: OSC 133 (WezTerm, kitty, Windows Terminal, foot) | Ghostty compresses pages in RAM and lists unlimited scrollback as planned |

Nothing surveyed pages history to disk while running, indexes it, and queries it
across panes. That is this design's opening.

## 3. Goals and non-goals

**Goals (v1)**
- Sustain many panes of heavy output without dropping keys or frames the human is
  watching.
- History that is effectively unbounded, on disk, searchable across panes, and
  survives a crash.
- Structure: commands, outputs and agent events as addressable blocks.
- Balanced use of CPU, RAM, SSD and GPU, adapting to the machine it runs on.
- Every performance claim measured, per [standards/performance.md](https://github.com/nativelite/ops/blob/main/standards/performance.md).

**Non-goals (v1)**
- Replacing the shell or the agent CLIs. Like atrium, we host the user's own
  unmodified tools (the policy guardrails in [agent-terminal-suite.md](https://github.com/nativelite/ops/blob/main/roadmap/agent-terminal-suite.md) apply
  unchanged).
- Remote/SSH multiplexing, tabs-as-a-service, a plugin marketplace.
- Being a general-purpose GUI toolkit.

## 4. Targets (confirmed by the founder, 2026-09-15)

Reference machine (founder, confirmed 2026-09-16) — the four resources foldwave
is built to use together:

| Resource | Part |
| --- | --- |
| CPU | 16 logical cores |
| RAM | 64 GB |
| SSD | **Samsung 990 PRO 1 TB NVMe, drive `E:`** — the log lives here |
| GPU | NVIDIA RTX 3070, 8 GB |

The machine has other drives; they are not part of the target configuration and
no target is stated against them. Every target gets a reproducible benchmark
before it is claimed.

| Area | Target |
| --- | --- |
| Input latency | key echo p50 < 1 ms, p99 < 5 ms over the shell's own latency, **while** other panes flood |
| Throughput | 16 panes each emitting 1 GB/min without falling behind, visible panes ≥ 60 fps |
| Idle | ~0% CPU, no busy polling, GPU idle when nothing changes |
| History | 100 GB per session on disk; open, jump to any line, and search in < 1 s |
| Memory | a fixed, machine-derived RAM budget per pane; RAM never grows with history |
| Crash | after a crash or kill, every pane's history up to the last flush replays |
| Fault isolation | a crash in one part doesn't take the ship down: an agent, a pane, a parser, the indexer, the AI server or the GPU device each fail alone. The failed part restarts or rebuilds from its log within 2 s (target), and every other pane keeps running |

Where atrium stands today (*measured*, Windows, release, output timestamped on a
reader thread): key echo p50 0.4–0.6 ms against 0.1 ms for bare `cmd`, idle CPU
1–2% of a core. atrium keeps no history on disk, and nothing like the
throughput target has been measured.

### 4.1 Measured ceilings on the reference machine

*Measured 2026-09-15/16 (SSD row re-measured 2026-09-21), release builds, M0 harness and direct probes, against
the §4 aggregate target of 273 MiB/s (16 panes at 1 GB/min each). These are what
each leg does in isolation, so a plan for using them together starts from facts.*

| Leg | Ceiling | vs target |
| --- | --- | --- |
| **RAM** — `memcpy`, 1 core | 23,235 MiB/s | 85× |
| **CPU** — `vterm` full emulation, 16 threads (E4, quiet machine, 2026-09-22) | **924.8 MiB/s**; 623.8 on the 6 P-cores alone, 125.5 on one P-core | **3.4×** |
| **SSD (E:)** — unbuffered write-through, 1 MiB appends, one write in flight (`fwbench disk`, 2026-09-21/22) | ≈1,800 MiB/s sustained past the SLC cache (p99 ≈1.1 ms); 4,295 MiB/s held across a 16 GiB burst inside it | **6.6×** |
| **GPU** — RTX 3070 | not yet measured (experiment E1, [`compute.md`](compute.md)) | — |
| *transport:* headless pipe, 16 panes | ≥ 1854 MiB/s | 6.8× (a floor, §8.9) |
| *transport:* ConPTY, 16 panes | 139 MiB/s | **0.51× — misses** |

**What this says about the architecture:**

- **The §4 targets are reachable with room to spare.** With agents headless,
  every leg clears 273 MiB/s: the SSD by 6.6× (sustained), the CPU by 3.4×.
  ConPTY misses outright.
- **CPU parsing is still the binding leg, but it is no longer close.** The SSD
  (≈1,800 MiB/s sustained) and the pipe transport (≥ 1,854) are about 2× faster
  than parsing at 924.8, and RAM is never a constraint. Parsing also has known
  headroom left: ≈38 % of its time is per-token allocation (E4). Engineering
  effort belongs in the parser, not in moving or storing bytes.
  - Two earlier framings were wrong and are recorded so they are not repeated:
    "CPU and SSD are the binding pair" (from the discredited 479 MiB/s SSD
    figure, corrected 2026-09-21), and the first E4 run's lower CPU numbers,
    taken while a browser used ~1.1 cores (corrected 2026-09-22).
- **This quantifies §8.7b's index-construction offload.** 723 MiB/s is *parsing
  alone*, with all 16 threads busy. A background indexer would compete for
  exactly those cores. Moving it to a GPU measured at 0% utilization is now the
  only place that work can go without taking cores from parsing.
- **Parsing scales sublinearly:** 111 MiB/s on one thread, 723 on sixteen — 6.5×
  for 16× the threads, per-thread falling from 111 to 45 MiB/s. Budget cores
  accordingly; do not assume linear fan-out.
- **The SSD is not a binding leg; the earlier 479 MiB/s was an artifact.** The
  first probe (2026-09-16: buffered append, `fsync` every 16 MiB) reported
  479 MiB/s and was wrongly treated as the drive's ceiling. `fwbench disk`
  measures the log writer's own pattern: unbuffered write-through
  (`FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH`, each write durable when it
  returns, no page cache, no flush). Measured on E: on 2026-09-21, release build,
  machine quiet, 2 GiB per pattern, 3 runs per cell, p50/p90/p99 over every
  write (2,048 samples at 1 MiB):

  | pattern | SLC cache fresh | sustained, cache exhausted (≈140 GiB written) |
  | --- | --- | --- |
  | buffered + `fsync`/16 MiB (control) | 1,345 MiB/s, fsync p50 5.9 ms | — |
  | write-through, 1 MiB, qd1 | **3,477 MiB/s**, p99 0.77 ms | **1,670–1,820 MiB/s**, p50 0.5 ms, p99 ≈1.1 ms |
  | write-through, 1 MiB, qd8–32 | 4,581 MiB/s at qd8 | 1,890–2,050 MiB/s, **p99 17–470 ms** |
  | write-through, 256 KiB, qd1 | 2,100–2,300 MiB/s | 1,370–1,630 MiB/s, p99 0.33 ms |
  | write-through, 4 MiB, qd1 | — | 1,480–2,150 MiB/s, p99 ≈2.5 ms |

  What this means:
  - **One write in flight is the right design.** Once the cache is gone, qd1
    gets within ≈10 % of the deepest queue. Deeper queues add almost no
    throughput and multiply tail latency 15–400×. The log writer needs no I/O
    queue.
  - **Plan on the sustained figure, not the cache figure.** 1,750 MiB/s is
    6.4× the target. The SLC figures hold only for bursts of a few tens of GiB.
  - **The trap: NTFS compression.** `E:\` has the NTFS *compressed* attribute
    set, and new directories inherit it, twice catching this harness out. On a
    compressed directory the same code measured 47–61 MiB/s (qd1), 89–108
    (qd8) and 100–143 (control); an independent .NET write-through probe
    agreed at 51 MiB/s. Compression costs **≈35× on durable writes**. Whether
    the 2026-09-16 probe hit the same attribute was **not verified**.
  - **Compression costs reads too** (`fwbench read`, 2026-09-22, 4 GiB probe,
    48 GiB written first to evict the page cache, so the reads are cold):

    | Pass | Compressed | Uncompressed |
    | --- | --- | --- |
    | cold, unbuffered | **535.4 MiB/s** (p50 2.41 ms) | **2,406.2 MiB/s** (p50 0.37 ms) |
    | warm, buffered | 8,522.9 (served from RAM) | 2,287.8 |
    | CPU seconds per GiB | 0.08 | 0.07 |

    So a compressed log reads back **4.5× slower** and with a **6.5× worse
    per-read latency** — history replay, search and checkpoint rebuilds all pay
    it. The CPU cost is small; the throughput cost is not. Warm figures are
    RAM, not the drive, and are reported only as a ceiling.
  - **What compression buys on this drive, measured with `compact /s`:**
    SteamLibrary 414.9 → 400.6 GB (3 %), XboxGames 286.1 → 283.3 (1 %),
    WindowsApps 284.4 → 281.6 (1 %), `E:\ai` 85.1 → 70.6 (17 %). Game and
    store content is already compressed, so squeezing it again returns almost
    nothing. **Verdict: the log directory must be uncompressed, and the
    drive-wide attribute is a bad trade** — ~30 GB saved on a 914 GB drive
    against 35× writes and 4.5× reads. The log writer should still refuse, or
    warn, on a compressed directory; `fwbench read` reads the attribute the
    same way (`FILE_ATTRIBUTE_COMPRESSED`).
  - **§8.0 is now stale.** It is built on ingest being SSD-bound at 479 MiB/s,
    and that is no longer true. The SSD has more headroom than any leg except
    RAM, and CPU parsing (723 MiB/s) is the binding leg. To be revised with the
    founder.
- **Ignore buffered write figures.** Every drive tested reported ≈3.6 GB/s
  buffered, because that is the OS page cache, not the device. Only durable
  (`fsync`) figures are real.
- **These are ceilings measured alone; how they compose is §8.0.** Used naively
  they do not add up. The pattern that keeps parsing off the ingest path — and
  the core budget that follows from it — is derived there.

## 5. Principles

1. **Nothing slow on the input or render path.** Every blocking source gets its
   own thread and wakes the loop the moment something arrives. (Learned in atrium,
   measured: screen writes, size queries and a file scan each froze or delayed
   the loop.)
2. **Bounded queues; skip ahead, never replay.** When a consumer falls behind,
   drop and resync from the source of truth. Replaying stale frames made a
   resumed terminal scroll old output for 5+ seconds (*measured*).
3. **The log is the truth; RAM is a cache; the screen is a view.** Any pane's
   screen can be rebuilt from its log plus a checkpoint.
4. **Adapt to the machine.** Budgets come from what is free, not from constants
   (atrium's memory guard and compile pool are the pattern).
5. **Measure against the baseline**, with timestamps taken where data arrives
   (a harness that sleeps adds its own latency: we measured 1.6 ms "bare cmd"
   that was really 0.1 ms).
6. **One concern per package.** The engine is a set of crates, each useful alone.

## 6. Build vs. reuse

**Founder direction (2026-09-15):** don't reinvent the wheel where it isn't
needed, but question whether the existing way is the best way. Take inspiration
from good open-source projects and make it our own. Much of the prior art is
genuinely open source, and AI makes studying, porting and optimizing it cheap.

The binding rule is now in [standards/zero-dependency.md](https://github.com/nativelite/ops/blob/main/standards/zero-dependency.md) ("Reusing open-source
work"):
- **Inspiration is the default:** study the design, write our own code. No
  licence notice is needed.
- **Copying or closely translating substantial code** keeps the original
  notice. MIT requires it too, so a `THIRD-PARTY-NOTICES.md` entry is the cost.

How that fits the org's non-negotiables:

- **Zero third-party *runtime dependencies* still holds.** Reuse is inspiration
  or porting into our own crate, never a crates.io dependency. The shipped
  artifact stays nativelite code plus std.
- **Licenses decide what is reusable.** Our libraries are MIT.
  - MIT code ports cleanly, with attribution: Ghostty, WezTerm, Windows
    Terminal, foot, Rio, xterm.js, and `vte`, which is dual MIT/Apache.
  - Apache-2.0-only code (Alacritty, Contour) keeps its own terms and NOTICE
    wherever it lands, so a crate built from it isn't pure MIT.
  - GPL/AGPL code (kitty, Zed's terminal crate, most of Warp) is for reading
    ideas only, never copying.
  - Per-project licenses, with sources, are in [`prior-art.md`](prior-art.md).
- **Reused code must also be rewritten on std.** The fastest parsers get their
  speed from dependencies: Highway and simdutf (Ghostty), memchr (`vte`, Rio). We
  port the technique, not the dependency.
- **Every component gets a short decision record** before it is built:

  | Question | Answer required |
  | --- | --- |
  | What exists? | The best 1–3 implementations, with license and source |
  | Is it the best way? | Measured or reasoned weakness for *our* workload (many panes, huge history, agents) |
  | Decision | **Inspiration** (our own code, the default), **port** (keeps the source's notice), **port and improve** (say what), or **reimplement** (say why reuse loses) |
  | Proof | The benchmark or test that shows the decision was right |

The rule is in [standards/zero-dependency.md](https://github.com/nativelite/ops/blob/main/standards/zero-dependency.md) ("Reusing open-source work"),
added on the founder's direction (§11).

## 7. What we start from

Org crates, all zero-dependency and already in production in atrium:

| Crate | Gives us | Gap for this design |
| --- | --- | --- |
| `pty-rs` | ConPTY/POSIX pty, `spawn_suspended`, blocking `PtyReader` for reader threads | none known for v1 |
| `rawterm-rs` | raw input, `Terminal::input()` for an input thread, `size()` from any thread | a GUI terminal takes input from its window instead; still useful for the TUI host |
| `ansi-rs` | VT parser, `Screen` grid, screen diff | the parser is byte-at-a-time; `Cell` stores `char` plus a full `Style` per cell |
| `vterm-rs` | VT100 emulator over `ansi::Screen` | **no scrollback**; synchronized output copies the whole screen per update |
| `uwidth` | grapheme and width tables | none known |
| `agsess`, `abus` | agent session status, board + bus | the block/event layer builds on these |
| atrium | resource governance (session job/cgroup, memory guard, compile pool), event-driven loop, non-blocking screen queue | it renders through a host terminal; this design owns its window |

Lessons carried over from atrium (each *measured* this session):

| Lesson | Measured |
| --- | --- |
| The agent-status refresh walked all history every second | 12% idle CPU, 170 ms p90 key echo; fixed to 1.4% / 7.5 ms |
| Per-pane timed reads | 65 ms key echo median at 8 panes; event-driven readers brought it to ~0.5 ms |
| A terminal that stops reading froze the loop | on Windows the size query blocked 63 s |
| Windows console input stalls with its output | when the host stops draining output, keys stop too, so a Windows design must keep ConPTY output drained at all times |
| Paused spawn into a job | a pane's children can't escape the memory limit |

## 8. Architecture

```text
            ┌──────────────── resource governor (budgets, pressure, frame pacing) ───────────────┐
            │                                                                                      │
 agent CLI ─► pty reader thread ─► parse worker ─► pane state (RAM: screen + hot tail)
  (per pane)        │                   │                 │
                    │                   │                 ├─► renderer (GPU): glyph atlas, damaged rows only
                    ▼                   ▼                 │
              pane log (SSD) ◄── block/semantic marks     └─► search / block queries
              append-only raw bytes, line index, checkpoints
                    │
                    └─► indexer (background): full-text + marks, across panes
```

### 8.0 How the four legs compose

*§4.1 measures each leg **alone**. This section is the pattern that uses them
**together**: what runs where, what is allowed to block what, and what the whole
thing costs at the §4 target of 273 MiB/s (16 panes, 17.1 MiB/s each). Every
constant below is a measured §4.1 number; everything derived from them is marked
**derived**, and every assumption is marked **unmeasured**. Nothing here has been
measured in composition yet — experiments E4 and E5 below are exactly that.*

**The invariant the rest of this section falls out of:**

> **The log is the truth. Screens, indexes and frames are caches of it.**

A screen can be rebuilt from a checkpoint plus log bytes. An index can be rebuilt
by re-reading segments. A frame can be drawn again. None of the three is
load-bearing for correctness, so each may be delayed, degraded or dropped
independently — and **no derived stage may ever block ingest.** That single rule
is what turns four legs into a pipeline instead of a chain.

**1. The ingest path is two legs, and neither of them is the CPU.**

A byte is safe once it is in RAM and on its way to the log. Parsing is not on
that path (§8.1: raw bytes are logged *before* parsing), so the ingest ceiling is
the slowest of the legs that *are* on it:

| Ingest leg | Ceiling | vs 273 MiB/s |
| --- | --- | --- |
| headless pipe transport | ≥ 1854 MiB/s | 6.8× |
| RAM, one copy into a slab | 23,235 MiB/s | 85× |
| **SSD, log append at a 16 MiB cadence** | **479 MiB/s** | **1.75×** |

**Ingest is SSD-bound at 479 MiB/s — 1.75× the target — and the 723 MiB/s parse
ceiling does not appear in the bound at all.** That is the whole trick: the
binding pair of §4.1 stops being a pair, because only one of the two sits on the
critical path.

The measured flush optimum also explains its own ceiling, which is a good sign
the number is real (**derived**):

| | Fill 16 MiB | `fsync` p50 | writer duty p50 | `fsync` p99 | writer duty p99 |
| --- | --- | --- | --- | --- | --- |
| at the 273 MiB/s target | 58.6 ms | 26.7 ms | **46 %** | 47.3 ms | 81 % |
| at the 479 MiB/s ceiling | 33.4 ms | 26.7 ms | 80 % | 47.3 ms | **142 % — over** |

One dedicated writer thread sustains the target with the p99 still inside its
budget, and saturates at exactly the rate we measured as the ceiling. It needs
one segment filling and one in flight: **32 MiB of buffer**, against 64 GB.

**2. Fan-out is free, so every consumer reads the same bytes.**

Parsing, log writing, indexing and rendering all read the *same* slab; nothing is
copied twice. Three concurrent consumers at target cost 819 MiB/s of read
bandwidth — **3.5 % of a single core's memcpy rate** (**derived**). RAM is not a
stage in the pipeline, it is the place the other three legs meet, and adding a
consumer is nearly free. RAM is the only leg with orders of magnitude to spare
(85×), and this is what we spend that headroom on.

**3. The CPU parses what is *observed*, not what *arrives*.**

§8.2 already says hidden panes keep no live screen. Priced out, that is the
largest single lever in the design (**derived**, 16 panes at target, 4 visible):

| Panes | Bytes/s | Work | CPU |
| --- | --- | --- | --- |
| 4 visible | 68.3 MiB/s | full VT parse | **under one core** at the measured 111 MiB/s single-thread rate |
| 12 hidden | 204.8 MiB/s | structural scan only — line starts, marks | ~12–14 % of a core at the measured 1.4–1.75 GiB/s scan (E4, 2026-09-21) |

**foldwave never parses 273 MiB/s, so the 723 MiB/s ceiling never binds.** We
parse the visible fraction, which at 4 visible panes fits in roughly one core
rather than the six-plus that full-parsing every pane would demand.

**4. Checkpoint interval is the knob between pane-switch latency and write
amplification.**

The cost of not parsing a hidden pane is paid when someone looks at it: rebuild
from the last checkpoint by replaying log bytes. For checkpoint interval `I` and
the measured single-thread parse rate `p` = 111 MiB/s, rebuild ≈ `I / p`, so
holding a pane switch to one 60 Hz frame gives (**derived**):

`I ≤ 16 ms × 111 MiB/s` → **checkpoint every ≈1.8 MiB of pane output**, which at
17.1 MiB/s per pane is one checkpoint per ≈104 ms.

That is paid for in log traffic: at an **unmeasured** checkpoint size of 200 KiB
it is 11 % write amplification, taking the log from 273 to ≈303 MiB/s — still
inside the 479 MiB/s ceiling. Both constants in that trade are unmeasured, so the
governor (§8.8) owns the interval and adapts it. The formula, not the number, is
the design.

**5. The GPU is never on the ingest path.**

Everything placed on the GPU in §8.7b — index construction, bulk scans, damage
passes — is re-derivable from the log and latency-tolerant. So a GPU stall (TDR,
contention with rendering, a driver reset, §8.8a) degrades search or a frame; it
can never stall ingest or lose a byte. This is a placement *rule*, not just a
list: **work moves to the GPU only if losing the GPU entirely would cost nothing
but time.**

**6. The composed budget at the §4 target.**

Every stage on its own threads and its own queue, nothing ever waiting on a
device it does not own (**derived** — and the thing E5 exists to check):

| Stage | Threads | Cost at 273 MiB/s |
| --- | --- | --- |
| pane readers | 16, blocking | a few % of one core in total (the copy is 1.2 %; syscall overhead **unmeasured**) |
| log writer | 1, owns E: | 46 % duty |
| parse, visible panes | 1–2 | ~68 MiB/s, under one core |
| structural scan, hidden panes | shared | ~20 % of a core (**unmeasured**) |
| renderer | 1 | GPU-bound |
| indexer | GPU, or deferred | 0 on the CPU when offloaded (E1) |

**≈3 of 16 logical cores at full target load.** That is §8.7b's rule — *foldwave
adds near-zero CPU to a machine the agents have already taken* — put in numbers
for the first time.

**7. Backpressure is a choice of which cache to let go stale.**

The shed order in §8.8 is not a list of tricks, it is this invariant applied.
Each step drops a *cache*, never the truth, in increasing order of what the user
notices: hidden-pane screens, then index construction, then background frame
rates, then flush cadence, then parse workers. Ingest and the focused pane's
input are never traded — the first is correctness, the second is the product.

**What this is not yet.** The composition above is arithmetic over six numbers
measured in isolation. Legs that do not interfere on paper routinely interfere in
practice: the page cache, the memory controller and the Windows scheduler all get
a vote. Two experiments settle it, and E4 feeds E5's parameters.

**Experiment E4: the parse budget (part of M0).** §4.1 measured parsing at 1
thread and at 16; §8.0 interpolates between them.

| Step | Measure |
| --- | --- |
| Scaling curve | full VT parse at 1, 2, 4, 6, 8, 12, 16 threads — locate the knee, since the budget above assumes near-single-thread efficiency at 1–2 threads |
| Structural scan | the hidden-pane path (line starts + marks, no state machine) in MiB/s per core — assumed ≥1 GiB/s above, never measured |
| Checkpoint | serialized screen + parser state: its size in bytes, and the wall time to rebuild a screen from a checkpoint plus `I` bytes of log |

Output: the real constants behind points 3 and 4, and therefore whether one core
really does cover visible-pane parsing.

**E4 result (re-measured 2026-09-22 on a quiet machine; supersedes the
2026-09-21 run).** Release build, `fwbench parse` and `fwbench gpu`, 64 MiB of
the harness's agent-like output per pane, fed 64 KiB at a time, 5 timed reps
after a warm-up. Core placement was set with `start /affinity`: `0x555` is one
thread per P-core, `0xFFF` P-cores plus their hyper-threads, `0xFFFF`
everything.

*Machine state.* The founder closed the browser and paused Google Drive first.
The noisy-run guard (`crates/fwbench/src/noise.rs`) reported **0.16–0.27 cores** used outside fwbench
during these runs, of which **0.09–0.14 was the Claude Code session driving
them**; the rest was Samsung Magician, the Intel graphics service, Windows
Terminal and Google Drive at 0.01–0.04 each. The first run of these same
benchmarks, on 2026-09-21, was taken while a browser burned ~1.1 cores, and it
measured **25–50 % low**. Those numbers are gone; these replace them.

*The split, one thread on a P-core:*

| Stage | Median MiB/s | Share of full emulation time |
| --- | --- | --- |
| `vterm` full emulation (tokenize + apply to screen) | 123.4 | 100 % |
| `ansi::Parser` tokenize, one heap token per token | 206.9 | 60 % |
| The state machine alone ([`fsm.rs`](../crates/fwbench/src/fsm.rs) table) | 565.6 | 22 % |
| All 11 start states in lockstep (enumerative, no shortcut) | 383.3 | 32 % |
| Structural scan, hidden-pane path (lines + OSC 133) | 1,798.4 | 7 % |

- **Applying tokens to the screen is ≈40 % of the time, tokenizing ≈60 %.**
  Only 22 % is the state machine itself; the other ≈38 % is `ansi` allocating
  a `String`/`Vec` per token. That part is recoverable on the CPU, with no
  GPU, by a tokenizer that emits spans into a caller-owned buffer.
- **The hidden-pane scan runs at 1.8 GiB/s per core,** comfortably above the
  ≥1 GiB/s the §8.0 budget assumed.
- **Core placement matters more than any tuning here:** the same single thread
  measured 125.5 MiB/s on a P-core, 100.4 on CPU 0 (which also takes the
  machine's interrupts) and 55.4 on an E-core.

*Scaling, full emulation, one pane per thread, aggregate median MiB/s:*

| Threads | P-cores only (`0x555`) | P + HT (`0xFFF`) | All 16 (`0xFFFF`) |
| --- | --- | --- | --- |
| 1 | 125.5 | 125.6 | 127.2 |
| 2 | 232.1 | 232.2 | 229.8 |
| 4 | **436.3** | 411.4 | 412.4 |
| 6 | **623.8** | 544.3 | 544.5 |
| 8 | — | 628.8 | 666.6 |
| 12 | — | 743.0 | 801.4 |
| 16 | — | — | **924.8** |

- **Parsing clears the §4 target on two P-cores** (232 MiB/s ≈ 0.85× target)
  and beats it on four (436 MiB/s, 1.6×). The whole machine reaches
  **924.8 MiB/s, 3.4× target** — above the 723 MiB/s §4.1 recorded on
  2026-09-16 and 51 % above the noisy 2026-09-21 run.
- **Six threads pinned to the six P-cores (623.8) beat eight threads spread
  over P-cores and hyper-threads (628.8) at three quarters of the thread
  count,** and beat six unpinned threads by 15 %. Pin parser threads to
  P-cores, then spill.
- **Hyper-threads and E-cores earn their keep only once the P-cores are full:**
  12 threads add 128 MiB/s over 8, and 16 add 123 over 12, against ~110 MiB/s
  for each of the first four P-core threads.
- Scaling is smooth and monotonic in every column, unlike the noisy run.
  Per-thread efficiency falls from 125.5 (1 thread) to 104.0 (6 P-cores) to
  57.8 (all 16).

*The GPU fit.* The workload was sampled at 10,000 chunk boundaries. From each
of the 11 start states, the table counts the bytes needed to reach the state
the true (ground) trajectory is in, within a 64 KiB chunk. These figures are
deterministic and unchanged from the 2026-09-21 run.

| Start state | Converged | p50 / p99 / max bytes | Never |
| --- | --- | --- | --- |
| Ground, UTF-8 (3 states) | 100 % | 0–1 / 1 / 1 | 0 |
| Escape | 98.6 % | 1 / 6 / 8 | 141 |
| EscapeIntermediate | 100 % | 1 / 6 / 8 | 0 |
| Csi | 100 % | 1 / 10 / 18 | 0 |
| Osc, OscEsc, Str, StrEsc | 0 % | — | 10,000 |

In the true stream, 96.56 % of byte offsets are in ground, 2.46 % in CSI and
0.98 % in escape. None fall in a string, because the workload contains no OSC
or DCS strings.

- **Parallel tokenizing fits.** Every non-string start state rejoins ground
  within 18 bytes. A chunk parsed from an unknown state therefore costs one
  pass plus at most 18 bytes of extra work per state.
- **The exception is string states**, and the fix is cheap. A string absorbs
  plain text until BEL or `ESC \`, so it never converges. The 141 escape
  misses are the same case: the byte after the boundary was `]`, `P`, `X`,
  `^` or `_`, which opens a string. Carry a "string open" flag through each
  chunk with a terminator scan (1.8 GiB/s), then parse each chunk from ground.
  The enumerative lockstep, which needs no convergence shortcut at all, still
  runs at 383.3 MiB/s, 68 % of the single trajectory.
- **Transfer is not the obstacle.** `fwbench gpu` (RTX 3070, PCIe 4.0 x16, CUDA
  Driver API, 2 s warm-up, 200 copies per cell):

  | Copy | Pinned host memory | Pageable |
  | --- | --- | --- |
  | 64 KiB | 4.9 GiB/s, 12 µs p50 | 4.0 GiB/s |
  | 1 MiB | 18.5 GiB/s H2D, 20.1 D2H, 53 µs p50 | 10.9 / 8.8 GiB/s |
  | 16 MiB | 22.5 / 24.2 GiB/s | 11.7 / 9.9 GiB/s |
  | 64 MiB | 22.6 / 24.3 GiB/s | 10.1 GiB/s |

  At the §4 target, 273 MiB/s in 1 MiB pinned batches keeps the link busy
  ≈1.4 % of the time, and a 1 MiB copy (53 µs) costs 0.6 % of parsing that MiB
  on one P-core (≈8.1 ms). §8.7b's "transfer alone would exceed the parse
  cost" is wrong for throughput by more than 100×.

**What E4 says.** Tokenizing is data-parallel enough for a GPU and cheap to
ship to it, but the CPU is no longer short of headroom: 924.8 MiB/s is 3.4×
the target, and a non-allocating tokenizer should lift the per-core figure
further. So the GPU case now rests on *giving cores back to the agents*
(§8.7b's actual goal), not on reaching the target. Applying tokens to the
screen (≈40 %) stays sequential per pane on the CPU. Still unmeasured: a real
GPU tokenizer kernel end to end (the next spike), and checkpoint size and
rebuild time (`vterm` has no checkpoint yet).

**Experiment E5: the composed pipeline (part of M0).** The first measurement of
the legs *running together* instead of one at a time.

| Step | Measure |
| --- | --- |
| Composed rate | read + log + parse + scan concurrently, 16 panes, ramping past the predicted 479 MiB/s ceiling: where it actually breaks, and which leg breaks first |
| Interference | each leg's solo rate re-measured while the others run — how much of §4.1 survives composition |
| **One log stream or sixteen** | §8.4 specifies a log *per pane*, but the 479 MiB/s optimum was measured on **one** sequential stream with a single `fsync` cadence. Sixteen interleaved streams with sixteen cadences is unmeasured and may be far worse. Measure both; if a single device-level log with pane-tagged records wins, §8.4 changes, and per-pane line indexes and checkpoints stay separate files |
| Latency under load | key-echo p50/p99 and pane-switch time while all of the above runs, against the §4 targets |

Until E4 and E5 run, everything in §8.0 is **a proposal with arithmetic behind
it**, not a measured result.

### 8.1 I/O (CPU)
- One blocking reader thread per pane (`PtyReader`), a bounded inbox, one wake
  signal to the core. This is atrium's `events.rs`, proven.
- Raw bytes are appended to the pane log **before** parsing, so a parser bug or
  a crash never loses output.

### 8.2 Parsing (CPU)
- A VT state machine with a **fast path for plain text**: find the next escape
  or control byte a word or block at a time, and copy printable runs in bulk.
  Portable SIMD is not in stable std, so candidates are word-at-a-time (SWAR)
  tricks in plain Rust, and `std::arch` intrinsics behind runtime feature
  detection. Decide by benchmark.
- This is the technique every fast parser in the survey converges on:
  - scan for ESC/C0 bytes vectorized
  - bulk-decode the plain-text run
  - enter the state machine only at control bytes

  Sources in `prior-art.md`. Ours must do it on std alone.
- Parsing runs on worker threads, never the render thread. kitty does the same
  (parsing on a separate thread from rendering).
- **Hidden panes don't need a live screen.** They log and index; their screen is
  rebuilt from the last checkpoint plus the log tail when shown. This is the
  largest single saving with many agents.
- Prior art to evaluate for port-or-improve: see `prior-art.md` (parsers).

### 8.3 Memory model (RAM)
- Per pane: the visible screen plus a **hot tail** of recent lines, sized by a
  budget from the governor.
- Compact line storage: text as UTF-8 bytes plus runs of attributes (not a
  full style per cell); styles interned in a per-pane table; wide characters and
  graphemes handled with `uwidth`.
- Older lines live only in the log; scrolling back pages them in (§8.4).
- Budget: a share of *available* memory, adjusted under pressure (the memguard
  pattern).

### 8.4 Storage (SSD)
Per pane, an append-only log, in segments — though **whether that is one
log per pane or a single device-level log with pane-tagged records is open:** the
479 MiB/s figure below was measured on a *single* sequential stream, and
experiment E5 (§8.0) decides it.


| Part | Content |
| --- | --- |
| data segments | raw pty bytes as received, fixed-size segments (e.g. 64 MiB) |
| line index | byte offset of each line start, plus timestamp, for O(1) jump-to-line |
| checkpoints | periodic serialized screen + parser state, so rebuilding a view replays at most one interval |
| marks | semantic boundaries: prompt, command, output, exit code, agent events (§8.7) |

- Writes are batched and flushed on a cadence and at checkpoints; the governor
  watches write latency.
- Cold segments may be compressed (std has no compressor: a small LZ-family
  codec would be its own crate; decide by measurement whether it pays).
- Reads for scrolling go through a page cache sized by the RAM budget (explicit
  reads first; memory-mapping needs our own FFI and is a measured decision).
- Crash safety: append-only, and the index and checkpoints are rebuildable from
  data segments.
- **Flush cadence has a sharp optimum, measured on E: (Samsung 990 PRO).**
  Sustained 256 KiB appends with a periodic `fsync`, 2 GiB written:

  | Flush every | Sustained | fsync p50 | p90 | p99 |
  | --- | --- | --- | --- | --- |
  | 4 MiB | 190 MiB/s | 7.8 ms | 10.2 ms | 709 ms |
  | **16 MiB** | **479 MiB/s** | 26.7 ms | 31.4 ms | 47.3 ms |
  | 64 MiB | 445 MiB/s | 115 ms | 153 ms | 162 ms |
  | 256 MiB | 126 MiB/s | 417 ms | 12,760 ms | 12,760 ms |

  Both directions off the optimum are bad, and the slow direction is
  catastrophic: at a 256 MiB cadence the drive stalls for **12.7 seconds** at the
  tail, which would be visible as a frozen pane. At 16 MiB the log sustains
  479 MiB/s, 1.75× the §4 target.
  - **The governor's flush rule starts at ≈16 MiB** and adapts from measured
    write latency (§8.8), never from a constant.
  - **`fsync` must never block a pane.** Even at the optimum, p50 is 26.7 ms and
    p99 is 47 ms — two orders of magnitude above the input-latency target. The
    log writer is therefore asynchronous to the reader: bytes reach the log
    buffer on the read path, but durability is a background cadence, and "the
    history up to the last flush replays" (§4) is exactly what that buys.
- **On other machines, where the log lives is a measured decision.** The
  reference configuration fixes it to E: (§4), but elsewhere the log writer
  probes candidate locations for durable write latency at startup, picks,
  records which it chose and surfaces it. Never assume the working directory is
  fast: a spinning disk measured on this same machine gave a 26.5 ms fsync p50
  and 124 MiB/s durable, which would miss the §4 target on storage alone.

### 8.5 Search and queries (CPU + SSD)
- A background indexer over data segments: an n-gram (e.g. trigram) index per
  segment for substring search, and a marks index for structure.
- Queries run across panes and time: "errors in any pane in the last hour",
  "every command agent X ran". Results link to log positions and blocks.

### 8.6 Rendering (GPU)
- A glyph atlas (rasterize each glyph once) and batched, instanced draws of
  visible cells; redraw only damaged rows; no frame when nothing changed.
- The renderer reads pane state and the page cache; it never waits on I/O or
  parsing. Background panes render at reduced rates; hidden panes don't render.
- **Decided (founder, 2026-09-15): our own GPU bindings, not `wgpu`.** wgpu
  wasn't designed around agentic workloads, and it's a dependency.
- **Recommended backend order, for Windows first with portability built in**
  (founder: "lay the ground for easy use, stick to compatible systems"):
  1. **Vulkan first.** One binding renders on Windows *and* Linux, on NVIDIA,
     AMD and Intel, and the same binding gives vendor-neutral **GPU compute**
     (§8.7a). The Vulkan loader ships with GPU drivers (measured locally:
     `vulkan-1.dll` 1.4.341 alongside the NVIDIA driver).
  2. **CPU fallback from day one.** Vulkan is often missing under remote
     desktop and in VMs, and the fallback doubles as the correctness reference.
  3. **Metal for macOS later.** macOS has no native Vulkan, and a translation
     layer would be a third-party dependency.
  4. **Direct3D 11 only if the M3 spike shows Vulkan-on-Windows falls short**
     (driver quirks, remote desktop). Windows Terminal chose Direct3D with a
     Direct2D fallback, and that choice is worth studying.
- All backends sit behind one small draw interface (upload glyph atlas, draw
  instanced cells, present), so a new platform is a backend, not a rewrite.
- Font rasterization follows §6: inspiration from permissive rasterizers, our
  own code.
- A CPU fallback (software rendering into a window bitmap) keeps it usable
  without a GPU and is the correctness reference for the GPU path. Windows
  Terminal (Direct2D) and WezTerm (Software) both keep one for remote desktop.
- Don't assume the GPU wins throughput: foot renders on the CPU and beat a GPU
  terminal on vtebench (sources in `prior-art.md`). The GPU buys smooth frames
  and large grids; parsing and storage decide throughput.

### 8.7 The agent layer
- **Blocks:** prompt, command, output and exit status from shell-integration
  marks (OSC 133 family), plus agent events from a side channel (atrium's bus is
  the starting point), stored as marks in the log.
- **Status:** agent session state from transcripts (`agsess`), shown per pane.
- **Resource accounting per pane:** CPU, memory, disk and GPU share, and the
  job/cgroup limits and compile pool from atrium.
- **Views:** fold blocks, diff-aware rendering, jump between an agent's commands,
  cross-pane query results (§8.5).

### 8.7a GPU compute (CUDA and alternatives)
Details, sources and licensing are in [`compute.md`](compute.md).

**Founder direction (2026-09-15):** use the GPU wherever it helps, balancing
SSD, RAM, GPU and CPU. The operating rule that makes this safe: **every GPU path
has a CPU path and a benchmark**, and the governor (§8.8) routes work to
whichever wins on this machine right now.

Candidates, each proven by benchmark before it ships:

| Candidate | Primary backend |
| --- | --- |
| Rendering | Vulkan (§8.6) |
| Local AI: summaries, error clustering, embeddings for semantic search, explaining a failed block | out-of-process model server |
| Bulk scans over history the index can't answer: substring, regex, highlighting | Vulkan compute (vendor-neutral), CUDA optional |
| **Index construction** over log segments (trigram build), continuous and batchable | Vulkan compute, CUDA optional |
| Compressing and decompressing cold log segments | our own compute kernels, measured against CPU |
| Colour and highlight passes over large visible regions, and diff rendering | Vulkan compute |
| Image output in panes (sixel/kitty-style images) | decode and scale on the GPU |

- **Local AI over agent output** is the best fit: summarize floods, cluster
  errors, semantic search, explain a failing block.
  - It runs in a local model server the user runs (llama.cpp's `llama-server` or
    Ollama, both MIT, both HTTP).
  - The terminal talks HTTP and links no CUDA, so it stays zero-dependency and
    vendor-neutral.
- **Bulk GPU search** is a spike, not a plan.
  - Published GPU regex speedups exclude transfer time.
  - The index answers most queries on the CPU.
  - GPU scanning stays only if an end-to-end benchmark (transfer included)
    beats a multithreaded CPU scan.
- **Index construction is the strongest new candidate** (added 2026-09-15).
  - The indexer trigram-indexes every byte of every pane's log, continuously, at
    the §4 throughput target — a steady multi-core load landing exactly when the
    agents have already taken the CPU (§8.7b).
  - It is bulk, batchable and latency-tolerant: nothing waits on it, so a
    transfer hop costs nothing the user can feel. That is the opposite of the
    bulk-search case, where transfer decides the answer.
  - Same rule as everything else here: it ships only if an end-to-end benchmark
    beats the multithreaded CPU build while leaving frame time inside §4
    (experiment E3, §8.7b).
- **If GPU scanning is kept:** CUDA through the Driver API (`nvcuda.dll` /
  `libcuda.so`, both installed with the driver) with prebuilt kernels, so
  nothing NVIDIA is redistributed. Vulkan compute for AMD/Intel, Metal on macOS,
  and a CPU fallback everywhere.
- **Out:** GPUDirect Storage (Linux datacenter only), DirectStorage
  (proprietary redistributable, not peer-to-peer; [`compute.md`](compute.md)),
  nvCOMP (proprietary), and CUDA for input or rendering. VT *tokenizing*
  became a GPU candidate on 2026-09-21 (E4, §8.7b); applying tokens to the
  screen stays on the CPU.
- **Rule:** CUDA is one optional backend, detected at runtime, never required.

### 8.7b Work placement: what can and cannot move off the CPU

**Founder observation (2026-09-15):** the CPU sits at 80–99% while the GPU
reads 0–4%. The instinct is right — the silicon is idle — but offload has to
be aimed at work that is genuinely GPU-shaped, and most of what pins the CPU on
an agentic machine is not.

*Measured while writing this section (reference machine, 2026-09-15):*
`nvidia-smi` reported **0% GPU utilization and 1975 MiB of 8192 MiB used**, while
two runaway `find / -iname ... -path *tree-sitter-rust*` processes left over from
an earlier agent session had burned **6.85 and 6.82 CPU-hours** since 10:39,
pinning 2 of 16 logical cores and thrashing the filesystem. The machine was not
CPU-bound by work a GPU could absorb; it was CPU-bound by a bug that nothing
surfaced. That is an argument for per-pane resource accounting (§8.7), not for
GPU compute.

| Work | Placement | Why |
| --- | --- | --- |
| Agent processes, compilers, test runs (`rustc`, `node`, `pytest`) | **CPU, permanently** | Not ours to move, and not data-parallel. Nothing offloads `rustc` to a GPU. Our duty is to *not add* to the load (§8.8) and to make runaway work visible (§8.7) |
| Local AI over agent output | **GPU, out of process** | The largest genuine CPU→GPU transfer available: on the GPU it costs ~0 CPU, on the CPU it would take 8–16 threads |
| Index construction over log segments | **GPU candidate, measure** | Bulk, batchable, latency-tolerant, embarrassingly data-parallel, and it runs continuously *while* agents already own the CPU. The best-shaped non-AI candidate (§8.7a) |
| Bulk unindexed scans | **GPU spike, measure** | Transfer-bound below a threshold; see [`compute.md`](compute.md) |
| Damage/diff and colour passes over large visible regions | **GPU, small win** | Data-parallel but small next to the rows above |
| Glyph rasterization | **CPU, cached** | One cost per glyph, then it lives in the atlas. Only a large CJK font load makes it interesting |
| Cold-segment compression | **CPU first** | [`compute.md`](compute.md) verdict; revisit only if measured |
| VT parsing | **Split (reopened 2026-09-21 on E4 data):** tokenizing is a **GPU candidate**; applying tokens to the screen stays **CPU** | Was "CPU, decided" because the parser is sequential and "transfer alone would exceed the parse cost". E4 measured both claims. Non-string start states converge within 18 bytes, so chunks parse in parallel (strings are handled by a terminator scan). A 1 MiB pinned copy costs 0.6 % of parsing that MiB. Screen apply (≈38 % of emulation time) is sequential per pane and stays on the CPU. Ships only if a GPU tokenizer kernel beats the CPU end to end (next spike) |
| Line-index / newline scanning | **CPU, decided** | Data-parallel, but CPU SIMD does it at tens of GB/s — PCIe transfer would dominate |
| Input handling | **CPU, decided** | Already sub-millisecond; any GPU hop makes it worse |

**The rule this yields:** the goal is not to raise GPU utilization. It is that
**foldwave adds near-zero CPU to a machine the agents have already taken.** GPU
offload is one way to get there. Not doing the work at all (hidden panes keep no
live screen, §8.2) and backing off under pressure (§8.8) are the others, and
they are cheaper.

Every row marked *measure* follows the §8.7a rule: a CPU path, a GPU path, and a
benchmark that picks between them on this machine, right now.

**Experiment E3: CPU attribution (part of M0).** Where does the CPU actually go
during a representative agentic session on the reference machine?

| Step | Measure |
| --- | --- |
| Baseline | A representative session (N agents building, testing, emitting output), CPU time attributed per process tree: agents, compilers, shells, and foldwave itself |
| foldwave internals | Within foldwave, CPU split by stage: pty read, parse, log write, index build, render, governor |
| Under flood | The same during the §4 throughput load, to find which stage grows fastest |
| Offload trial | Index construction moved to the GPU: CPU seconds returned, and the frame-time cost measured against E1's contention numbers ([`compute.md`](compute.md)) |

Output: a table saying what share of a pinned CPU foldwave is responsible for,
and which stage is worth moving. Until E3 runs, the *measure* rows above stay
proposals.

### 8.8 The resource governor
**CPU pressure is the first input, not one of many.** On an agentic machine the
agents have already taken the CPU (§8.7b, measured). The governor's primary job
is to keep foldwave's own cost near zero under that load; balancing the GPU comes
second. Earlier drafts of this section had it the other way round.

- Inputs, in priority order: **CPU pressure** (load per core, run-queue depth,
  and foldwave's own share of it), then available memory and commit, pane RSS
  (job/cgroup), disk write latency, frame time, and which panes are visible and
  focused.
- Outputs: RAM budgets per pane, flush cadence, how many parse workers, frame
  rate per pane, whether a hidden pane keeps a live screen, **whether index
  construction runs on the CPU, runs on the GPU, or waits**, build or agent
  limits, and GPU time shared between rendering and compute.
- **Under CPU pressure, shed in this order:** stop keeping live screens for
  hidden panes; move index construction to the GPU or defer it; lower background
  pane frame rates; lengthen the flush cadence; cut parse workers. The focused
  pane's input and frames are never traded.
- How much compute slows rendering on a shared GPU is **unknown**: no official
  guidance exists, so experiment E1 in `compute.md` measures it and sets the
  governor's rule.
- Windows' TDR resets the GPU only when a *single* GPU task can neither finish
  nor be preempted within 2 s. Long AI jobs are many short tasks and are
  unaffected; only our own scan kernels must keep each launch short (sources in
  `compute.md`).
- Rule: the focused pane's input and frames come first; background work yields.

### 8.8a Fault isolation and recovery
Founder requirement (2026-09-15): if one part crashes, it recovers and doesn't
take the ship down.

| What fails | Contained by | Recovery |
| --- | --- | --- |
| An agent or shell | its own process tree, in a per-pane job (Windows) or cgroup (Linux), as atrium does today | the pane shows the exit; restart on request; history stays in the log |
| A pane's parser or emulator (panic on a hostile byte stream) | parsing runs in a **pane host** process per pane (or per small group), not in the UI process | the supervisor restarts it; the screen rebuilds from the last checkpoint plus the log tail |
| The indexer | its own worker process | restart and resume from the last indexed segment |
| The local AI server | already out of process (§8.7a) | the feature degrades; the terminal keeps running; retry with backoff |
| The GPU device (driver reset, TDR, hot-unplug, remote desktop) | the renderer treats device loss as normal | recreate the device and rebuild the glyph atlas from pane state; CPU fallback meanwhile |
| The UI process itself | pane hosts and logs outlive it | a restarted UI reattaches to running pane hosts, or replays logs (atrium's `recover` is the seed) |
| Disk full or I/O errors | the log writer | surface it loudly, keep live screens in RAM, drop to a bounded in-RAM tail, never silently lose |

- **The supervisor** is a small, boring process with no parsing and no GPU. It
  starts pane hosts and the indexer, watches heartbeats, restarts with backoff,
  and holds the session job, so nothing outlives it by accident (atrium's
  reap/job/watchdog code is the seed).
- **Isolation costs something:** an IPC hop per pane between host and UI. The
  M0 harness measures whether shared-memory screen snapshots keep that inside
  the latency target. If not, parsing moves back in-process behind panic
  isolation, and that decision gets written down.

### 8.9 Platform notes
- **Windows:** console apps need ConPTY, so a console host always sits between
  us and the agent CLI. Measured consequences:
  - **the console coalesces output on a ≈15.2 ms tick** (*measured 2026-09-15,
    M0 harness, reference machine*). Key-echo latency through `cmd` follows
    `p50 ≈ 15.16 ms – gap` for keystroke gaps below the tick, and collapses to
    ≈0.1 ms once the gap clears it:

    | Gap between keys | p50 | p90 |
    | --- | --- | --- |
    | 0 ms | 15.52 ms | 16.12 ms |
    | 1 ms | 14.10 ms | 14.62 ms |
    | 5 ms | 10.19 ms | 10.65 ms |
    | 8 ms | 7.15 ms | 7.65 ms |
    | 10 ms | 5.16 ms | 5.70 ms |
    | 13 ms | 2.16 ms | 2.65 ms |
    | 15 ms | 0.14 ms | 0.42 ms |
    | 16 ms | 0.06 ms | 0.17 ms |

    n=60 per row, `fwbench echo`, timestamped on arrival on a reader thread.

    Consequences:
    - The 0.1 ms "bare `cmd`" figure in §4 is real, but only at **human typing
      cadence**, where the console is idle between keys. It is not a floor that
      survives fast input.
    - Input arriving faster than the tick – pasted text, a held key, an agent
      driving a TUI – pays up to a full tick. No terminal on top of ConPTY can
      undercut this, because the wait happens before we ever see the bytes.
    - It shows up in the tail even at a comfortable cadence: at a 25 ms gap,
      p50 was 0.06 ms but p99 was 11.74 ms (*measured, under an 8-pane flood*).
      **The §4 p99 < 5 ms target is therefore not reachable through ConPTY**,
      only on the headless and pty transports.
    - Which strengthens the headless-over-pipes preference in the transport
      table below: it is not only the fastest path, it is the only one without
      this floor.
    - **Not yet isolated:** whether the tick is ConPTY's own render timer, the
      Windows scheduler's ≈15.6 ms timer resolution, or `cmd`'s input
      handling. Experiment E2 (§11) should now answer a sharper question:
      does OpenConsole show the same period?
  - keep every pane's output drained at all times, or its input stalls
  - spawn suspended so a pane joins its job before it runs

- **The app itself takes no console:** `#![windows_subsystem = "windows"]` on the
  binary crate sets the PE subsystem to `WINDOWS` (2) instead of `CONSOLE` (3),
  so the loader never allocates a console and no host window flashes up at
  launch. foldwave owns its own window (§8.6), so this is the right setting for
  the app. *(Verified 2026-09-15: subsystem byte 3 → 2; launched detached, a
  console binary reports a console attached and a `windows`-subsystem binary
  reports none.)*
  - **It is not a silencer for stdout.** Handles are still *inherited*: run from
    a shell or with redirection, `println!` works normally. What changes is that
    nothing is *allocated* when there is no parent console, so output then goes
    nowhere. Diagnostics must therefore not depend on stdout: use a log file, or
    `AttachConsole(ATTACH_PARENT_PROCESS)`, or keep a console in development
    with `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`.
  - **It applies to the app only,** never to `fwbench` or `resmon`, whose whole
    purpose is to print.
  - **It changes nothing about child processes.** Console apps we host still
    need ConPTY, and the ≈15.2 ms coalescing tick above is a property of the
    console host's handling of *their* output, not of our subsystem. This
    removes a popup; it does not touch the latency floor.

  Windows Terminal bundles its own **OpenConsole** (MIT) instead of relying on
  the system `conhost.exe`, to get newer ConPTY behavior (sources in
  `prior-art.md`). Whether we bundle it is decided by experiment E2 (§11).
  Agents avoid the console host entirely by running headless.
- **Linux/macOS:** a POSIX pty is a kernel device, so there is no console host
  in between: this is the closest to bare metal. cgroups (Linux) for hard
  limits. macOS runtime testing is still thin in the org.
- **Why Windows can't go bare metal for interactive programs:**
  - Windows console programs (`cmd`, PowerShell, an interactive `claude`) don't
    write VT bytes to a device. They call the Windows Console API ("put this
    text at row 5", "read a key event").
  - Something must implement that API and turn it into a byte stream. That is
    the console host (conhost, or OpenConsole), and ConPTY is how a terminal
    attaches to one.
  - There's no kernel pty to talk to instead.
- **Ways around the console host, by case:**

  | Case | Path | Console host? |
  | --- | --- | --- |
  | Interactive Windows console apps | ConPTY + Windows' built-in console host | Yes, and unavoidable (already part of Windows); its cost is measured in M0 |
  | Agents run **headless** (e.g. `claude -p --output-format stream-json`, a documented public surface per [agent-terminal-suite.md](https://github.com/nativelite/ops/blob/main/roadmap/agent-terminal-suite.md)) | plain pipes | **None.** Structured events instead of an emulated TUI, the fastest and most "agentic" path; foldwave renders the events itself |
  | Linux tools under WSL | a real Linux pty inside WSL | Still bridged to Windows through WSL's relay (cost to measure in M0) |
  | Windows programs that only read and write stdio (build tools, many CLIs) | plain pipes | None, but they see no TTY and may drop colour or progress output |

  **Measured 2026-09-15 (M0 harness, reference machine, release):** the two
  transports are not close. Aggregate throughput, panes of synthetic agent
  output, 32 MiB each:

  | Panes | ConPTY | per pane | Headless pipe | per pane | Pipe advantage |
  | --- | --- | --- | --- | --- | --- |
  | 1 | 17.2 MiB/s | 17.2 | 238.9 MiB/s | 238.9 | 13.9× |
  | 2 | 32.7 MiB/s | 16.4 | 459.0 MiB/s | 229.5 | 14.0× |
  | 4 | 65.4 MiB/s | 16.4 | 818.2 MiB/s | 204.6 | 12.5× |
  | 8 | 96.6 MiB/s | 12.1 | 1338.7 MiB/s | 167.3 | 13.9× |
  | 16 | 139.4 MiB/s | 8.7 | 1854.3 MiB/s | 115.9 | 13.3× |

  Against the §4 target of 16 panes at 1 GB/min each (17.07 MiB/s per pane,
  273 MiB/s aggregate):
  - **ConPTY misses it by about 2×** and degrades with concurrency: a single
    pane just reaches the per-pane target, 16 panes reach half of it.
  - **The headless pipe exceeds it by ≈6.8×**, and that is a floor, not a
    ceiling: our own workload generator tops out at ≈266 MiB/s per core
    (*measured, output discarded*), and the one-pane pipe figure of 238.9 MiB/s
    is 88% of that. The pipe path is **generator-bound in this harness, not
    transport-bound**, so its real ceiling is higher and has not been found.
  - ConPTY at one pane reaches only 6.5% of what the generator can produce, so
    there the console host is unambiguously the limit.
  - None of this includes parsing, rendering or logging. These are transport
    ceilings, not foldwave's speed.

  Design consequence: foldwave supports all three transports (ConPTY,
  pipes/headless, pty) behind one pane interface. Agent integrations prefer
  headless where the agent documents it — now backed by a measured 13×—14×
  throughput margin as well as the absence of the coalescing tick above.

## 9. Package plan

One concern each; names are placeholders.

| Package | Concern | Reuse candidates (see §6, `prior-art.md`) |
| --- | --- | --- |
| `panelog` | append-only pane log: segments, line index, checkpoints, marks | reimplement: nothing surveyed does it |
| `vtfast` or `ansi-rs` 0.x | fast-path VT parser | port the technique from `vte` (MIT), Windows Terminal's `stateMachine.cpp` (MIT) or Ghostty (MIT), on std only |
| `vterm-rs` next | emulator with compact lines, checkpoint/restore, no whole-screen clones | extend ours; study Ghostty's `PageList` (MIT) and Windows Terminal's `textBuffer` (MIT) |
| `textindex` | n-gram index over log segments | reimplement likely (small) |
| `governor` | budgets from machine pressure | extract from atrium (memguard, compile pool, job/cgroup) |
| `glyphs` | font loading and rasterization | evaluate a permissive rasterizer first |
| `gpu-*` | per-platform draw backends (D3D11, Metal, Vulkan/GL) + CPU fallback | study Windows Terminal's AtlasEngine (MIT, D3D + D2D fallback) and Ghostty's Metal/OpenGL renderers (MIT) |
| `http-rs` (or reuse `web-server-rs` parts) | minimal HTTP client over std sockets, for a local model server | our own; std has none |
| `gpuscan` (spike) | optional GPU text scan: CUDA Driver API, Vulkan/Metal later, CPU fallback | spike only if the M0 harness shows unindexed scans matter |
| the app | window, input, panes, blocks, views, composition | new, reusing atrium's agent layer |

## 10. Milestones

| Milestone | Deliverable | Proof |
| --- | --- | --- |
| **M0 Harness** | throughput and latency benchmark: N panes of recorded agent output, timestamped key echo, frame timing, RSS, disk rate. vtebench (MIT/Apache) supplies workloads, but it measures only PTY read speed, not latency or frames, so it can't be the harness. Includes experiments E1, GPU contention (`compute.md`), E2, built-in console host vs OpenConsole (§11), and **E3, CPU attribution** (§8.7b) | a baseline table for atrium today and for 1–2 existing terminals; the E1 contention table on the RTX 3070; the E3 attribution table that says which stage is worth offloading |
| M0 status (2026-09-15) | **Partly delivered.** `resmon` (four-resource attribution, E3) and `fwbench` (pane floods over ConPTY *and* headless pipes, key-echo latency) are in `crates/`. Headline numbers: 16 panes sustain **139 MiB/s over ConPTY** vs **1854 MiB/s over pipes** (13.3×), against a 273 MiB/s target; key echo under an 8-pane flood p50 **0.06 ms**, p99 **11.74 ms**. Still to build: frame timing (needs M3), E1, E2, a disk throughput/latency probe, and **E4 and E5** (§8.0 — the parse budget, and the first measurement of the legs running composed rather than one at a time) | see `crates/fwbench/README.md` and §8.9 |
| **M1 Headless core** | `panelog` + fast parser + `vterm` checkpoints, driven by the harness with no UI | throughput and history targets met headless |
| **M2 atrium adopts it** | unbounded history, search and crash replay inside atrium | atrium benchmarks don't regress; replay after a kill |
| **M3 Renderer spike** | one platform (Windows, D3D11), glyph atlas, damaged-row redraw, CPU fallback | frame time and latency under flood vs. baseline |
| **M4 App alpha** | window, panes, blocks, cross-pane search, governor | all §4 targets measured on the reference machine |

M1–M2 pay off in atrium before any GPU work, which de-risks the expensive part.

## 11. Decisions

Founder answers of 2026-09-15.

| # | Question | Decision | Notes / still open |
| --- | --- | --- | --- |
| 1 | Name, product | **foldwave**, a new product | Anime-themed (Macross), picked from vetted shortlists; see Naming below. atrium continues and adopts the engine crates (M2) |
| 2 | App license | **FSL-1.1-Apache-2.0**, like atrium | Engine crates stay MIT |
| 3 | Reusing open source | **Inspiration over original code.** Copying is a rare exception | Now in [standards/zero-dependency.md](https://github.com/nativelite/ops/blob/main/standards/zero-dependency.md). Inspiration needs no notice; any copied code keeps its MIT/Apache notice (a legal requirement) |
| 4 | GPU binding | **Our own bindings, not wgpu** | Backend order recommended in §8.6: Vulkan, CPU fallback, Metal later, D3D11 only if the spike needs it. To confirm after the M3 spike |
| 5 | Platforms | **Windows first**, groundwork for others | Vulkan and a pane-transport interface keep Linux close; macOS gets Metal later |
| 6 | Console host | **Decide by measurement (experiment E2).** Founder wants the most performant option | Built-in host vs bundled OpenConsole, below. Either way agents run headless over pipes where they document it, and Linux/macOS use ptys |
| 7 | GPU compute | **Use the GPU wherever it helps**, balanced across SSD, RAM, GPU and CPU | Every GPU path gets a CPU path and a benchmark; §8.7a lists the candidates; local AI stays out of process |
| 8 | Targets | **Confirmed**, plus **fault isolation**: a crash recovers without taking the ship down | §4 targets; §8.8a design |
| 9 | Leveraging the whole machine | **Aim offload at work that is GPU-shaped, not at the utilization number.** The governor's first input is CPU pressure | Founder observation 2026-09-15: CPU 80–99% while the GPU idles. §8.7b places every stage and names what will never move; index construction (§8.7a) is the new GPU candidate; E3 measures it |

**Experiment E2: built-in console host vs OpenConsole (part of M0).**

| Item | Detail |
| --- | --- |
| Hosts compared | Windows' built-in `conhost.exe` (10.0.26100.1 on the reference machine) and `OpenConsole.exe` from Windows Terminal 1.24, already installed there (measured 2026-09-15) |
| Measured through each | flood throughput, key echo p50/p99 during a flood, and console-host CPU and memory |
| Cost of testing OpenConsole | the standard pseudo-console call always starts the built-in host, so it needs our own launch code, inspired by Windows Terminal's `winconpty` (MIT) |
| Sharpened by M0 (2026-09-15) | the built-in host coalesces output on a ≈15.2 ms tick (§8.9, measured). The first question for OpenConsole is whether it shows the same period, since that tick, not our code, sets the p99 floor for every ConPTY pane |
| Decision rule | bundle it (optional, falling back to the built-in host) only if it is clearly better on throughput or p99 latency; otherwise don't |

**Naming: foldwave (founder, 2026-09-15).**
- **Meaning:** in Macross, a fold wave is a faster-than-light signal sent through
  fold space. That fits a terminal carrying many agents' output at speed.
- **Why it was picked:** built from ordinary English words, so franchise risk
  is lower than a coined term; easy to say and spell.
- **Availability** (checked 2026-09-15): `foldwave` is free on crates.io (API)
  and in the nativelite GitHub org.
- **Conflicts:** the only use found in 2 web searches was a Red Dot concept for a
  foldable microwave (https://www.red-dot.org/project/foldwave-portable-microwave-with-foldable-structure-65817).
- **Still to do:** a trademark search in the software category before the first
  public release (neutral-branding guardrail in [agent-terminal-suite.md](https://github.com/nativelite/ops/blob/main/roadmap/agent-terminal-suite.md)).

How we got here:
- **Rejected for conflicts:**

  | Name | Conflict |
  | --- | --- |
  | orchestrate | IBM watsonx Orchestrate |
  | istari | Istari Digital; Contrast Security's multi-agent "istari" |
  | funnels | AI marketing funnels |
  | divergence | an agent-terminal competitor, divergence.sh |
  | tachikoma, misaka, omoikane, lorien, numenor, silmaril, veda, amonhen | existing AI-agent products |
- **Rejected on taste:** weathertop.
- **Taken on crates.io:** magi, sibyl, amadeus, bebop and others.
- **Runners-up that were clear:** iserlohn, lastorder.

## Appendix: measured this session (atrium, Windows, release)

| Change | Before | After |
| --- | --- | --- |
| Agent status refresh off the loop | idle CPU 12.3%, key echo p90 170 ms | 1.4%, 7.5 ms |
| Only the focused pane waits | 8 panes: p50 65 ms | 6.2 ms |
| Event-driven readers | p50 ~6 ms (1 pane), ~65 ms (8 panes) | 0.4–0.6 ms, harness timestamping on arrival; bare `cmd` 0.1 ms |
| Terminal that stops reading | loop frozen; size query blocked 63 s | loop runs; output queued, dropped past 256 KiB, repainted |
| agsess refresh (439 sessions) | ~135 ms | ~35 ms |

Harness: a release atrium in a pty; idle CPU from process CPU time over 10 s;
key echo is the time from writing a key to its echo arriving on a reader thread.
The 8-pane p50 before event-driven readers was measured with the older, sleeping
harness, so it overstates atrium's own cost somewhat.
