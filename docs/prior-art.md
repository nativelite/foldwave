# Prior art: open-source terminals (verified)

Checked on 2026-09-15 against primary sources: repo LICENSE and Cargo files,
source, official docs, and maintainer posts or PRs. Every line carries its
source. Anything that couldn't be confirmed says **unverified**. Re-check
licenses before porting anything: projects relicense.

## Reusable? (license summary)

Our libraries are MIT (see [design §6](design.md)).

| Project | License (SPDX) | Port into an MIT crate? |
| --- | --- | --- |
| Ghostty (incl. libghostty-vt) | MIT | ✅ with attribution |
| WezTerm | MIT | ✅ |
| Windows Terminal (incl. AtlasEngine, OpenConsole) | MIT | ✅ |
| foot | MIT | ✅ |
| Rio | MIT | ✅ |
| xterm.js | MIT | ✅ |
| libvterm (neovim mirror, archived) | MIT | ✅ (upstream leonerd license **unverified**) |
| Warp `warpui` / `warpui_core` | MIT | ✅ those crates only |
| `vte` crate (alacritty/vte) | Apache-2.0 OR MIT | ✅ take it under MIT |
| vtebench | MIT OR Apache-2.0 | ✅ (a benchmark, for M0) |
| Alacritty | Apache-2.0 | ⚠️ code keeps its Apache-2.0 terms and NOTICE, so not pure MIT |
| Contour | Apache-2.0 | ⚠️ same as Alacritty |
| kitty | GPL-3.0 | ❌ read for ideas only |
| Zed `crates/terminal` | GPL-3.0-or-later | ❌ |
| Warp (everything except the MIT UI crates) | AGPL-3.0 | ❌ |

**Porting SIMD code means rewriting it on std.** Ghostty's SIMD path uses Google
Highway and simdutf; `vte` depends on `memchr` and `arrayvec`; Rio uses `memchr`
and simdutf. Porting any of them to our zero-dependency rule means rewriting
those parts on std (`std::arch` or word-at-a-time tricks).

## Per project

### Alacritty (Rust)
- License `Apache-2.0` — https://github.com/alacritty/alacritty/blob/master/alacritty/Cargo.toml
- Parser: the `vte` crate (0.15) — https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/Cargo.toml
- Renderer: OpenGL, with a GLES2 or GLSL3 path chosen at runtime — https://github.com/alacritty/alacritty/blob/master/alacritty/src/renderer/mod.rs
  - Glyph atlas — https://github.com/alacritty/alacritty/blob/master/alacritty/src/renderer/text/mod.rs
  - Per-line damage tracking — https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/src/term/mod.rs
- Scrollback: in-memory ring buffer rotated by a `zero` offset — https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/src/grid/storage.rs
  - Capped at `MAX_SCROLLBACK_LINES = 100_000` — https://github.com/alacritty/alacritty/blob/master/alacritty/src/config/scrolling.rs
- OSC 133: **unverified**, appears absent.

### `vte` crate (Rust)
- License `Apache-2.0 OR MIT`; depends on `memchr`, `arrayvec` — https://github.com/alacritty/vte/blob/master/Cargo.toml
- Parser: Paul Williams' DEC ANSI state machine, with semantics behind a `Perform` trait — https://github.com/alacritty/vte/blob/master/README.md
- Fast path: in the ground state, `memchr(0x1B)` finds the next ESC, then a bulk `str::from_utf8` validates and dispatches the run — https://github.com/alacritty/vte/blob/master/src/lib.rs (`advance_ground`)

### WezTerm (Rust)
- License MIT — https://github.com/wezterm/wezterm/blob/main/LICENSE.md
- Parser: `vtparse`, the DEC ANSI state machine modified for UTF-8 — https://github.com/wezterm/wezterm/blob/main/vtparse/src/lib.rs
- Renderer: OpenGL (default), WebGpu (Metal, Vulkan or DX12) or Software; Software is used under Windows Remote Desktop — https://wezterm.org/config/lua/config/front_end.html
- Scrollback: in-memory `VecDeque<Line>` — https://github.com/wezterm/wezterm/blob/main/term/src/screen.rs
  - Default `scrollback_lines = 3500` — https://wezterm.org/config/lua/config/scrollback_lines.html
- Shell integration: OSC 133 zones, OSC 7, OSC 1337 — https://wezterm.org/shell-integration.html

