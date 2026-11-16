# foldwave: what "best" means, and where the next gains are

> **Status:** 2026-11-16. A review of foldwave as built, and a research
> direction that follows from its own measurements. **The founder accepted the
> direction the same day, with Linux as the first Unix** ([design §11](design.md),
> rows 23 and 24), and with one correction that §3 and §4 now carry: foldwave
> is about speed and efficiency, and about pushing beyond, as the fold engine
> it is named for does. Everything else here is still a proposal; the
> decisions still open are listed in §8. Numbers marked
> *measured* are quoted from [`design.md`](design.md) with their section.
> Facts about other projects and about operating systems are in
> [`prior-art.md`](prior-art.md) and [`compute.md`](compute.md), each with its
> source. Everything else is a hypothesis and says so.

## 1. The idea, restated

The original idea ([design §1](design.md)) is to use the whole machine in
balance: the CPU reads and parses, RAM holds what is hot, the SSD keeps
everything, the GPU draws. foldwave has built that and measured it. This
document asks what the idea means *after* the targets are met, because the
measurements changed the question.

## 2. Review: where foldwave stands

**Every target is met with room to spare** (*measured*, design §10.1):

| Resource | Target | Measured | Headroom |
| --- | --- | --- | --- |
| Throughput, 16 panes | 273 MiB/s | 1,111–1,136 MiB/s in fwterm; 1,312–2,429 composed | 4–9× |
| CPU at the target rate | — | ≈1.3 of 16 cores; the machine ~92 % idle | — |
| History | 100 GB, search < 1 s | 104 GiB, search 238–284 ms | 3.5–4× |
| Memory per pane | a fixed budget | 33–43 MiB against 64 MiB, flat as history grows | 1.5–2× |
| Idle, 16 panes | ~0 % CPU | 0.78–0.88 % of one core | — |
| Crash | replay to the last flush | 96 of 96 logs exact prefixes | — |
| Install | — | one 3.9 MB file, no dependencies | — |

