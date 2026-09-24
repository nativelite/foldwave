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
| Input latency | key echo p50 < 1 ms, p99 < 5 ms over the shell's own latency, **while** other panes flood — **measured against the full composed pipeline** (§8.0): p50 0.08 ms, p99 0.68 ms at target, +0.03 ms p99 even at 12.5× target |
| Throughput | 16 panes each emitting 1 GB/min without falling behind, visible panes ≥ 60 fps |
| Idle | ~0% CPU, no busy polling, GPU idle when nothing changes |
| History | 100 GB per session on disk; open, jump to any line, and search in < 1 s |
| Memory | a fixed, machine-derived RAM budget per pane; RAM never grows with history |
| Crash | after a crash or kill, every pane's history up to the last flush replays — **measured** (E6, §8.0): 9 kills, every log an exact prefix, no torn tail |
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
| **CPU** — `vterm` full emulation, 16 threads (E4, quiet machine, 2026-09-22, span tokenizer) | **1,828.3 MiB/s**; 1,158.2 on the 6 P-cores alone, 214.0 on one P-core | **6.7×** |
| **SSD (E:)** — unbuffered write-through, 1 MiB appends, one write in flight (`fwbench disk`, 2026-09-21/22) | ≈1,800 MiB/s sustained past the SLC cache (p99 ≈1.1 ms); 4,295 MiB/s held across a 16 GiB burst inside it | **6.6×** |
| **GPU** — RTX 3070 | not yet measured (experiment E1, [`compute.md`](compute.md)) | — |
| *transport:* headless pipe, 16 panes | ≥ 1854 MiB/s | 6.8× (a floor, §8.9) |
| *transport:* ConPTY via the built-in `conhost.exe`, 16 panes | 139–145 MiB/s | **0.51× — misses** |
| *transport:* ConPTY via OpenConsole 1.24 (E2, 2026-09-22), 16 panes | **433–444 MiB/s** | **1.6×** |

**What this says about the architecture:**

- **The §4 targets are reachable with room to spare.** With agents headless,
  every leg clears 273 MiB/s: the SSD by 6.6× (sustained), the CPU by 6.7×.
  ConPTY misses outright.
- **There is no single binding leg any more.** After the span tokenizer
  (`nativelite-ansi` 0.4.0, `nativelite-vterm` 0.5.3), the three working legs
  land within 3 % of each other: CPU 1,828 MiB/s, SSD ≈1,800 sustained, pipe
  transport ≥ 1,854. RAM is never a constraint. A plan that needs more than
  ~1.8 GiB/s has to move *every* leg, not one.
  - Three earlier framings were wrong and are recorded so they are not
    repeated: "CPU and SSD are the binding pair" (from the discredited
    479 MiB/s SSD figure, corrected 2026-09-21); the first E4 run's low CPU
    numbers, taken while a browser used ~1.1 cores (corrected 2026-09-22); and
    "CPU parsing is the binding leg", true only while the tokenizer allocated
    a token per token (corrected the same day, below).
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
| **SSD, unbuffered write-through appends** | **≈1,800 MiB/s sustained** (4,295 inside the SLC cache) | **6.6×** |

**Ingest is SSD-bound at ≈1,800 MiB/s, 6.6× the target.** Parsing does not
appear in the bound at all: it is not on the path. *(Until 2026-09-21 this read
"SSD-bound at 479 MiB/s"; that figure was a buffered-plus-`fsync` probe, and a
compressed directory, not the drive — §4.1.)*

E5 then ran the whole pipeline rather than reasoning about it: **16 panes
logging, parsing and scanning at once reached 1,312–2,429 MiB/s**, and at the
target rate the machine was ~92 % idle. The composition holds.

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
| 4 visible | 68.3 MiB/s | full VT parse | **a third of a core** at the measured 208–214 MiB/s single-thread rate (E4, span tokenizer) |
| 12 hidden | 204.8 MiB/s | structural scan only — line starts, marks | ~12–14 % of a core at the measured 1.4–1.75 GiB/s scan (E4, 2026-09-21) |

**foldwave never parses 273 MiB/s, so the parse ceiling never binds.** We parse
the visible fraction, which at 4 visible panes fits in a third of a core rather
than the 1.3 cores that full-parsing every pane would demand. E5 measured the
whole composed load at **≈1.3 of 16 cores** at target — §8.0's predicted ≈3 was
pessimistic by about 2×.

**4. Checkpoint interval is the knob between pane-switch latency and write
amplification.**

The cost of not parsing a hidden pane is paid when someone looks at it: rebuild
from the last checkpoint by replaying log bytes. For checkpoint interval `I` and
the measured single-thread parse rate `p` = 208 MiB/s (E4, span tokenizer),
rebuild ≈ `I / p`, so holding a pane switch to one 60 Hz frame gives
(**derived**):

`I ≤ 16 ms × 208 MiB/s` → **checkpoint every ≈3.3 MiB of pane output**, which at
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
| log writer | per pane | 2.3 % of pane time (E5, measured) |
| parse, visible panes | 1–2 | 3.2 % of pane time, ~68 MiB/s (E5) |
| structural scan, hidden panes | shared | 2.0 % of pane time (E5) |
| renderer | 1 | GPU-bound |
| indexer | GPU, or deferred | 0 on the CPU when offloaded (E1) |

**Predicted ≈3 of 16 logical cores at full target load; E5 measured ≈1.3.**
That is §8.7b's rule — *foldwave adds near-zero CPU to a machine the agents have
already taken* — now measured rather than derived: at the target rate the
composed pipeline leaves the machine ~92 % idle.

**7. Backpressure is a choice of which cache to let go stale.**

The shed order in §8.8 is not a list of tricks, it is this invariant applied.
Each step drops a *cache*, never the truth, in increasing order of what the user
notices: hidden-pane screens, then index construction, then background frame
rates, then flush cadence, then parse workers. Ingest and the focused pane's
input are never traded — the first is correctness, the second is the product.

**What this was, before E5.** The composition above was arithmetic over six
numbers measured in isolation; E5 (below) has since run it. Legs that do not interfere on paper routinely interfere in
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
| `vterm` full emulation (tokenize + apply to screen) | **208.2** | 100 % |
| `ansi::Parser::feed` tokenize, one heap token per token | 307.1 | 68 % |
| `ansi::Parser::feed_with` tokenize, borrowed spans | 858.6 | 24 % |
| The state machine alone ([`fsm.rs`](../crates/fwbench/src/fsm.rs) table) | 555.7 | 37 % |
| All 11 start states in lockstep (enumerative, no shortcut) | 389.9 | 53 % |
| Structural scan, hidden-pane path (lines + OSC 133) | 1,833.4 | 11 % |

*(Measured after `vterm` adopted spans. The convergence table below is the
earlier run of the same day; it is deterministic and unaffected.)*

- **Removing per-token allocation was worth 1.76× on full emulation, and it
  shipped.** `nativelite-ansi` 0.4.0 added `Parser::feed_with`: the same state
  machine handing a closure borrowed events (text runs point into the caller's
  buffer, sequence payloads into reused scratch), plus `Utf8Decoder` for
  consumers that want `str`. `nativelite-vterm` 0.5.3 consumes it.
  - Emulation **120.6 → 208–212 MiB/s**; on six P-cores 623.8 → 1,158.2; on
    all 16 logical CPUs 924.8 → **1,828.3**.
  - Tokenizing alone is 858.6 MiB/s with spans against 307.1 with owned
    tokens, and beats the bare table-driven machine (555.7) because a text run
    costs nothing per byte beyond its range check.
  - The owned `feed` got faster too (192.5 → 307.1): it is now a thin adapter
    over the same machine, and sequence scratch is reused rather than
    reallocated.
  - Neither crate's public API changed. `vterm`'s 60 behaviour tests and
    atrium's 812 tests pass untouched, so atrium inherits the speedup without
    a code change.
- **Screen apply is now the larger half: ≈76 % of emulation time** (208.2 of a
  possible 858.6 if tokenizing were free). The next parser-side win is
  therefore in `Screen`, not in tokenizing — or in not applying at all, which
  is what §8.0's hidden-pane path already does.
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
| Composed rate | read + log + parse + scan concurrently, 16 panes, flat out and paced at the target: where it breaks, and which leg breaks first |
| Interference | each leg's solo rate re-measured while the others run — how much of §4.1 survives composition |
| **One log stream or sixteen** | §8.4 specifies a log *per pane*, but the 479 MiB/s optimum was measured on **one** sequential stream with a single `fsync` cadence. Sixteen interleaved streams with sixteen cadences is unmeasured and may be far worse. Measure both; if a single device-level log with pane-tagged records wins, §8.4 changes, and per-pane line indexes and checkpoints stay separate files |
| Latency under load | key-echo p50/p99 and pane-switch time while all of the above runs, against the §4 targets |

