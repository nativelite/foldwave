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

Reference machine: the founder's Windows box (64 GB RAM, NVMe, RTX 3070 8 GB).
Every target gets a reproducible benchmark before it is claimed.

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
Per pane, an append-only log, in segments:

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
- **Out:** GPUDirect Storage (Linux datacenter only), nvCOMP (proprietary),
  and CUDA for parsing, input or rendering.
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
| VT parsing | **CPU, decided** | A sequential, branchy state machine. 16 panes is 16-way parallelism; a GPU wants thousands of threads. Transfer alone would exceed the parse cost |
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

  Design consequence: foldwave supports all three transports (ConPTY,
  pipes/headless, pty) behind one pane interface. Agent integrations prefer
  headless where the agent documents it.

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
| M0 status (2026-09-15) | **Partly delivered.** `resmon` (four-resource attribution, E3) and `fwbench` (pty pane flood + key-echo latency) are in `crates/`. First numbers on the reference machine: 8 panes sustained **94.9 MiB/s** aggregate (≈12 MiB/s per pane, against a 17.1 MiB/s per-pane target), key echo under that flood **p50 0.06 ms / p99 11.74 ms**. Still to build: frame timing (needs M3), E1, E2, and a disk throughput/latency probe | see `crates/fwbench/README.md` |
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