### Ghostty (Zig)
- License MIT — https://github.com/ghostty-org/ghostty/blob/main/LICENSE
- SIMD parsing:
  - `src/simd/vt.cpp` uses Highway and simdutf to find control sequences and decode UTF-8 up to the next ESC — https://github.com/ghostty-org/ghostty/blob/main/src/simd/vt.cpp
  - First SIMD work: PR #1472 (2024-02-06) — https://github.com/ghostty-org/ghostty/pull/1472
  - SIMD OSC reading: PR #14011 (2026-08-25) — https://github.com/ghostty-org/ghostty/pull/14011
- Renderers: `Metal.zig`, `OpenGL.zig`, `WebGL.zig` — https://github.com/ghostty-org/ghostty/tree/main/src/renderer
- Scrollback:
  - A linked list of page-aligned pages allocated straight from the OS (mmap / VirtualAlloc) — https://github.com/ghostty-org/ghostty/blob/main/src/terminal/PageList.zig
  - LZ4 page compression that decommits resident memory, on main since 2026-07-08 (whether it's in a release is **unverified**) — https://github.com/ghostty-org/ghostty/blob/main/src/terminal/compress.zig
  - The docs describe scrollback as "completely in memory", `scrollback-limit` in bytes, and unlimited scrollback as "a future planned feature" — https://ghostty.org/docs/config/reference
- Shell integration docs: https://ghostty.org/docs/features/shell-integration (which sequences it supports **unverified** here)
- **libghostty-vt:** extracted from Ghostty, announced as "zero-dependency (not even libc)", SIMD parsing, C API not ready as of 2025-09-22 — https://mitchellh.com/writing/libghostty-is-coming

### kitty (C / Python), GPL-3.0: ideas only
- License GPL-3.0 — https://github.com/kovidgoyal/kitty/blob/master/LICENSE
- SIMD escape parser (AVX2 / SSE4.2 / ARM), reported 50–400% faster — https://github.com/kovidgoyal/kitty/issues/7005
- Renderer: OpenGL 3.3 — https://github.com/kovidgoyal/kitty/blob/master/kitty/data-types.h
  - Glyph cache in VRAM; parsing on a separate thread from rendering — https://sw.kovidgoyal.net/kitty/performance/
- Scrollback: allocated on demand in 2048-line segments — https://github.com/kovidgoyal/kitty/blob/master/kitty/history.c
  - Negative `scrollback_lines` means effectively infinite, in RAM — https://sw.kovidgoyal.net/kitty/conf/
- Benchmark (kitty's `kitten __benchmark__`, kitty 0.33, Ryzen 7 PRO 5850U), average MB/s — https://sw.kovidgoyal.net/kitty/performance/

  | Terminal | MB/s |
  | --- | --- |
  | kitty | 134.6 |
  | gnome-terminal | 61.8 |
  | alacritty 0.13.1 | 54.1 |
  | wezterm 20230712 | 48.5 |
  | xterm | 30.7 |
- Shell integration: OSC 133 — https://sw.kovidgoyal.net/kitty/shell-integration/

### Windows Terminal (C++)
- License MIT — https://github.com/microsoft/terminal/blob/main/LICENSE
- Parser: `StateMachine::ProcessString` prints plain runs in bulk, using `FindActionableControlCharacter` (SSE2)
  - https://github.com/microsoft/terminal/blob/main/src/terminal/parser/stateMachine.cpp
  - https://github.com/microsoft/terminal/blob/main/src/types/utils.cpp
- Renderer (AtlasEngine): `BackendD3D` is Direct3D with its own glyph cache; `BackendD2D` is a Direct2D fallback for remote desktop or no GPU — https://github.com/microsoft/terminal/blob/main/src/renderer/atlas/README.md
- Scrollback:
  - Rows in a VirtualAlloc'd buffer committed on demand — https://github.com/microsoft/terminal/blob/main/src/buffer/out/textBuffer.hpp
  - `DEFAULT_HISTORY_SIZE = 9001` — https://github.com/microsoft/terminal/blob/main/src/inc/DefaultSettings.h
  - With `persistedWindowLayout`, the buffer is saved to disk as VT on exit and restored on relaunch (PR #16598, 2024-03-29) — https://github.com/microsoft/terminal/pull/16598
- Shell integration: OSC 133 A/B/C/D, marks on the scrollbar — https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration

### foot (C)
- License MIT — https://codeberg.org/dnkl/foot/src/branch/master/LICENSE
- Renderer: CPU via pixman, no GPU — https://codeberg.org/dnkl/foot/src/branch/master/meson.build
  - Damage tracking plus "scroll damage" (memmove or a mapping trick) — https://codeberg.org/dnkl/foot/wiki/Performance
- Scrollback: ring grid (`offset`/`view`) — https://codeberg.org/dnkl/foot/src/branch/master/terminal.h
- vtebench (2022-05-12, i9-9900), times in ms — https://codeberg.org/dnkl/foot/src/branch/master/doc/benchmark.md

  | Test | foot | Alacritty 0.10.1 |
  | --- | --- | --- |
  | dense cells | 45.46 | 97.45 |
  | unicode | 10.92 | 15.94 |

  A CPU renderer beating a GPU one on throughput is a reminder that the GPU is not the parse bottleneck.
- OSC 133 handler at `osc.c` — https://codeberg.org/dnkl/foot/src/branch/master/osc.c

### Contour (C++), Apache-2.0
- License — https://github.com/contour-terminal/contour/blob/master/LICENSE.txt
- Parser: vt100.net state machine plus a `parseBulkText` fast path — https://github.com/contour-terminal/contour/blob/master/src/vtparser/Parser.hpp
- Renderer: Qt RHI (which GPU APIs **unverified**) — https://github.com/contour-terminal/contour/tree/master/src/contour/display
- Persistent sessions via a daemon; vertical line markers; synchronized output — https://github.com/contour-terminal/contour/blob/master/README.md
  - Scrollback design and whether sessions persist to disk: **unverified**

### Rio (Rust)
- License MIT — https://github.com/raphamorim/rio/blob/main/LICENSE
- Parser `rio-vt` with `memchr` and simdutf — https://github.com/raphamorim/rio/blob/main/rio-vt/Cargo.toml
- Renderer `sugarloaf`: native Metal (`baremetal`) and optional `wgpu` — https://github.com/raphamorim/rio/blob/main/sugarloaf/Cargo.toml
- Scrollback and benchmarks: **unverified**

### Warp (Rust)
- `warpui`/`warpui_core` are MIT; the rest is AGPL-3.0 — https://github.com/warpdotdev/Warp/blob/master/README.md
- Metal renderer with a glyph texture atlas, and "blocks" — https://www.warp.dev/blog/how-warp-works (vtebench chart numbers **unverified**)

### Zed terminal (Rust), GPL-3.0-or-later
- `crates/terminal` license — https://github.com/zed-industries/zed/blob/main/crates/terminal/Cargo.toml
- Uses `alacritty_terminal` from a Zed fork — https://github.com/zed-industries/zed/blob/main/Cargo.toml
- GPUI: Metal on macOS; Linux depends on `gpui_wgpu`; Windows backend **unverified** — https://github.com/zed-industries/zed/tree/main/crates

### xterm.js (TypeScript)
- License MIT — https://github.com/xtermjs/xterm.js/blob/master/LICENSE
- Parser: `TransitionTable` from the vt100.net / Paul Williams state machine — https://github.com/xtermjs/xterm.js/blob/master/src/common/parser/EscapeSequenceParser.ts
- Buffer: `CircularList`, default `scrollback: 1000` — https://github.com/xtermjs/xterm.js/blob/master/src/common/services/OptionsService.ts
- WebGL2 renderer addon; `@xterm/headless` plus a serialize addon for state save/restore — https://github.com/xtermjs/xterm.js/blob/master/README.md

## Cross-cutting findings

### No surveyed terminal pages scrollback to disk while running
- **Windows Terminal:** writes the buffer to disk on exit and restores it (PR #16598).
- **Ghostty:** compresses pages in RAM; unlimited scrollback is "planned".
- **kitty:** can keep unlimited scrollback, but in RAM.
- **Contour:** persistent sessions use a daemon; whether that touches disk is **unverified**.

This is the gap [design §8.4](design.md) (the pane log) targets.

### Every fast parser uses the same trick
Scan for ESC and other control bytes with a vectorized search, bulk-decode the
plain-text run, and enter the state machine only at control bytes:
- Ghostty — Highway + simdutf
- kitty — hand-written SSE4.2 / AVX2 / NEON
- `vte` — memchr + bulk UTF-8 check
- Windows Terminal — SSE2 control-character scan
- Rio — memchr + simdutf

### GPU renderers converge on glyph atlas + damage tracking
Alacritty, Windows Terminal (AtlasEngine), kitty and Warp all do this. Two
projects also keep a CPU or software fallback: WezTerm (Software) and Windows
Terminal (Direct2D), for remote desktop or no GPU.

### vtebench measures only PTY read throughput
- Repo (alacritty org) — https://github.com/alacritty/vtebench
- Its own disclaimer: no frame rate or latency measurement.
- Tests: cursor_motion, dense/light/medium cells, scrolling variants, sync_medium_cells, unicode.
- **Consequence:** useful as a workload inside our M0 harness, not as the harness.

### ConPTY on Windows
- A console host always sits between the terminal and console apps. It runs the Console API calls and renders changes back to the terminal as VT.
  - https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/
  - https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session
- **Windows Terminal ships its own OpenConsole** to get newer ConPTY behavior. Its `winconpty` looks for `OpenConsole.exe` next to the module (or in an `x64`/`arm64`/`x86` subfolder) before falling back to `System32\conhost.exe` — https://github.com/microsoft/terminal/blob/main/src/winconpty/winconpty.cpp (`_ConsoleHostPath`)
- OpenConsole is MIT (Windows Terminal repo), so we can bundle it too. Whether to bundle it is decided by measurement: experiment E2 in [design §11](design.md).
- **Measured (E2, 2026-09-22):** OpenConsole 1.24 beats the built-in host on throughput (~3×), latency under load and CPU per byte ([design §11](design.md)). It opens every session with a Device Attributes query (`ESC [ c`) and draws nothing until the terminal answers it.
- **Telemetry in OpenConsole (checked 2026-09-22):**
  - **No networking of its own.** `OpenConsole.exe` from `Microsoft.Windows.Console.ConPTY` 1.24.260710001 imports no network DLL (no `ws2_32`, `winhttp`, `wininet`), and 8 OpenConsole processes under an 8-pane flood owned **0 TCP connections and 0 UDP endpoints** (*measured*, `Get-NetTCPConnection` / `Get-NetUDPEndpoint` polled every 100 ms). It does call `EventRegister` / `EventWriteTransfer`, i.e. ETW.
  - **ETW providers flagged for Windows telemetry.** The `Microsoft.Windows.Console.Host` provider is defined with `TraceLoggingOptionMicrosoftTelemetry()` — https://github.com/microsoft/terminal/blob/main/src/host/tracing.cpp
  - **Few telemetry-keyword events, none per keystroke or per byte.** In the console-host code (not the Windows Terminal UI), a search of `main` finds: `IsLegacyLoaded` (a boolean, only when the legacy console loads) — https://github.com/microsoft/terminal/blob/main/src/host/exe/exemain.cpp — and `ConsoleHandoffSessionStarted` / `Succeeded` / `Failed` (a CLSID, a boolean, an HRESULT; the default-terminal handoff path) — https://github.com/microsoft/terminal/blob/main/src/server/IoDispatchers.cpp. Whether the handoff events can fire on the ConPTY path foldwave uses was **not traced**.
  - **Verbose events carry content but are debug-only.** `CharsTrace` and the input-record events include characters and keys at `WINEVENT_LEVEL_VERBOSE` with a trace keyword (tracing.cpp); they are recorded only by a session that enables that level explicitly.
  - **Whether anything leaves the machine is Windows' decision**, via its own telemetry service and the user's diagnostic-data setting — not OpenConsole's. The built-in `conhost.exe` comes from the same codebase and the same provider, so switching changes nothing here.
  - **Caveats:** the source read was `main`, not the exact 1.24.260710001 tag; active ETW sessions could not be listed without elevation; the Windows Terminal *app* (settings editor, tabs, command palette) has far more telemetry, and foldwave does not use it.

### Shell integration
- **OSC 133** (semantic prompts proposal, Per Bothner): A/N/P prompt start, B/I end, C command start, D command end — https://gitlab.freedesktop.org/Per_Bothner/specifications/-/blob/master/proposals/semantic-prompts.md
- **OSC 633** (VS Code): A/B/C/D, plus E (command line with a nonce) and more; VS Code also accepts OSC 133 A–D — https://code.visualstudio.com/docs/terminal/shell-integration