**E5 result (measured 2026-09-22, quiet machine, release build, `fwbench
compose`).** 16 panes, 4 visible and 12 hidden, agent-like bytes pushed through
log → parse-or-scan concurrently, logs on E:, 1 MiB write-through writes. The
sources are pre-generated, so this measures the pipeline and not the generator
([`flood`](../crates/fwbench/src/flood.rs) covers the transport leg).

*Composed rate, flat out:*

| Log layout | 16 × 256 MiB (4 GiB) | 16 × 1 GiB (16 GiB) | 16 × 2 GiB (32 GiB) |
| --- | --- | --- | --- |
| none (control) | 2,859.1 MiB/s | — | — |
| one file per pane | 1,724.2 | 2,429.5 / 1,312.2 | 2,163.4 |
| one device-level log | 2,092.5 | 2,135.9 / 1,431.7 | 1,464.6 |

- **The composed pipeline clears the §4 target by 5–9×.** Every run with a
  durable log landed between 1,312 and 2,429 MiB/s against 273 MiB/s. §8.0's
  arithmetic said ingest would be the bound; composition did not break it.
- **Logging is what the pipeline spends its time on** — 43–80 % of pane time
  with a log, against 72 % on parsing and 28 % on scanning with logging off.
- **The legs survive composition.** With logging off, a visible pane parsed at
  179.7–187.9 MiB/s while 15 other panes ran, against 208–214 MiB/s solo
  (E4): **≈87 % survives**. Hidden panes scanned at 1,284–1,519 MiB/s each
  against 1,833 solo (≈75 %).

*At the §4 target rate (paced to 273 MiB/s aggregate, 16 × 256 MiB):*

| Log layout | achieved | log write p50 / p99 | pane time working |
| --- | --- | --- | --- |
| one file per pane | 272.7 MiB/s | 0.93 ms / 5.52 ms | **8.1 %** |
| one device-level log | 272.7 MiB/s | 1.15 ms / 11.15 ms | 7.0 % |

- **At the target the machine is ~92 % idle.** The whole composed load — 16
  panes logging durably, 4 parsed, 12 scanned — costs **≈1.3 of 16 logical
  cores**, against §8.0's predicted ≈3. §8.0's budget was pessimistic by
  about 2×.

*One log or sixteen (§8.4's open question):* **no decisive winner, and drive
state matters more than layout.** Running both twice in alternating order, the
first run of a pair beat the second whichever layout it used (per-pane 2,429
then single 2,136 with the SLC cache fresh; single 1,432 then per-pane 1,312
once it was exhausted). The spread between layouts, ≤ 21 %, is inside the
spread the drive's own cache state produces, so this experiment cannot
separate them on throughput.

- **Per-pane logs do win on write latency at the target rate** (p99 5.52 ms
  against 11.15 ms), which is the figure the §4 latency target cares about,
  because a device-level log serializes every pane's append through one
  handle.
- **Decision: keep §8.4's log per pane.** It is not measurably slower, it has
  the better tail, and it keeps retention, per-pane indexes and checkpoints
  simple. Revisit only with a protocol that preconditions the drive between
  runs.
- The device-level variant carries one [`SECTOR`] header per write to keep
  payloads aligned (0.4 % at 1 MiB writes); that overhead is not what decides
  the comparison.

**Latency under the composed load (measured 2026-09-22, `fwbench compose
--echo`).** E5's last row. Key echo through `cmd` (ConPTY, 25 ms between
keys) was measured idle, then again while the full pipeline ran in the same
process: 16 panes logging to E:, 4 parsed, 12 scanned.

| Load | Keys | Idle p50 / p99 | Under load p50 / p99 | Added p50 / p99 |
| --- | --- | --- | --- | --- |
| paced at the 273 MiB/s target | 1,000 | 0.13 / 4.37 ms | **0.08 / 0.68 ms** | −0.05 / −3.68 ms |
| flat out, 3,405 MiB/s (12.5× target) | 600 | 0.15 / 0.88 ms | **0.10 / 0.90 ms** | −0.06 / +0.03 ms |
| paced at target, four 200-key runs | 200 each | p99 0.71–4.40 ms | p99 0.60–0.97 ms | p99 −3.75 to +0.26 ms |
| flat out, two 200-key runs | 200 each | p99 0.69–1.61 ms | p99 1.09–6.20 ms | p99 +0.40 / +4.59 ms |

- **The composed pipeline does not touch input latency.** At the target rate
  a key echoes in 0.08 ms at the median and 0.68 ms at p99 — against §4's
  limits of 1 ms and 5 ms *over* the shell's own latency — and even running
  flat out at 12.5× the target the added p99 is 0.03 ms. Every run, all ten,
  met both limits. Not one key was lost.
- **p99 needs more than 200 samples.** At 200 keys p99 is the second-worst
  keystroke, and the idle baseline alone swung 0.69–4.40 ms between runs. One
  200-key flat-out run showed +4.59 ms; the 600-key run under heavier load
  showed +0.03 ms. The 600- and 1,000-key rows are the result; the 200-key
  rows show the spread.
- **The idle tail is often worse than the loaded one** (4.37 ms idle vs 0.68
  loaded at 1,000 keys). The likely cause is CPU power states — an idle core
  sleeps deeper and wakes slower, a loaded one stays awake — but that is
  **unverified**. If it holds, foldwave's worst input latency is on a quiet
  machine, not a busy one.
- **What this is not:** foldwave's own input path, which does not exist yet.
  It is a real shell behind ConPTY, echoing while the pipeline competes for
  the same machine and the same process. Pane-switch time is E6's rebuild
  figure (15.2–15.6 ms from 3.3 MiB).