**What sets it apart today.** No other surveyed terminal pages history to disk
while running, indexes it, or searches it across panes
([prior-art](prior-art.md#where-the-others-moved)). Ghostty's maintainers
moved disk scrollback to "yes, planned" in April 2026, so the lead is real and
has a clock on it.

**Five things the measurements say that the design text has not yet drawn
together:**

1. **The pipeline is no longer where the gains are.** Each leg clears its
   target several times over, and E3 measured the whole terminal at ~0.5 % of
   a real 8-agent session (design §8.7b). Doubling any stage's speed would move
   nothing a person can feel.
2. **In the default configuration the GPU only draws.** Panes run in pane
   hosts, and hosts sign their own logs on the CPU (design §11 row 11): the
   window's GPU indexer signed 0 host blocks in the default run (§8.8a). The
   GPU index is real and measured, but it serves `--no-isolate` and
   `--host-signs gpu` only. The cheaper window path measured about −12 % total
   CPU and is still waiting on a founder ruling (§8.8a).
3. **The CPU paths are scalar.** No crate in this repository uses `std::arch`.
   panelog's CRC-32C is described as about 30 lines of plain code, signatures
   build at 0.75 GiB/s on one thread (§8.7a), and the log writer is CPU-bound
   at 363–388 MiB/s with signatures (§8.4). The engine crates (`ansi`,
   `vterm`, `panelog`) are in other repositories and were not read for this
   review, so their state is **unverified**. This matters for point 2: the
   GPU-against-CPU verdict was taken against a CPU path that has not been
   vectorized.
4. **Two things that decide "best" have never been measured.** Key-to-photon
   latency: the instrument resolves 6.0 ms per screen read, and the scorecard
   says the p99 < 5 ms end-to-end check still needs a local display or a
   camera (§10.1). And energy: no joule, watt or wakeup count appears anywhere
   in the design.
5. **The largest waste found so far was not foldwave's.** Two runaway `find`
   processes had burned 6.8 CPU-hours each with nothing surfacing them
   (§8.7b); Defender took ~14 % of the machine during E3 and short-lived build
   tools ~42 %. The design drew the right conclusion ("an argument for
   per-pane resource accounting") and then did not build it: the strip's `CPU`
   reading is fwterm's own share.

**Stale text in `design.md`** (left as it is; listed so it can be fixed in one
pass):

- §4.1 still carries "723 MiB/s is *parsing alone*", "Parsing scales
  sublinearly: 111 MiB/s on one thread", and "§8.0 is now stale… CPU parsing
  (723 MiB/s) is the binding leg", all superseded by the 1,828 MiB/s row in the
  same section.
- §8.0 point 4 says checkpoint traffic is "still inside the 479 MiB/s
  ceiling", a figure §4.1 retracts.
- §8.8 says how compute slows rendering "is **unknown**"; E1 measured it.
- §8.7a says index construction "ships only if" E3 passes; it shipped.
- §10's milestone table lists M5's status above M4's, and several cells (§10
  M4 status, §11 rows 16, 17, 19, 20) are journals of many hundred words in one
  table cell. Moving each to a subsection under its heading would make the
  decisions readable again.

## 3. The thesis

Terminals that have claimed to be the fastest each won one benchmark, and the
benchmarks in use stop at the parser or the frame counter
([prior-art](prior-art.md#what-the-benchmarks-in-use-leave-out)). None of them
measures what a person feels or what the machine pays.

foldwave is named for the fold engine: a drive that does not cross space
faster but removes the space to be crossed. That is already how it got its
speed. Logging before parsing took the parser off the ingest path. Not parsing
a hidden pane returned about 40 % of fwterm's CPU. Handing out spans, not
tokens, made emulation 1.76× faster by not allocating. None of these made a
stage work harder; each removed a trip.

So speed and efficiency are one goal here, not a trade, and the §4 targets are
a floor, not a finish. The pipeline clears them by 4–9×; the question now is
where the next folds are. The measurements say they are not inside the
pipeline's stages any more. They are in three places no benchmark in use
scores:

1. **Instant.** The time from a key to a photon, and its worst case, are at
   the floor the display allows, whether the machine is idle or saturated.
2. **Invisible.** What the terminal costs the machine (CPU time on the fast
   cores, memory, disk, energy, wakeups) is small enough to ignore, is
   published, and shrinks under pressure.
3. **A lever.** The terminal is the one program that sees every process every
   agent starts. Its own share is 0.5 %; its reach is the other 99.5 %.

## 4. Principles

These extend [design §5](design.md); they replace nothing in it.

1. **Fold the distance.** The fastest path is the one with the least in it:
   fewer copies, fewer wake-ups, fewer trips between devices, less work done
   for nobody. Every gain is looked for as something to remove before
   something to speed up, and a target once met is replaced by the next
   ceiling, not retired. Speed that is not needed in one place is spent in
   another: headroom on the fast cores goes back to the agents, so the whole
   session runs faster. *Evidence:* parsing on one E-core ran at 55.4 MiB/s
   against 125.5 on a P-core (E4, before the span tokenizer), and one pane's
   target is 17.1 MiB/s. A slow core covers a pane three times over, and the
   P-core it leaves free goes to a compiler.
2. **The right silicon is a class, not only a device.** "CPU or GPU" is one
   axis. The others are which kind of core, at what clock, with what I/O and
   memory priority. Every modern OS has a way to say "this work is not urgent"
   ([compute](compute.md#os-mechanisms-for-placing-work-researched-2026-11-16)),
   and foldwave's invariant already sorts its work for it: the log is truth
   and runs at full priority; everything derived from it is a cache and can
   run in the efficiency class.
3. **Touch each byte once per resource, while it is already there.** A byte
   arriving from a pane is in the CPU's cache once. Checksum, line starts,
   marks and search signature are all functions of that same pass. Doing them
   in one vectorized sweep is cheaper than any second trip, to another thread
   or across a bus. A second device earns the work only when it needs no trip
   at all (memory the GPU already shares) or when the batch is large and
   nobody is waiting (the measured ≥ 4 MiB rule).
4. **Work at the rate of the observer.** Parse what is watched (built, design
   §11 row 10). Draw what can be read: a pane scrolling faster than anyone can
   read does not need the display's full rate. Index what is searched.
   Compress what is cold. Each is the same rule: derived work is owed only to
   someone who is looking.
5. **Only the human's milliseconds are latency.** One path gets the fast core,
   the short queue and the wake-up: key in, glyph out. Everything else may be
   late. That path is measured at the photon, on a quiet machine as well as a
   loaded one, because the worst tail seen so far was on the quiet one (E5).
6. **Give back before being asked.** Memory a hidden pane holds is rebuildable
   from its log, so the OS may take it. Threads that have nothing to do sleep
   on an event, never a timer. A terminal that is not being looked at should
   cost what a closed one costs.
7. **Account for the whole session.** foldwave already owns a job object per
   pane and a process-tree monitor (`resmon`). Showing where the machine went,
   per pane, and flagging work that burns without producing anything, is worth
   more to an agentic session than any further gain inside the terminal.
8. **The proof is part of the product.** `fwprobe` already measures any
   terminal from outside. A published, reproducible scorecard on latency,
   footprint and energy, run against the field, is how the claim is made, and
   on energy and wakeups it would be the first one anywhere
   ([prior-art](prior-art.md#what-nobody-measures)).

## 5. One philosophy, every platform

foldwave is Windows first (design §11 row 5) and is meant for Linux and the
other Unix systems too. The principles above are not Windows ideas. Each
system has its own name for the same thing, and on several counts the Unix
systems offer more. Sources for every cell are in
[compute](compute.md#os-mechanisms-for-placing-work-researched-2026-11-16);
none of it is measured yet.

| Principle | Windows | Linux | macOS |
| --- | --- | --- | --- |
| An efficiency class for derived work | EcoQoS per thread or process | `SCHED_IDLE` per thread; `cpu.idle` per cgroup | QoS utility and background; `PRIO_DARWIN_BG` also throttles disk |
| The pressure signal | CPU utilization and a low-memory notification | pressure-stall information: stalled time for CPU, memory and I/O, with triggers | a memory-pressure event source |
| One handle on a pane's whole process tree | a job object (built) | a delegated cgroup, through a systemd scope | none found; process groups and polling |
| Giving memory back | offer and reclaim | `MADV_FREE`, `MADV_PAGEOUT`, also on another process | purgeable memory, `MADV_FREE` |
| A write that is durable when it returns | unbuffered write-through (built, measured) | `O_DIRECT` with `O_DSYNC` | only `F_FULLFSYNC`, which is slow; a barrier sync is the middle tier |
| The filesystem trap | NTFS compression (found, design §4.1) | Btrfs copy-on-write and compression | none identified |
| A frame as late and as short as possible | per-present mode, present wait, possibly a DXGI path | the same Vulkan extensions; tearing and FIFO depend on the compositor | `CAMetalLayer`, always composited in a window |
| Compute over memory the GPU already shares | integrated GPUs only | `VK_EXT_external_memory_host` on Intel and AMD | every Apple-silicon Mac, by design |
| Energy, readable | GPU only (`nvidia-smi`) | CPU counters need root; GPU through NVML | per process, in nanojoules, without root |
| The transport | a console host in between, or pipes | a kernel pty with 4–8 KiB buffers, or pipes | a kernel pty, possibly 1 KiB a read |
| What taxes the session | Defender; a Dev Drive softens it | file indexers | scanning of every `exec`; the Developer Tools list softens it |

**What this changes in the plan:**

- **The principles become an interface, the mechanisms a backend.** `resmon`
  already has a `sys` module per platform. "Run this thread in the efficiency
  class", "tell me about pressure", "one handle on this tree" and "take this
  memory if you need it" are four small calls with one implementation per
  system, in the same shape as the pane-transport interface design §8.9
  already has.
- **Linux is where the lever is strongest.** A cgroup per pane gives exact
  accounting, real limits and a pressure reading per pane, which is more than
  a Windows job object reports. E9 (the session ledger) is cheaper to build
  there.
- **Apple silicon is where the original idea is most literal.** CPU and GPU
  share memory, so "the GPU reads what the CPU just wrote" costs nothing; the
  efficiency cores are a documented scheduling target; and energy per process
  is readable. E16 and the energy half of E8 are naturally measured there
  first. It also needs a Metal renderer: Khronos has now deprecated MoltenVK,
  which confirms design §8.6.
- **Durability is not portable as built.** "Durable when the write returns"
  holds on Windows and Linux. On macOS it costs a full device flush per write.
  The crash target (design §4, "up to the last flush") allows a cadence there;
  which one is a measurement, then a decision.
- **Two hazards to test early.** On Windows, whether agents under a minimized
  foldwave are slowed by window-driven QoS (in E11). On macOS, whether App Nap
  throttles a pane host, which has no window.
- **One dependency question on Linux.** A Vulkan window on Wayland needs
  `libwayland-client`. It is a system library in the sense `vulkan-1.dll` is,
  but it is the founder's reading of the zero-dependency rule that settles it
  (§8).

## 6. Research programme

Each experiment follows the house rule: a hypothesis, a measurement on the
reference machine with median, p90/p99 and sample count, and a rule for keeping
or dropping it. Numbering continues from E6. Order is by expected value.

| # | Experiment | Hypothesis | Measure | Keep if |
| --- | --- | --- | --- | --- |
| E7 | **Key to photon** | fwterm's end-to-end latency is set by the present path, not by its own code (composed pipeline p99 0.68 ms, design §8.0) | A photodiode or a ≥ 240 fps camera on the local display; PresentMon's `PresentMode` to see which path the Vulkan swapchain gets. Then, one at a time: a present mode switched per present (FIFO idle, mailbox or immediate while typing), `VK_KHR_present_wait2`, `VK_NV_low_latency2`, and a DXGI flip-model waitable swap chain as the comparison | it is the instrument; a present-path change stays if it cuts p50 by half a refresh or more, or tightens p99, with idle still at 0 frames |
| E8 | **Footprint ledger** | foldwave's cost can be stated in a few numbers nobody else publishes | `fwbench` mode: CPU-seconds by core class, context switches and wakeups per second idle, working set, VRAM, GPU watts (`nvidia-smi`), bytes on disk per byte logged; joules where the OS exposes them (per process on macOS; the harness with a capability on Linux; GPU only on Windows). The same from outside for other terminals, with `fwprobe` | it is the instrument; it becomes the public scorecard (§7) |
| E9 | **Session ledger** | the waste worth finding is in the panes, not the terminal (the 6.8 CPU-hour `find`, design §8.7b) | per-pane process-tree CPU, memory and I/O from `resmon`, shown in the strip; a rule for "burning without output"; Defender's share and whether a pane's directory is on a trusted Dev Drive. Replay the runaway case | the rule flags the replayed runaway within minutes with no false alarm across an E3 fleet run, and the recorder costs < 0.1 % of a core |
| E10 | **One vectorized ingest pass** | the logger's CPU per GiB at least halves with AVX2 line and control-byte scans, the SSE4.2 CRC-32C instruction and a tuned signature build, all on `std::arch` | CPU-s per GiB for the logger, before and after, and the flooding-pane cost (≈0.7 of a core, design §8.8). Then re-run §8.8a's host-signing comparison against the new CPU path | CPU-s per GiB falls ≥ 30 % with byte-identical logs and signatures. Whatever the result, the GPU-or-CPU default is re-judged on it |
| E11 | **Efficiency class for derived work** | signature builds, catch-up replay, retention and hidden panes' hosts can run in the OS's efficiency class at no cost a person feels, returning fast-core time to the agents | a fleet build's wall time and fwterm's CPU-s by core class, with and without; catch-up time on showing a pane; log write p99. Also: whether agents under a *minimized* foldwave are slowed by Windows' window-driven QoS ([compute](compute.md#windows)) | pane-switch catch-up stays inside its present figure (200 MB in ~0.3 s), write p99 does not move, and fast-core time falls measurably |
| E12 | **Compressed cold generations** | agent output compresses ≥ 3× with an LZ4-class codec of our own, per 1 MiB block, on efficiency-class threads, so the 10 GiB cap holds ≥ 3× the history; search stays < 1 s because only candidate blocks are read | ratio on real session logs; compress rate on an E-core; the 104 GiB search benchmark on compressed generations | ratio ≥ 2× on real logs and search stays under 1 s; else drop it (NTFS compression is already ruled out, design §4.1) |
| E13 | **Memory given back** | a shed pane's emulator and history can be offered to the OS and rebuilt from the log if taken | working set of 16 panes, 15 hidden, before and after; time to show a pane whose memory was taken; the low-memory notification as a governor input in place of polling | hidden panes' resident memory falls ≥ 50 % and showing one stays inside the catch-up figure |
| E14 | **The idle tail** | the quiet-machine p99 (4.37 ms idle against 0.68 ms loaded, E5) comes from deep CPU sleep states | key-echo p99 over ≥ 1,000 keys, idle, with and without one core kept shallow for 250 ms after any input; the watts that costs | p99 on a quiet machine drops below the loaded figure for < 1 W average |
| E15 | **Draw at the reader's rate** | a flooding pane drawn at 30–60 fps is indistinguishable in use from 150 fps and saves GPU power | GPU watts and fwterm CPU at each rate during a flood; the founder's eye on the feel, since typing keeps the full rate (the 250 ms input window already exists, design §8.8) | watts fall measurably and the founder accepts the feel |
| E16 | **Compute where memory is shared** | on a GPU that shares the machine's memory (an integrated GPU, Apple silicon), the signature build needs no upload, so the ≥ 4 MiB batching rule and the read-back that sank the window path (§8.8a) do not apply | the vendor-neutral Vulkan compute build design §8.7a already calls for, over host memory imported with `VK_EXT_external_memory_host`, on the reference machine's Intel GPU; later the Metal no-copy equivalent | it beats the E10 CPU path in CPU-s per GiB with frames unchanged |

**Table stakes, not research.** A terminal is also judged on what it accepts.
The 2026 list is in [prior-art](prior-art.md#protocols-a-terminal-is-judged-on-in-2026):
the kitty keyboard protocol, synchronized output (mode 2026), OSC 8 and 52,
in-band resize (mode 2048), and OSC 7501 program status, which is the one
agent-specific sequence in the field and maps directly onto the strip's agent
glyphs. Where fwterm stands on each has not been audited.

**What to stop considering.** Each was a reasonable candidate; the data has
answered it.

| Idea | Why not |
| --- | --- |
| A GPU VT tokenizer (design §8.7b, "next spike") | the CPU has 6.7× headroom and the terminal is 0.5 % of a session; there are no cores to give back |
| Bulk GPU regex search | the index answers in < 300 ms at 104 GiB |
| Drawing on the Intel GPU on the reference desktop | both monitors are on the RTX 3070, so it adds a copy per frame, and E1 found no contention to escape |
| Large pages | cannot be given back and escape the pane host's job |
| Pinning threads to fast cores | measured, no better (design §8.8) |
| IoRing for the log writer | one write in flight is the measured optimum (design §4.1) |
| The Reflex SDK, Windows ML in process, DirectStorage | each needs a proprietary redistributable |
| Raising GPU utilization as a goal | design §11 row 9 already rules it out; this document agrees |

## 7. The scorecard

The claim "best" should be a table anyone can reproduce, with foldwave's row
beside the others'. Proposed columns, each from E7 or E8:

| Axis | Unit | Who publishes it today |
| --- | --- | --- |
| Key to photon, idle and under a 16-pane flood | ms, p50 / p99, n ≥ 1,000 | nobody with a distribution; one n = 5 camera study |
| Frames under flood | fps, gap p99 | DOOM-fire, informally |
| Throughput, per pane and 16 panes | MiB/s | vtebench, kitty (parser only) |
| Idle cost, 1 and 16 panes | % of a core, wakeups/s, watts | nobody |
| Cost per GiB of output | CPU-seconds, joules | nobody |
| Memory per pane, and how it grows with history | MiB | one study, per window |
| History reachable, and time to search it | GiB, ms | nobody (no other terminal has it) |
| Survives a kill | logs intact of logs written | nobody |
| Install size, dependencies | MB, count | nobody |

## 8. Open decisions for the founder

Nothing above is assumed. These are the calls it needs:

1. ~~Is this the direction?~~ **Decided 2026-11-16: yes** (design §11 row
   23). Speed and efficiency stay the point; the three places in §3 are where
   they are pursued next.
2. **Report or act (E9, E11).** A session ledger that only *shows* a runaway
   process is clearly inside the non-goal "host the user's own unmodified
   tools". Lowering the scheduling class of a hidden pane's process tree does
   not modify a tool, but it does change how the user's work runs. Report
   only, act on request, or act by default?
3. **The default signer (already open, design §8.8a).** Suggested: decide it
   after E10, on the vectorized CPU path.
4. **A camera or photodiode for E7** (already raised, design §10.1).
5. **Drawing below the display's rate during a flood (E15).** A feel
   question; the measurement only says what it saves.
6. **A public scorecard (§7).** Whether to publish foldwave's numbers against
   named terminals, and when.
7. **A compression codec as its own crate (E12).** Design §8.4 already allows
   for one; this would be the decision record.
8. ~~Which Unix comes first.~~ **Decided 2026-11-16: Linux** (design §11 row
   24). When macOS follows is open.
9. ~~`libwayland-client` on Linux.~~ **Decided 2026-11-16: nativelite's own
   (`nativelite-wayland`), and wired in** (design §11 row 25).
10. **Durability on macOS (§5).** A full flush per write, a barrier, or a
    cadence.
11. **Targets beyond §4.** The founder's direction is to push beyond, and the
    §4 targets are met. Candidates for the next set, each a proposal until
    confirmed and each needing its benchmark first: key to photon within one
    refresh at p99, idle and under a flood (E7); idle cost stated in wakeups
    per second and watts, not only CPU share (E8); 16 panes at the measured
    transport ceiling instead of 1 GB/min each; search under 1 s at 1 TB.
12. ~~`libxkbcommon` for keyboard layouts.~~ **Decided 2026-11-16: nativelite's
    own (`nativelite-xkb`), and wired in** (design §11 row 25).
13. **Where foldwave's folder lives on Linux.** Built as
    `$XDG_DATA_HOME/foldwave`, else `~/.local/share/foldwave`, for settings
    and default logs together; the convention would split them (settings
    under `~/.config`, logs under `~/.local/state`).
14. **A title bar on GNOME.** GNOME draws none for an application. Our own,
    in the grid like the instrument strip, or the `libdecor` library.
15. **Vulkan frames on Linux.** With our own Wayland library there is no
    Vulkan surface to draw to, so Linux draws on the CPU renderer. The way
    back to the GPU is Vulkan images handed to the compositor as dmabufs, with
    our own frame pacing: whether to build it now, or run on the CPU renderer
    while the rest of the Linux port catches up. It decides E7 (key to
    photon) on Linux.