**E6 result: crash and replay (measured 2026-09-22, `fwbench replay`).** §4
promises that after a crash "every pane's history up to the last flush
replays"; nothing had tested it. The harness spawns a writer that appends a
*seeded* stream to a log on E: (unbuffered write-through, §8.4's pattern),
kills it with `TerminateProcess` — no unwinding, no flush, no destructors —
then checks the file byte for byte against the stream that produced it and
rebuilds a screen from it.

| Kills | Appends | Durable after the kill | Log an exact prefix? | Torn tail |
| --- | --- | --- | --- | --- |
| 9, from 20 ms to 1.5 s of writing | 256 KiB and 1 MiB | 44 MiB – 4.8 GiB | **yes, every time** | **none, every time** |

- **Write-through delivers what it promises.** A killed writer leaves whole
  appends or nothing: no partial record, no reordering, no corruption at the
  tail. That is the property the log's crash story rests on, and it is now
  measured rather than assumed.
- **Replaying the log runs at 186–220 MiB/s**, the `vterm` emulation rate, as
  expected: replay *is* parsing.
- **A pane rebuild from 3.3 MiB of log takes 15.2–15.6 ms**, inside a 60 Hz
  frame (16.7 ms) but with only ~9 % to spare. §8.0's checkpoint formula is
  confirmed (3.3 MiB / 15.2 ms = 217 MiB/s, the measured parse rate), but the
  interval it produces has no margin. **Proposal: checkpoint every ≈2 MiB**
  (≈10 ms rebuild, 40 % margin) until the governor tunes it from measured
  rebuild latency.
- **What this does not test: power loss.** `TerminateProcess` kills the
  process, not the drive. Write-through with no buffering asks the device to
  persist each write before returning, but the 990 PRO is a consumer drive
  with no power-loss protection, so a real power cut could still lose what sat
  in its volatile cache. Testing that needs hardware we do not have.
- **Still untested:** a *concurrent* crash — 16 panes writing when the kill
  lands — and replay of a device-level log, which would have to demultiplex
  pane-tagged records. Both matter only if §8.4's per-pane decision is
  revisited.

**What E5 settles.** §8.0 is no longer a proposal with arithmetic behind it:
the legs compose, the pipeline runs 5–9× the target flat out, and at the target
it leaves ~92 % of the machine idle. Two things it does not cover: key-echo
latency and pane-switch time under this load (the `combined` mode measures echo
against a flood, but not against the full ingest pipeline), and a log that
survives a crash — E5 writes durably but never reads back or replays.

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
Per pane, an append-only log, in segments. **One log per pane was decided by
E5 (§8.0) on 2026-09-22.** Sixteen panes logging at
once measured within 21 % of a single device-level log either way, a spread
smaller than the drive's own cache state produces between runs, so throughput
does not separate them. Per-pane logs win the tail that matters — at the §4
target rate, write p99 5.52 ms against 11.15 ms, because a device-level log
serializes every pane's append through one handle — and they keep retention,
per-pane indexes and checkpoints simple.


| Part | Content |
| --- | --- |
| data segments | raw pty bytes as received, fixed-size segments (e.g. 64 MiB) |
| line index | byte offset and timestamp of every 64th line start (sparse — see below), for jump-to-line in one lookup plus a scan of at most 63 lines |
| checkpoints | periodic serialized screen + parser state, so rebuilding a view replays at most one interval |
| marks | semantic boundaries: prompt, command, output, exit code, agent events (§8.7) |

**`panelog` on-disk format (proposal, 2026-09-22, M1).** Nothing in §11
fixes these details; the numbers are what M1 builds and then measures.

- **Why the line index is sparse.** An offset and timestamp for *every* line
  (16 bytes) does not fit the §4 history target: agent output averages about
  40 bytes a line, so 100 GB is ~2.5 billion lines and a ~40 GB index. One
  entry per 64 lines is ~625 MB for 100 GB. Jump-to-line becomes one index
  read plus a forward scan of at most 63 lines (a few KiB) — microseconds,
  against a 1 s target. 64 is a parameter; the governor may change it.
- **Records, never rewritten.** Each flush appends one *record* to the
  current segment: a 64-byte header (magic, pane id, record sequence number,
  logical start offset, payload length, CRC-32C of the payload, CRC-32C of the
  header), then the payload, padded to a 4 KiB sector boundary so every write
  stays unbuffered write-through (§8.4 above, E6). Nothing is ever overwritten,
  so a crash can only lose the record being written, never an earlier one.
  - Padding costs up to 4 KiB a flush: 0.4 % for a full 1 MiB record, and
    about 80 KiB/s for a pane flushing a trickle every 50 ms.
  - CRC-32C is our own ~30 lines (std has none), checked on recovery.
- **Segments of 64 MiB of payload**, one file each, plus two small side files
  per segment: a *record map* (16 bytes per record: logical start → file
  offset) and the *sparse line index*. Both are rebuildable from the segment
  alone, so they are written buffered, not durably.
- **Recovery** opens the maps, then scans forward from the last mapped record
  through the segment tail, accepting records whose magic, sequence number and
  both CRCs check out, and stops at the first that does not. Everything before
  it is the durable log; nothing after it is trusted. A missing or short map
  or index is rebuilt by the same scan.
- **Checkpoints, marks, search and compression are later M1 slices**; the
  first slice is append, flush, crash-safe recovery, jump-to-line and range
  reads, with the §4 history target measured headless on E:.

**`panelog`, first M1 slice (built and measured 2026-09-22).**
[`nativelite/panelog-rs`](https://github.com/nativelite/panelog-rs) (private,
MIT, std only, no `unsafe`): records of a 64-byte header (CRC-32C of header and
payload) plus payload, padded to 4 KiB and written unbuffered write-through,
never rewritten; 64 MiB segments; per-segment record maps and a **sparse line
index** (one entry per 64 lines) as rebuildable caches. Recovery keeps exactly
the records that validate, cuts a torn tail and rebuilds maps and index;
`Reader::verify` re-checks every payload for corruption at rest.

- **Why sparse:** an offset and timestamp for every line is ~40 GB of index for
  100 GB of agent output (~40–50 bytes a line); one entry per 64 lines is
  ~625 MB. A jump is one index read plus a scan of fewer than 64 lines.
- **§4 history target, measured on E: (`fwbench history`, quiet machine):**

  | Log size | Open | Jump to a random line p50 / p99 / max | Write | On-disk overhead |
  | --- | --- | --- | --- | --- |
  | 16 GiB, 338 M lines, 1,000 jumps | 40.0 ms | 0.085 / 0.172 / 0.832 ms | 777 MiB/s | 0.51 % |
  | **100 GB (93.1 GiB), 1.97 G lines, 2,000 jumps** | **277.6 ms** | **0.112 / 0.238 / 1.377 ms** | 675 MiB/s | 0.51 % |

  Open and jump-to-any-line are met with more than three orders of magnitude to
  spare. Search, the third part of the target, is a later slice.
- **Write rate is CPU-bound on one thread** (CRC-32C, newline scan, copy into
  the staging record) at 675–777 MiB/s, below the drive's ≈1,800 MiB/s. That is
  2.5× the per-machine ingest target already, and one pane never needs it;
  hardware CRC and a SWAR newline scan are the obvious levers if it ever
  matters.
- **Checkpoints (built and measured 2026-09-22).** `vterm` gained
  `Term::checkpoint` / `Term::restore` (the whole emulator state; taken only
  between sequences, so the tokenizer never needs saving; 24x80 ≈ 20 KB, save
  23 µs, restore 63 µs), and `panelog` stores them beside the log
  (`Writer::checkpoint`, `Reader::checkpoint_before`). A pane is rebuilt by
  restoring the nearest checkpoint and replaying the log after it.
  `fwbench history --checkpoint-mib N`, 8 GiB of agent output on E:, 1,000
  rebuilds at random lines of a 24x80 pane:

  | Checkpoint every | Rebuild p50 / p90 / p99 | Worst | Disk overhead |
  | --- | --- | --- | --- |
  | 2 MiB | 5.59 / 9.90 / 13.98 ms | 20.54 ms | 1.08 % |
  | **1 MiB** | **3.09 / 5.04 / 6.94 ms** | **9.11 ms** | 1.65 % |

  **Proposal: checkpoint every 1 MiB of pane output.** Every one of 1,000
  rebuilds then fit inside one 60 Hz frame (16.7 ms); at 2 MiB the tail
  overshoots. The difference costs 0.6 % more disk. This supersedes §8.0's
  arithmetic figure (3.3 MiB) and E6's ~2 MiB proposal: measured end to end,
  the tail needs the smaller interval.
- **Search (built and measured 2026-09-22).** Each 1 MiB block of log carries
  a 2^16-bit trigram signature (0.78 % of the data); a query reads only blocks
  whose signature, with the next block's, holds every trigram of the needle,
  and scans those on all cores, newest first, up to a limit. Tested against a
  brute-force scan, including a match straddling a block boundary.
  `fwbench history`, 100 GB (93.1 GiB, 1.97 G lines) on E:, eight marker
  lines planted through the log:

  | Query | Matches | Time (repeat) |
  | --- | --- | --- |
  | rare: one planted marker | 1 | **400 ms** (422) |
  | absent: must consider every block | 0 | **498 ms** (395) |
  | common word, first 1,000 | 1,000 | 236 ms (118) |
  | common word, any case, first 100 | 100 | 115 ms (113) |

  **The §4 history target is met at 100 GB: open 299–433 ms, jump to any line
  0.1–0.3 ms, search ≤ 0.5 s.** Costs: 1.29 % disk overhead in total, and the
  write rate falls to 363–388 MiB/s because signatures are built at ingest —
  still more than 20× what one pane needs. Limits, stated in the API: matching
  is on raw bytes (a colour change mid-word hides that match), and needles
  under three bytes scan everything.
- **Marks (built and measured 2026-09-22).** `ansi::MarkScanner` finds OSC
  133 / 633 shell marks (prompt `A`, command `B`, output `C`, end `D` with its
  exit code) in the raw stream with no parser or screen, across any chunk
  split. `panelog` stores each as a 32-byte CRC'd entry in `marks.bin`, sorted
  by offset (`Writer::mark`), and finds them by binary search with positioned
  reads (`Reader::marks_in`, `Reader::last_mark_before`), so millions of marks
  are never loaded. Like the other side files, a damaged tail is dropped on
  open. `fwbench history`, 16 GiB on E:, a shell command cycle every 256 KiB
  of output, one command in 50 failing:

  | Query | Result |
  | --- | --- |
  | marks stored (65,536 commands) | 262,143 |
  | find the last failed command | **0.176 ms**, the planted offset |
  | list the marks in the last GiB | 16,384 marks in 25.5 ms |

  Scanning on the writer thread costs ≈12 % of the write rate with
  checkpoints off (386 → 339 MiB/s, 4 GiB A/B) and ≈5 % with 1 MiB
  checkpoints on (131 → 125 MiB/s, where feeding `vterm` dominates). Without
  checkpoints the writer stays above the 273 MiB/s per-machine target; with
  them it does not on one thread, so checkpointing has to run off the writer
  thread, as the §8.0 budget already gives every stage its own threads. The
  16 GiB run was on a noisy machine (0.37 cores busy elsewhere). The lookups are far from any limit; the write-rate costs
  are A/B on the same machine and state.
- Still to come in M1: compression of cold segments. This is a proposal, not
  something §4 needs, since the history target is met uncompressed.

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
  | 16 MiB | 479 MiB/s | 26.7 ms | 31.4 ms | 47.3 ms |
  | 64 MiB | 445 MiB/s | 115 ms | 153 ms | 162 ms |
  | 256 MiB | 126 MiB/s | 417 ms | 12,760 ms | 12,760 ms |

  **This whole table is superseded** (2026-09-21): it prices a
  buffered-plus-`fsync` loop, which is not how the log writer works. Unbuffered
  write-through appends need no flush cadence at all and sustain ≈1,800 MiB/s
  with a write p99 of ≈1.1 ms (§4.1). It is kept because the shape is still
  instructive — both directions off a flush optimum are bad, and the slow
  direction is catastrophic (a 256 MiB cadence stalls 12.7 s at the tail, a
  visibly frozen pane).
  - **The log writer uses unbuffered write-through, not a flush cadence.** Each
    append is durable when it returns, so there is no window to tune and no
    `fsync` stall to hide.
  - **Write latency still must never block a pane.** At the target rate E5
    measured a log-write p50 of 0.93 ms and p99 of 5.52 ms per pane — well
    under the flush figures above, but still far above the input-latency
    target, so the log writer stays asynchronous to the reader and "the
    history up to the last flush replays" (§4) is what that buys.
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

**M3 renderer spike (built and measured 2026-09-22).** `crates/fwrender`
(MIT, std only):
- **Frame:** turns a `vterm` screen into 16-byte cell instances and marks only
  the rows that changed.
- **Atlas:** a glyph atlas of 8-bit coverage, rasterized once per glyph.
- **CPU renderer:** redraws damaged rows only. It is both the fallback and the
  reference.
- **Vulkan backend:** hand-bound. `vulkan-1.dll` is loaded at run time, and the
  shaders are SPIR-V generated by our own code, with no compiler and no
  validation layer on the machine. One instanced draw covers the grid; only
  damaged rows and new glyphs are uploaded.
- **Win32 window.**
- **Our own glyph rasterizer (2026-09-22):** the new package
  [`nativelite/glyphs-rs`](https://github.com/nativelite/glyphs-rs) (private,
  MIT, std only) reads TrueType fonts and `.ttc` collections, and rasterizes
  outlines with an exact-area coverage rasterizer, written from our own
  derivation of the technique stb_truetype and font-rs use. It covers `cmap`
  formats 4 and 12, simple and composite glyphs, and `OS/2`/`hhea` metrics.
  `fwrender::FontRasterizer` draws each glyph straight into its cell at the
  exact pen position. It has regular, bold, italic and bold italic faces, and
  falls back to MS Gothic, Malgun Gothic and Segoe UI Symbol for characters
  Consolas lacks (CJK is centred across its two cells). It is now the default;
  GDI stays behind `fwbench render --gdi`.
  - **Against GDI** (`fwbench glyphs`, Consolas 16 px, printable ASCII regular
    and bold, 20 rounds, n = 3,760 each): the same 9x19 cell. Ours takes 2.3–2.5
    µs per glyph at p50 against GDI's 4.3; at p99, 8.4–8.8 µs against GDI's
    6.2–22 µs over two noisy runs. Each glyph is rasterized once into the
    atlas, so this is not a hot path.
  - **Coverage differs from GDI by 20.4 of 255** per pixel on average. The
    first run showed 55.7: Consolas's `hhea` puts a line gap after the
    ascender, which placed our baseline 3 px above GDI's. Cells now follow
    Windows' `OS/2` metrics, as GDI does. What remains is **hinting**: GDI
    snaps Consolas's stems and cap height to whole pixels, and we draw the
    true outline, so small text is softer.
  - **Sharpening without hinting (2026-09-22), a switch:**
    `fwrender::Sharpening`, on by default, and `Sharpening::OFF` (or
    `fwbench render --no-sharpen`) for the raw outline. There are three
    cheap steps:
    - **vertical fit** (`glyphs::VerticalFit`): baseline, x-height and cap
      height land on whole pixel rows;
    - **stem align:** each glyph moves by under half a pixel so its left edge
      sits on a pixel column;
    - **stem snap:** the glyph is drawn again, shifted right by what the
      font's stem (the width of `|`) lacks of a whole pixel, so vertical
      strokes become whole pixels wide with both edges on columns.

    A gamma curve on coverage was also measured. `fwbench glyphs`, Consolas
    16 px, printable ASCII regular and bold, 20 rounds, n = 3,760 each:

    | Glyphs | Per glyph p50 / p99 | Grey share of ink (lower is crisper) | Mean difference from GDI |
    | --- | --- | --- | --- |
    | ours, raw (`OFF`) | 2.1 / 7.8 µs | 70.7 % | 20.4 |
    | + vertical fit | 2.2 / 8.1 µs | 68.5 % | 20.5 |
    | + stem align | 2.2 / 8.0 µs | 66.0 % | 19.1 |
    | + gamma 0.65 (no snap) | 2.2 / 8.1 µs | 63.8 % | 18.0 |
    | **+ stem snap (the default)** | **4.1 / 15.9 µs** | **39.6 %** | 19.9 |
    | GDI, hinted | 4.3 / 5.0 µs | 32.1 % | 0 |

    Stem width is what made unhinted text look soft. At 16 px Consolas's
    stems are ~1.2 px, one solid column plus a grey one, and GDI's hinting
    rounds them to 2 px. Snapping them cuts the grey share almost to GDI's,
    for one extra outline pass: the same median cost per glyph as GDI, paid
    once per glyph since the atlas keeps it. Side by side
    ([full sheet](img/m3-glyphs-compare.png): raw, the steps as in the table,
    GDI last; [4x zoom](img/m3-glyphs-zoom.png): raw, default, GDI), the
    default looks as clean as GDI and slightly heavier where three stems
    share the 9 px `m`. The remaining difference from GDI is weight and
    placement, not blur. Full TrueType hinting (running the fonts' own
    programs) is not needed on this evidence.
  - **Not yet:** CFF/OTF fonts, colour emoji, and looking fonts up by family
    name (Consolas's files are named directly for now).

**Correctness:** the Vulkan output equals the CPU reference **pixel for pixel**
(largest channel difference 0) on the RTX 3070. The test covers colours,
reverse, bold, dim, underline, strike, wide glyphs, the cursor, margins, a
one-row update and a resize.

`fwbench render`:
- **Setup:** one pane of agent output fed into `vterm` on its own thread, in a
  real window. Latency is measured from bytes arriving at the emulator to the
  frame that shows them being presented; the display's scan-out comes on top.
- **Warm-up:** device and swapchain creation and one warm-up frame happen
  before the clock.
- **Pacing:** frames are capped at the display's 164 Hz. Vulkan's FIFO did not
  pace to the display on this machine, so we pace frames ourselves, with a
  high-resolution waitable timer; the default ~15.6 ms timer tick had held a
  164 fps cap to 65 fps.
- **Runs:** 8 s each, on a noisy machine (0.45–1.1 cores busy elsewhere).
  Compare the backends to each other, not to absolute budgets.

| Window, output | Backend | fps | Render + present p50 (p99) | Output to present p50 / p90 / p99 |
| --- | --- | --- | --- | --- |
| 1600x900 (177x47), 17 MiB/s | CPU | 121 | 1.64 ms (3.4) | 5.81 / 9.05 / 10.70 ms |
| 1600x900, 17 MiB/s | **Vulkan, Fifo** | 151 | **0.11 ms** (0.33) | **3.56 / 6.22 / 6.93 ms** |
| 1600x900, flat out | CPU | 117 | 1.75 ms (4.2) | 6.25 / 9.80 / 12.28 ms |
| 1600x900, flat out | **Vulkan, Mailbox** | 150 | **0.19 ms** (0.50) | **3.33 / 6.31 / 6.93 ms** |
| 2560x1400 (284x73), flat out | CPU | 92 | 4.0 ms (8.2) | 10.21 / 14.51 / 18.32 ms |
| 2560x1400, flat out | **Vulkan, Mailbox** | 148 | **0.21 ms** (0.45) | **3.94 / 6.46 / 7.16 ms** |

In every run, a 3 s idle phase drew **0 frames**. The process's idle CPU read
0–1.6 % of a core, an upper bound: it includes the process snapshots taken to
measure it.

- **Throughput is not the renderer's.** Flat out, the pane consumed
  163–178 MiB/s at 1600x900 and 136–138 MiB/s at 2560x1400, the same on
  either backend. That is `vterm`'s single-core rate (§4.1), which falls as
  the grid widens. §8.6's caution holds: the GPU does not change throughput.
- **What the GPU buys is headroom and a flat frame cost.** Per frame, Vulkan
  costs ~0.1–0.2 ms of CPU at any window size. The CPU renderer costs 1.6 ms
  at 1600x900 and 4 ms at 2560x1400, which is 27–66 % of a core at the
  display's rate. The CPU renderer also cannot hold 164 fps at the larger size.
  Drawing on every change without a cap, Vulkan reached 2,500–5,300 fps and the
  CPU renderer ~650 fps at 1600x900.
- **Latency:** once a frame is paced, output reaches the screen in 3.3–3.9 ms
  p50 and ≤ 7.2 ms p99 on Vulkan. That is about half a 164 Hz frame of waiting
  plus under 0.5 ms of work.
- **Found on the way: a `vterm` scroll region costs 8.6×.** 64 MiB at 177x47
  ran at 182 MiB/s with the full screen scrolling and at 21 MiB/s with
  `DECSTBM` set to all but the last row: the scroll moves every cell of the
  region instead of rotating the ring. TUIs with a status line use scroll
  regions. **Fixed 2026-09-22** (ansi-rs 926580e): `Screen` now reaches rows
  through a row map, so any region scrolls by rotating row indices; the same
  probe runs 176–177 MiB/s with the region and 173–177 without.
- **Result for decision 4:** Vulkan on Windows did not fall short, so the spike
  gave no reason for D3D11. **The founder confirmed Vulkan first on
  2026-09-22 (§11).**
- **Against other terminals (measured 2026-09-22).** Two new tools:
  - `crates/fwterm`: a minimal foldwave terminal. It runs one real shell
    through ConPTY, with `vterm`, our glyphs and Vulkan, and handles keys,
    resize and query replies. It has no scrollback or configuration yet.
  - `crates/fwprobe`: measures any terminal from the outside, the same way
    for all.
    - **Latency:** it launches the terminal running `cmd`, sizes and raises
      the window, types a character with `SendInput`, and reads the echo
      region with `BitBlt` until the pixels change (the Typometer method). The
      prompt forces a steady cursor, keystrokes are 60–140 ms apart, and each
      terminal gets 200 samples.
    - **Throughput:** the terminal runs `fwprobe emit`, which writes 32 MiB of
      agent-like output with raw `WriteFile` and times it from the inside.
      Three runs each.

  Every terminal ran in its default configuration, at a 1200x760 window on
  the reference machine. It was **not a quiet machine** (other Claude
  sessions, a browser).

  **Key press to pixels on screen** (includes the console host, the terminal
  and the desktop compositor):

  | Terminal | Console host | p50 / p90 / p99 | min / max | n |
  | --- | --- | --- | --- | --- |
  | **fwterm** (Vulkan) | built-in conhost | **6.90 / 11.60 / 13.46 ms** | 1.59 / 14.54 | 200 |
  | fwterm (Vulkan) | OpenConsole 1.24 (Microsoft's ConPTY package) | 7.87 / 12.24 / 15.13 ms | 1.96 / 15.47 | 200 |
  | Alacritty 0.17.0 | built-in conhost | 7.22 / 12.40 / 14.27 ms | 1.66 / 16.08 | 200 |
  | Windows Terminal 1.24 | its OpenConsole | 8.48 / 12.03 / 13.77 ms | 2.33 / 14.24 | 200 |
  | fwterm (Vulkan) | WezTerm's bundled OpenConsole (2024) | 21.60 / 27.81 / 32.47 ms | 11.42 / 34.64 | 200 |
  | WezTerm 20240203 | its bundled OpenConsole (2024) | 39.19 / 44.67 / 48.62 ms | 26.24 / 74.61 | 200 |

  - **The instrument resolves ~6.1 ms, not better.** A GDI read of the screen
    waits for the compositor's next frame (165 Hz → 6.06 ms; measured read cost
    6.04–6.12 ms in every run), so every sample is quantized to compositor
    frames.
  - **fwterm, Alacritty and Windows Terminal are therefore tied** at about one
    to two compositor frames. fwterm had the lowest p50 and p99, but by less
    than one frame. This supports "as fast as the fastest", not "faster".
  - **The console host dominates.** WezTerm's bundled 2024 OpenConsole adds
    ~15 ms even under fwterm, and WezTerm itself is slowest.
  - **What the probe cannot see:** our in-process measure — output arrival
    to present, p99 ≤ 7.2 ms (above) — is below this instrument's resolution.
    To separate terminals by less than a frame needs a photodiode, or the
    compositor's own frame statistics.

  **Output throughput** (32 MiB, 3 runs, median):

  | Terminal | Console host | MiB/s (runs) |
  | --- | --- | --- |
  | Windows Terminal 1.24 | its OpenConsole | **115.2** (91.8, 115.2, 119.5) |
  | fwterm | OpenConsole 1.24 | 82.9 (82.2, 82.9, 84.0) |
  | fwterm | built-in conhost | 14.9 (12.9, 14.9, 14.9) |
  | WezTerm 20240203 | its bundled OpenConsole (2024) | 12.6 (12.6, 12.6, 12.6) |
  | Alacritty 0.17.0 | built-in conhost | did not finish: 1 MiB at 17.6 MiB/s, but 8 and 32 MiB stalled past 120–200 s; cause not investigated |

  - **The console host sets the ceiling.** fwterm moved 14.9 MiB/s through
    conhost, and 15.9 with `vterm` switched off entirely (a one-off
    experiment), so the host is the limit there, not our code. That matches
    M0: 139 MiB/s across 16 conhost panes is ~8.7 per pane.
  - **On the same modern host, fwterm reaches 83 MiB/s against Windows
    Terminal's 115.** The remaining gap is ours to find. Candidates: the
    reader hands 64 KiB chunks to `vterm` under a lock the renderer also
    takes; the pane lock contention seen in `fwbench render`; and how fast
    we drain the pipe. Not yet profiled.
  - **This makes the §11 row 6 decision concrete:** running on the built-in
    host costs fwterm ~5.5× in throughput. Decided 2026-09-23: ship
    Microsoft's ConPTY package, and use pipes for agents.
- **The pipe lane (built and measured 2026-09-23).** `fwterm --pipe` starts
  the program with no console (`CREATE_NO_WINDOW`). Its stdout and stderr
  reach the emulator through plain pipes, with bare LF turned into CR LF as a
  tty would. Same `fwprobe throughput`, 3 runs each:

  | fwterm lane | MiB/s (runs) |
  | --- | --- |
  | **pipe, 256 MiB** | **173.1** (172.2, 173.1, 173.3) |
  | pipe, 32 MiB | 169.8 (159.4, 169.8, 170.5) |
  | pipe, emulation switched off (one-off experiment, 1 GiB) | 1,249 (1,238, 1,249, 1,258) |
  | console, OpenConsole 1.24 | 82.9 |
  | console, built-in conhost | 14.9 |

  - **The pipe lane is 1.5× Windows Terminal (115.2) and 11.6× fwterm on
    conhost** for the same output.
  - **The limit is now our emulator, not the transport.** One pane's full
    emulation (a full-screen scroll per line) runs at ~173 MiB/s on one core,
    and without it the same path moves 1,249 MiB/s. 173 MiB/s is already ~10×
    the §4 per-pane need (17.1 MiB/s); per-pane emulation speed is the next
    lever for single floods, alongside §8.0's cheaper path for panes nobody
    is looking at.
  - **Other terminals have no pipe lane:** they run every program under a
    console. This is foldwave's structural advantage for agent output, not a
    tuning win.
- **The emulator gap, profiled and narrowed (2026-09-23).** Stage by stage on
  64 MiB of agent output through `vterm` at 177x47 (1.32 M lines of ~52 bytes;
  `profile_vterm_stages` in `fwbench`, reproducible):

  | Stage | Before | After |
  | --- | --- | --- |
  | Parser (`feed_with`) | 79 ms | 84 ms (unchanged; noise) |
  | + UTF-8 decode | +44 ms | +26 ms: std's validator takes the longest valid prefix in one call |
  | Clearing the row each scroll brings in | **~165 ms** | **~7 ms**: rows clear lazily (written length + blank per row) |
  | Text into cells, the rest | ~60 ms | ~80 → less with `Screen::write_ascii` (no scratch cells) |
  | **`Term::feed` total** | **347 ms, 188 MiB/s** | **198 ms, 330 MiB/s (1.75×)** |

  End to end in fwterm (`fwprobe throughput`, 3 runs): pipe lane
  173.1 → 254.6 MiB/s; console lane on OpenConsole 1.24 82.9 → 102.5 MiB/s,
  against Windows Terminal's 115.2 on the same host.

  **Second pass, the same day:**
  - **The pipe lane's CR-LF copy was its whole overhead.** It is replaced by
    `vterm`'s new line mode (LNM, `CSI 20 h`): a line feed also returns to
    column 0, so no byte is rewritten. Pipe lane 254.6 → 325.8 MiB/s, level
    with `vterm` alone.
  - **The parser skips plain text a word at a time** (a SWAR test for C0 and
    DEL over 8 bytes): `feed_with` 777 → 2,168 MiB/s.
  - **`vterm`'s ASCII check on text runs** is now std's word-wide `is_ascii`,
    since the parser never passes controls in text.

  | | Start of the day | Now |
  | --- | --- | --- |
  | `vterm` `Term::feed`, 64 MiB at 177x47 | 188 MiB/s | **570 MiB/s (3.0×)** |
  | fwterm pipe lane, 256 MiB | 173.1 MiB/s | **543.0 MiB/s** (526.6, 543.0, 551.2) |
  | fwterm console lane, OpenConsole 1.24, 32 MiB | 82.9 MiB/s | **120.4 MiB/s** (120.0, 120.4, 121.2) |
  | Windows Terminal 1.24, 32 MiB (re-measured with the above) | 115.2 MiB/s | 118.1 MiB/s (116.0, 118.1, 119.6) |

  - **The console lane now matches Windows Terminal** on the same console
    host (120.4 against 118.1, within run-to-run noise), so that lane is
    host-bound again.
  - **The pipe lane is 4.6× Windows Terminal.** That pane now spends ~115 ms
    per 64 MiB emulating: parser ~30, UTF-8 decode ~28, the rest in `vterm`
    and the screen.
- **Third pass: ASCII skips the decoder, and 16 panes at once (2026-09-23).**
  - With no partial character held (`ansi::Utf8Decoder::is_idle`), plain
    ASCII text goes straight to the cells: `vterm` 570 → **631 MiB/s** (3.4×
    this morning's 188).
  - **One pane, pipe lane, 1 GiB:** 613.8 MiB/s on Vulkan (609.4, 613.8,
    617.0), 618.8 on the CPU backend. That is ~3 % below `vterm` alone, so the
    renderer's 165 Hz snapshot of the pane costs next to nothing, and the
    backend does not matter for throughput.
  - **Sixteen panes at once, pipe lane, measured end to end:** 16 fwterm
    windows on screen, each fed 1 GiB of agent output by its own emitter,
    with full emulation and drawing on every pane. Launched together, their
    emits overlap: finishing times within 0.15–0.16 s of each other.

    | Run | Combined (sum of concurrent panes) | Per pane | Wall clock, window launch included |
    | --- | --- | --- | --- |
    | 1 | 4,371 MiB/s | 270–279 MiB/s | ≥ 2,875 MiB/s |
    | 2 | 4,420 MiB/s | 273–285 MiB/s | ≥ 2,913 MiB/s |
    | 3 | 4,646 MiB/s | 284–297 MiB/s | ≥ 3,099 MiB/s |

    **Median 4.42 GB/s: 16× the §4 target of 273 MiB/s, and each pane ~16×
    its 17.1 MiB/s share**, on a noisy machine. The machine's 16 logical cores
    are shared by 16 emitters, 16 readers and 16 renderers, so this is the
    whole system under load, not a transport ceiling.
- **Frame rate and the console lane under the 16-pane load (2026-09-23).**
  `fwterm --stats` records every frame drawn while output flows.
  - **The display in use sets the pace.** The first 16-pane runs drew a steady
    ~31 fps per pane: the founder was connected through Remote Desktop, and
    the session's display ("Microsoft Remote Display Adapter") runs at 32 Hz,
    so fwterm correctly paced to it. It was not a bug. A normal-priority
    render thread also waited scheduler slices with every core busy (worst
    gap 115 ms); the UI thread now runs above normal
    (`fwrender::win::raise_thread_priority`), and the worst gap became 38 ms.
    The reader also steps aside for the renderer's lock, as `fwbench render`
    already did.
  - **Sixteen panes flooding at 4.22 GB/s combined, paced at 165 fps (the
    local display's rate, forced with `--max-fps 165`): every pane drew
    139.6–141.7 fps.** Frame gaps: median 7.0 ms, p99 8.6–10.3 ms, worst
    10.9–29.5 ms. **§4's ≥ 60 fps for visible panes is met 2.3×** under the
    full load. Measured through a remote session, so this is what the loop
    sustains, not what that session's 32 Hz display showed.
  - **Console lane, 16 panes at once, on Microsoft's OpenConsole 1.24**
    (`fwprobe throughput`, 256 MiB each):

    | 16 panes | Combined | Per pane | Emit times |
    | --- | --- | --- | --- |
    | **fwterm** | **842 MiB/s** (≥ 596 counting window launch) | 51.8–54.4 MiB/s | 4.70–4.94 s |
    | Windows Terminal 1.24 | 456 MiB/s (≥ 337) | 26.4–31.6 MiB/s | 8.11–9.69 s |

    **1.85× Windows Terminal on the same console host.** A shorter 64 MiB run
    gave 1,006 against 700. At one pane the two tie (120.4 against 118.1).
    Under load fwterm's cheaper emulation and rendering leave more of the
    machine to the console hosts. Both clear the §4 target (273 MiB/s).
  - **Latency while 16 panes flood: measured coarsely (2026-09-23, founder's
    call to run it through Remote Desktop).** `fwprobe latency`, 200 samples
    each, all four on the same 32 Hz remote display, so every screen read
    cost ~31 ms (one frame) and every sample is quantized to it:

    | Terminal | p50 / p90 / p99 | min / max |
    | --- | --- | --- |
    | fwterm, idle | 22.07 / 33.89 / 38.25 ms | 2.24 / 39.20 |
    | fwterm, 16 pipe-lane panes flooding | 23.08 / 33.34 / 37.12 ms | 4.49 / 37.25 |
    | Windows Terminal, idle | 23.82 / 36.12 / 42.60 ms | 1.91 / 42.65 |
    | Windows Terminal, same load | 23.24 / 33.59 / 36.73 ms | 4.48 / 37.34 |

    - **The load moved nothing by even one display frame.** fwterm under 16
      flooding panes is within 1 ms of idle, far inside this instrument's
      ~31 ms step; no sample was missed. fwterm and Windows Terminal are tied.
    - **This cannot confirm §4's p99 < 5 ms.** That needs the local 164 Hz
      display (~6 ms resolution) or a 240 fps camera on keyboard and screen
      (~4 ms, end to end including the display).
    - **Founder call (2026-09-23): tying or beating Windows Terminal on
      key-to-screen latency, with no degradation under load, is the bar for
      now.** A difference this small is not visible to a person typing. The
      p99 < 5 ms check on the local display or a 240 fps camera stays
      available, but it is not blocking.

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
      **The §4 p99 < 5 ms target is therefore not reachable through the
      built-in `conhost.exe`** — E2 (§11) found OpenConsole has no such tick and
      meets it (p99 0.31–0.43 ms under an 8-pane flood),
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
| `glyphs` | font loading and rasterization | **built as [`glyphs-rs`](https://github.com/nativelite/glyphs-rs)** (2026-09-22): our own TrueType parser and exact-area rasterizer, the technique of stb_truetype / font-rs re-derived, no code taken (§8.6) |
| `gpu-*` | per-platform draw backends (D3D11, Metal, Vulkan/GL) + CPU fallback | study Windows Terminal's AtlasEngine (MIT, D3D + D2D fallback) and Ghostty's Metal/OpenGL renderers (MIT) |
| `http-rs` (or reuse `web-server-rs` parts) | minimal HTTP client over std sockets, for a local model server | our own; std has none |
| `gpuscan` (spike) | optional GPU text scan: CUDA Driver API, Vulkan/Metal later, CPU fallback | spike only if the M0 harness shows unindexed scans matter |
| the app | window, input, panes, blocks, views, composition | new, reusing atrium's agent layer |

## 10. Milestones

| Milestone | Deliverable | Proof |
| --- | --- | --- |
| **M0 Harness** | throughput and latency benchmark: N panes of recorded agent output, timestamped key echo, frame timing, RSS, disk rate. vtebench (MIT/Apache) supplies workloads, but it measures only PTY read speed, not latency or frames, so it can't be the harness. Includes experiments E1, GPU contention (`compute.md`), E2, built-in console host vs OpenConsole (§11), and **E3, CPU attribution** (§8.7b) | a baseline table for atrium today and for 1–2 existing terminals; the E1 contention table on the RTX 3070; the E3 attribution table that says which stage is worth offloading |
| M0 status (2026-09-22) | **Mostly delivered.** `resmon` (four-resource attribution, E3) and `fwbench` (`flood`, `echo`, `combined`, `disk`, `read`, `parse` = E4, `compose` = E5, `gpu`) are in `crates/`, plus `fwcuda` (CUDA Driver API) and a noisy-run guard on every mode. Headline numbers on the quiet reference machine: transport **139 MiB/s over ConPTY** vs **1,854 over pipes**; log drive **≈1,800 MiB/s sustained** write-through, **2,745** cold read; full VT emulation **1,828 MiB/s** across 16 threads after the span tokenizer; the composed pipeline **1,312–2,429 MiB/s**, and **~92 % idle** at the 273 MiB/s target. Still to build: frame timing (needs M3) and **E1** (GPU contention). **E2 done 2026-09-22**: OpenConsole beats the built-in host on every measure; how to ship it is a founder decision. Measured since the last status: the crash/replay test (E6) and key echo against the composed pipeline | [`crates/fwbench`](../crates/fwbench), [`crates/resmon`](../crates/resmon), [`crates/fwcuda`](../crates/fwcuda) |
| **M1 Headless core** | `panelog` + fast parser + `vterm` checkpoints, driven by the harness with no UI | throughput and history targets met headless |
| M1 status (2026-09-22) | **Delivered, headless.** `panelog` (append, crash-safe recovery, sparse line index, checkpoints, trigram search, marks), `vterm` checkpoint/restore and `ansi::MarkScanner`, driven by `fwbench history`. §4 history target met at 100 GB: open 0.3–0.4 s, jump to any line 0.1–0.3 ms, search ≤ 0.5 s; pane rebuild p99 6.9 ms with 1 MiB checkpoints; last failed command found in 0.18 ms. Open: compression of cold segments (a proposal), and running checkpointing off the writer thread | [`panelog-rs`](https://github.com/nativelite/panelog-rs), [`crates/fwbench`](../crates/fwbench) |
| **M2 atrium adopts it** | unbounded history, search and crash replay inside atrium | atrium benchmarks don't regress; replay after a kill |
| **M3 Renderer spike** | one platform (Windows; Vulkan per §8.6, D3D11 only if it falls short), glyph atlas, damaged-row redraw, CPU fallback | frame time and latency under flood vs. baseline |
| M3 status (2026-09-22) | **Spike delivered on Windows with Vulkan, not D3D11** (§8.6 puts Vulkan first; this row said D3D11). `fwrender`: frame with row damage, glyph atlas, CPU reference renderer, a Vulkan backend that matches it pixel for pixel, and a Win32 window. `fwbench render` at 164 fps: output to present p99 ≤ 7.2 ms on Vulkan against 10.7–18.3 ms on CPU; Vulkan's frame cost is flat at ~0.2 ms up to 2560x1400; idle draws 0 frames. Our own glyph rasterizer (`glyphs-rs`) replaces GDI: the same cells, and with the default sharpening (vertical fit, stem align, stem snap; switchable off) about as crisp as GDI's hinting at the same cost per glyph. Measured against Windows Terminal, Alacritty and WezTerm from the outside (`fwprobe`): key-to-screen latency tied with the fastest within the instrument's one-frame resolution; throughput set by the console host, where fwterm reaches 83 MiB/s on OpenConsole 1.24 against Windows Terminal's 115. Open: the throughput gap on the same host, and the installed-OpenConsole launcher (§11 row 6). Decision 4 is confirmed as Vulkan, and the `vterm` scroll-region cost found here is fixed | [`crates/fwrender`](../crates/fwrender), §8.6 |
| **M4 App alpha** | window, panes, blocks, cross-pane search, governor | all §4 targets measured on the reference machine |
| M4 status (2026-09-23) | **Under way.** Route A bundled: `tools/fetch_conpty.py` fetches Microsoft's ConPTY package (pinned, SHA-256 checked) beside `fwterm.exe`, with `THIRD-PARTY-NOTICES.md`; each pane falls back to the system conhost if the bundled host cannot start (pty-rs 1fa80be). **Scrollback (hot tail):** `ansi::Screen::with_history` keeps lines scrolled off the top as a ring of the screen's own rows (no copy), `vterm` `Term::with_history`, `fwrender::Frame::update_view`; fwterm scrolls with the wheel and Shift+PgUp/PgDn, anchored on the text while output flows, typing returns to live (default 10,000 lines). Cost measured: pipe lane 605.5 MiB/s without history, 418.1 with 10,000 lines — cold rows in a ~35 MB ring; compact lines (§8.3) are the fix. **Deep history (2026-09-23):** `fwterm --log DIR` writes the pane's output to its own panelog on a logger thread (bounded 4 MiB queue: the reader waits for the disk rather than growing memory; flushed when idle and on close), with a `vterm` checkpoint per MiB taken under the pane lock so it lands at the right offset. Scrolling past the in-memory history opens the log and rebuilds the screen at that line (nearest checkpoint + replay; p99 6.9 ms measured in M1). Checked in a window: 3,000 lines with 100 in memory, wheel back ~1,000 and ~2,930 lines showed lines 1974–1994 and 54–74 from the log. Pipe lane with logging 359.0 MiB/s (358.7, 359.0, 361.4), paced by panelog's single-thread writer; 418 without. Opt-in: nothing bounds a log's size yet (retention is still to design), and C: is small. The hand-over between memory and log is approximate when lines wrap (a wrapped line is one log line, several screen rows). **Several panes (2026-09-23):** a window holds a tree of split panes over one cell grid, one-cell `│`/`─` dividers with joined corners, the lines around the focused pane in blue, all drawn into a single `fwrender::Frame` (`Frame::set_grid`, `update_view_at`, `put`), so neither backend changed. Windows Terminal's default keys: Alt+Shift+Plus/Minus/D split, Alt+Arrow or a click moves focus, Ctrl+Shift+W closes; a pane whose program exits closes, and the last one closes the window with its exit code; `--panes N` opens a near-square grid. Console-lane exit is now detected from the process (polled every 50 ms), since the console host keeps the output pipe open after its client exits; a closed pane's pseudoconsole is released (checked: its OpenConsole goes away). Measured over Remote Desktop on a noisy machine, so relative numbers only. One pane, pipe lane, 1 GiB via `fwprobe throughput`, alternating old/new builds: median 375.8 MiB/s before, 376.9 after, no regression. 16 panes, 1 GiB of `fwbench --emit` each, 2400x1300, `--max-fps 165`: one window against 16 separate windows of the same pane size, history off: 13.7 s output span at 132 fps against 15.6 s wall at 128–149 fps; history 10,000: 27.5–30.3 s at 90–109 fps against 32.6 s at 107–136 fps. Getting there took one fix: the UI thread took the 16 pane locks one after another, each waiting out that reader's chunk, and fell to 43–56 fps with history on; it now raises every due pane's `want` first so the waits overlap. Both layouts stall up to ~1.3 s now and then with 10,000-line history; that is the cold-memory cost compact lines address. Next: selection and copy, fonts by name, compact lines | [`crates/fwterm`](../crates/fwterm) |

M1–M2 pay off in atrium before any GPU work, which de-risks the expensive part.

## 11. Decisions

Founder answers of 2026-09-15.

| # | Question | Decision | Notes / still open |
| --- | --- | --- | --- |
| 1 | Name, product | **foldwave**, a new product | Anime-themed (Macross), picked from vetted shortlists; see Naming below. atrium continues and adopts the engine crates (M2) |
| 2 | App license | **FSL-1.1-Apache-2.0**, like atrium | Engine crates stay MIT |
| 3 | Reusing open source | **Inspiration over original code.** Copying is a rare exception | Now in [standards/zero-dependency.md](https://github.com/nativelite/ops/blob/main/standards/zero-dependency.md). Inspiration needs no notice; any copied code keeps its MIT/Apache notice (a legal requirement) |
| 4 | GPU binding | **Our own bindings, not wgpu.** **Founder decision (2026-09-22), after the M3 spike: Vulkan first**, CPU fallback, Metal later | The spike found Vulkan on Windows pixel-exact against the CPU reference, at ~0.2 ms of CPU per frame at any window size (§8.6), so there is no case for D3D11 or D3D12. One thing to watch: FIFO presentation did not pace to the 164 Hz display on the reference machine, and we pace frames ourselves. If pacing or remote desktop ever fall short, a DXGI flip-model present path is the Windows-native answer to test |
| 5 | Platforms | **Windows first**, groundwork for others | Vulkan and a pane-transport interface keep Linux close; macOS gets Metal later |
| 6 | Console host | **Decide by measurement (experiment E2).** Founder wants the most performant option. **Measured 2026-09-22: OpenConsole is clearly better** (3× throughput, p99 under a flood 0.31–0.43 ms vs 6.2–9.9 ms, 6× less host CPU per byte). ~~Founder decision (2026-09-22): use the OpenConsole that Windows Terminal installs, through our own launcher; bundling Microsoft's binaries not approved.~~ **Superseded — founder decision (2026-09-23): two lanes.** (1) Agents run with no console, over plain pipes, wherever they document a non-interactive mode: native transport, measured 4.1–4.3 GB/s per pane. (2) Everything that needs a console runs under **Microsoft's `Microsoft.Windows.Console.ConPTY` package (MIT), shipped with foldwave** — `conpty.dll` plus the `OpenConsole.exe` beside it — falling back to the built-in `conhost.exe` whenever it is missing or fails to start (slower, never broken). Why the change: Microsoft documents only `CreatePseudoConsole` (built-in host) and this package; driving Terminal's installed copy needs undocumented console-driver calls that only Windows Terminal itself makes, while WezTerm and Microsoft's node-pty ship the package (§8.6, `prior-art.md`). A console driver of our own was ruled out: console programs reach Windows' driver whatever we install, and programs that could be changed to use ours can use a pipe instead | Measured below and in §8.6. Linux/macOS use ptys |
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

**E2 result (measured 2026-09-22, quiet machine).** Windows' built-in
`conhost.exe` (10.0.26100.1) against OpenConsole from the
`Microsoft.Windows.Console.ConPTY` 1.24.260710001 package (MIT, the same
version line as the installed Windows Terminal 1.24), loaded at runtime with
`nativelite-pty`'s new `use_conpty_library` and `fwbench --conpty`. Identical
harness, workload and machine; only the console host differs.

*The first attempt was wrong, and why matters for foldwave.* OpenConsole opens
every session with a Primary Device Attributes query (`ESC [ c`) and **draws
nothing until a terminal answers it**; the built-in host never asks. The
harness did not answer, so the first run measured that stall: OpenConsole
looked *half* as fast, and each echo run had one ~680 ms key and one lost key.
`fwbench` now answers DA1 and cursor-position queries as any terminal must
(`crates/fwbench/src/terminal.rs`). **foldwave's emulator has to answer them
too**, or every OpenConsole pane starts with a stall.

| Measure | `conhost.exe` | OpenConsole | |
| --- | --- | --- | --- |
| Flood, 1 pane (32 MiB, 2 rounds) | 16.4–17.1 MiB/s | 58.3–58.8 MiB/s | **3.4×** |
| Flood, 4 panes | 64.2–66.2 | 171.3–188.9 | **2.8×** |
| Flood, 8 panes | 89.8–98.8 | 272.0–277.3 | **2.9×** |
| Flood, 16 panes | 144.0–145.3 | 433.0–443.8 | **3.0×** |
| Key echo, keys back to back (0 ms gap, n = 60) | p50 15.46 ms | p50 **0.01 ms** | no tick |
| Key echo, 25 ms gap (n = 60) | p50 0.13, p99 0.57 ms | p50 0.13, p99 0.72 ms | even |
| Key echo **during** an 8-pane flood (n = 200, 2 runs) | p99 **9.89 / 6.23 ms — over** | p99 **0.31 / 0.43 ms** | meets §4 |
| Flood throughput in that same run | 97.4–98.2 MiB/s | 288.6–289.1 MiB/s | 2.9× |
| Host CPU per MiB of output (16 × 128 MiB) | 86.8–87.0 ms | 13.3–14.4 ms | **6× less** |
| Peak working set per host | 8.3–8.7 MiB | 10.9 MiB | +2.4 MiB |

- **The ≈15.2 ms coalescing tick is conhost's, not ConPTY's.** OpenConsole
  echoes keys sent back to back in 0.01 ms at the median; conhost's p50 still
  follows 15.2 ms minus the gap. That settles the open question in §8.9: the
  tick is the built-in host's own render timer, not the Windows timer
  resolution and not `cmd`.
- **OpenConsole meets the §4 input target through ConPTY; conhost cannot.**
- **The built-in host is CPU-bound, and it is expensive.** At 16 panes the
  conhosts burned ≈13 of 16 logical cores to move 145 MiB/s. OpenConsole moved
  465–473 MiB/s on about 7. Even OpenConsole spends real CPU per byte —
  another reason agents that can run headless should (§8.1).
- **By E2's own decision rule, OpenConsole is clearly better** — on
  throughput, on p99 latency and on CPU — so the rule says bundle it, optional,
  falling back to the built-in host.
- **How to get it onto users' machines is a founder decision**, because every
  route touches the zero-dependency rule differently:
  1. *Bundle* the NuGet package's `conpty.dll` + `OpenConsole.exe` (MIT,
     ~1.2 MB): simplest, but ships Microsoft binaries in a nativelite app.
  2. *Use an installed Windows Terminal's* `OpenConsole.exe` when present,
     with our own launcher (a port of `winconpty`, MIT, needing `ntdll`
     calls): nothing redistributed, but only helps users who have Windows
     Terminal, and the launcher leans on undocumented console-driver details.
  3. *Build OpenConsole from source* (MIT) as part of the release: no opaque
     binaries, but a large C++ build in a Rust project.
  E2 answers *whether*. **Founder decision (2026-09-22): route 2, with the
  built-in host as the permanent fallback** — see §11 row 6. Microsoft's own
  `winconpty` does the same, preferring an `OpenConsole.exe` and falling back
  to `System32\conhost.exe` ([`prior-art.md`](prior-art.md)). Still to build:
  the launcher (a port of `winconpty`), finding the installed OpenConsole
  without depending on Windows Terminal's install path, and a per-pane health
  check that retries on conhost if OpenConsole fails to start. Telemetry was
  checked and is recorded in [`prior-art.md`](prior-art.md): OpenConsole has
  no networking of its own, and its telemetry-flagged events are few,
  per-session and carry no text.

**E2b: how far below native is the best console host? (measured 2026-09-22).**
Founder question: is a console host *inherently* this slow, or is there room?
Two things first:
- **ConPTY already passes VT through.** Microsoft removed the separate
  "passthrough mode" in March 2024 (microsoft/terminal #16954: it "fails to
  address other pain points (performance, out-of-sync issues after resize,
  etc.)") and in August 2024 rewrote ConPTY so that "any VT output that an
  application generates will now be given to the terminal unmodified" (#17510,
  "Goodbye VtEngine Edition", reporting ~20× faster output through ConPTY).
  OpenConsole 1.24 includes that; there is no faster switch left to turn on.
- **The emitter was flattering nobody but itself.** New `fwbench` flags make
  each flood pane replay pre-generated bytes (`--emit-cached`, so the emitter
  cannot be the bottleneck) and write them with `WriteFile` on a plain handle
  (`--emit-raw`) instead of `std`'s console path, which converts to UTF-16 and
  calls `WriteConsoleW`.

| One pane, MiB/s | generated, console API | cached, console API | cached, raw `WriteFile` |
| --- | --- | --- | --- |
| plain pipe (native) | 272.0–276.6 | 3,429.7–3,582.7 | **4,052.8–4,289.9** |
| OpenConsole 1.24 | 55.1–60.9 | 64.0–78.0 | **153.4–179.7** |
| built-in conhost | 18.0–18.3 | 19.4 | 23.1–23.4 |

| 16 panes, cached raw writes | Aggregate | Host CPU per MiB |
| --- | --- | --- |
| plain pipe (native) | **17,082–18,165 MiB/s** | — |
| OpenConsole 1.24 | 1,237–1,391 MiB/s | 9.7 ms |
| built-in conhost | 175–180 MiB/s | 83.4–83.9 ms |

- **The console API write path costs OpenConsole more than 2×** (64–78 →
  153–180 MiB/s one pane). Programs that emit raw VT through `WriteFile` are
  already much better served than ones going through `WriteConsoleW`.
- **Even so, the best console host available runs ~13× below native** at 16
  panes and ~24× below at one; conhost is ~100× below. OpenConsole still
  spends 9.7 ms of CPU per MiB on output it passes through unmodified — about
  twice what foldwave's entire parser plus screen apply costs (≈4.8 ms/MiB,
  E4).
- **So the gap is real and large.** Whether a purpose-built host that only
  forwards VT could close it depends on one unmeasured cost: the kernel console
  driver's own round trip per write. Scoping our own console host — protocol,
  API surface, effort and that risk — is the next step (E2c).

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
