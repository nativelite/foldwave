# GPU compute (CUDA and alternatives): verified facts and design

Founder input (2026-09-15): CUDA utilization could help. This file records where
it does, where it doesn't, and what the zero-dependency rule allows. Every fact
is from a primary source fetched 2026-09-15, or measured locally and labelled
so. **Unverified** marks anything that couldn't be confirmed.

## Decision summary (proposed)

| Use | Verdict | Why |
| --- | --- | --- |
| **Local AI over agent output** (summarize floods, cluster errors, semantic search) | ✅ Best fit | Real GPU work, and it can run in a **separate local process** (llama.cpp's `llama-server`, Ollama: both MIT, both HTTP), so the terminal links no CUDA and stays zero-dependency |
| **Bulk search over huge history** (scan GBs when the index can't answer) | 🟡 Spike, measure first | Published GPU regex speedups are large, but exclude transfer time, and transfer is what decides small searches |
| **Compressing cold log segments** | 🟡 Probably CPU | nvCOMP (GPU compression) is proprietary; measure whether CPU compression is enough first |
| **Reading from SSD straight to GPU** (GPUDirect Storage, DirectStorage) | ❌ Out | GPUDirect Storage: Linux-only, datacenter GPUs. DirectStorage: not peer-to-peer (it stages through system RAM), read-only, and a proprietary redistributable. Our own `ReadFile` plus a GPU copy takes the same two hops with nothing to ship (below) |
| **VT tokenizing** | 🟡 Candidate (2026-09-21, E4) | Measured to fit: non-string start states converge within 18 bytes, and a 1 MiB pinned copy costs 0.6 % of parsing it on a P-core ([design §8.0 E4 result](design.md)). A kernel must still beat the CPU end to end |
| **Screen apply, input, rendering** | ❌ Out | Screen apply is sequential per pane, input is already sub-millisecond, and rendering belongs to D3D/Metal/Vulkan, not CUDA |

**Design rule:** CUDA is NVIDIA-only. Treat it as **one optional compute
backend** beside the CPU, detected at runtime, never required. Everything must
work on AMD, Intel and Apple GPUs and with no GPU.

## Local machine (measured)

- The founder's machine has an **NVIDIA GeForce RTX 3070 with 8 GB** of video memory, driver 616.92 (`nvidia-smi`).
- `C:\Windows\System32\nvcuda.dll` is present, and the display-driver package also contains `nvptxJitCompiler64.dll`.
- No CUDA Toolkit is installed: no `CUDA_PATH`, no `nvrtc*.dll`.
- **Hybrid graphics, confirmed 2026-09-15:** an Intel integrated GPU runs alongside the 3070 (`IntelGraphicsSoftware` resident). The Optimus caveat below — D3D starting before CUDA can put rendering on the integrated GPU and break interop — applies to the reference machine itself, not only to laptops.
- **Idle snapshot (2026-09-15, measured):** `nvidia-smi` reported **0% GPU utilization, 1975 MiB of 8192 MiB used**, while the CPU carried two runaway `find` processes at 6.8 CPU-hours each ([design §8.7b](design.md)).
- **Trust `nvidia-smi`, not Task Manager,** for utilization: Task Manager's GPU figure defaults to the 3D engine and under-reports compute.
- **Shader toolchain (measured 2026-09-15, `C:\Windows\System32`):** `vulkan-1.dll` and `d3dcompiler_47.dll` are present; `dxcompiler.dll` is not. So D3D11 can compile HLSL at runtime from a system DLL, while a Vulkan backend needs SPIR-V compiled ahead of time and checked in — a build-time step, not a runtime dependency. Recorded for the M3 renderer decision ([design §8.6](design.md)); it does not change the Vulkan-first recommendation.

- **Host↔GPU copies (measured 2026-09-21, `fwbench gpu`):**
  - Link: RTX 3070 at PCIe 4.0 x16 (`nvidia-smi`).
  - Setup: CUDA Driver API through `crates/fwcuda`, synchronous copies, 2 s warm-up, 200 copies per cell, two runs agreeing within 4 %.
  - Pinned host memory: 18–20 GiB/s at 1 MiB, 21.6–23.5 GiB/s at 16–64 MiB, and 13 µs p50 for 64 KiB.
  - Pageable memory: 7.2–10.7 GiB/s.
  - Without a warm-up, a 20-copy smoke run swung between 10.9 and 18.5 GiB/s, so warm the GPU's clocks before measuring.
- **Driver API signature change:** CUDA 13 docs give `cuCtxCreate` a new four-argument form, `(CUcontext*, CUctxCreateParams*, flags, CUdevice)` — https://docs.nvidia.com/cuda/cuda-driver-api/cuda_driver_api/group__CUDA__CTX.html
  - The three-argument `cuCtxCreate_v2` is documented in the 12.6 archive (https://docs.nvidia.com/cuda/archive/12.6.0/cuda-driver-api/group__CUDA__CTX.html) and is still exported by driver 616.92, which reports CUDA 13040. `fwcuda` uses it.

## CUDA without the toolkit: the Driver API

- **The driver ships the CUDA runtime.** On Linux, the display driver ships the user-mode CUDA driver (`libcuda.so`); the Toolkit ships cudart and the other libraries — https://docs.nvidia.com/deploy/cuda-compatibility/why-cuda-compatibility.html
- **The Windows equivalent:** NVIDIA's NVRTC guide names the driver library "`libcuda.so.1`/`nvcuda.dll`" — https://docs.nvidia.com/cuda/nvrtc/index.html
  - No NVIDIA doc lists `nvcuda.dll` as a driver file (**unverified** from docs). It is present locally with no toolkit (above).
- **Kernels can be loaded at runtime** through the Driver API: `cuModuleLoadData`, `cuModuleLoadDataEx`, `cuModuleLoadFatBinary`, with errors for invalid or unsupported PTX versions — https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MODULE.html
- **So precompiled PTX or fatbin kernels need only the driver.** The driver package includes the PTX JIT compiler. This is inferred from the EULA's driver library list and the local files, not stated verbatim by NVIDIA.
  - Caveat: a newer NVRTC may emit PTX an older driver rejects — https://docs.nvidia.com/cuda/nvrtc/index.html
  - So pin the PTX version or ship a fatbin.
- **The Driver API controls contexts and modules explicitly;** the runtime API manages them implicitly — https://docs.nvidia.com/cuda/cuda-driver-api/driver-vs-runtime-api.html
- **Implication for nativelite:** our own `extern` FFI to `nvcuda.dll` / `libcuda.so` with prebuilt kernels means nothing to redistribute. It works wherever the NVIDIA driver is installed.

## Redistribution and licensing

- **NVRTC is part of the CUDA Toolkit,** not the driver — https://docs.nvidia.com/cuda/nvrtc/index.html
- **The CUDA EULA's Attachment A** (updated 2026-01-26) lists what may be redistributed — https://docs.nvidia.com/cuda/eula/index.html
  - `cudart`
  - `nvrtc` and `nvrtc-builtins`
  - `nvJitLink`
  - on Linux, `libcuda.so` and `libnvidia-ptxjitcompiler.so`
- **Its conditions:**
  - the app must add "material additional functionality"
  - the SDK must not be used "in any manner that would cause it to become subject to an open source software license"
  - **Implication:** bundling NVIDIA runtime files with an MIT or FSL release is legally awkward. Using the user's installed driver avoids it.
- **nvCOMP is proprietary** (NVIDIA SDK license); binaries may be redistributed inside an application under the same open-source restriction — https://docs.nvidia.com/cuda/nvcomp/license.html
  - Formats: LZ4, Snappy, Deflate, Gzip, Zstandard, GDeflate, Bitcomp, gANS — https://docs.nvidia.com/cuda/nvcomp/
  - Windows ≥ 10 supported — https://docs.nvidia.com/cuda/nvcomp/installation.html

## GPUDirect Storage (reading from SSD straight to the GPU)

- Install and troubleshooting cover Ubuntu, RHEL and DGX OS only (no Windows) — https://docs.nvidia.com/gpudirect-storage/troubleshooting-guide/index.html
- GPUs: Tesla/Quadro, compute capability ≥ 6; later releases drop Pascal and Volta — same guide, and https://docs.nvidia.com/gpudirect-storage/release-notes/index.html
- Needs the `nvidia-fs` kernel module and recent kernels for NVMe P2PDMA — release notes.
- On Btrfs and ZFS it falls back to CPU reads (compatibility mode) — release notes.
- **Verdict:** not for a desktop terminal.

## DirectStorage (Windows' SSD-to-GPU loading API)

*Researched 2026-09-21.*

- **Not peer-to-peer.** With GPU decompression, "the first copy queue is used to copy the compressed stream from system memory to the GPU" — https://devblogs.microsoft.com/directx/directstorage-1-1-now-available/
- **What BypassIO shortens is the path to RAM, not the path to the GPU.**
  - Reads go I/O manager → NTFS → classpnp → StorNVMe, skipping the filter stacks — https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/bypassio
  - It needs NVMe, NTFS and noncached reads.
  - A minifilter that has not opted in (wof.sys, for example) forces the normal path.
  - **NTFS compression cannot be enabled on a BypassIO-active file** — same page. This is one more reason to keep the log directory uncompressed.
- **Read-only.** DirectStorage 1.3 has no write request, and BypassIO lists noncached writes as "coming in a future release" (same page; the 1.3 `dstorage.h` was inspected from the NuGet package).
- **Not part of Windows.** `dstorage.dll` and `dstoragecore.dll` ship in NuGet `Microsoft.Direct3D.DirectStorage` under the proprietary Microsoft DirectStorage SDK license, and the app must redistribute them — https://www.nuget.org/packages/Microsoft.Direct3D.DirectStorage
  - They are absent from System32 on the reference machine. It "ships with your game" — https://devblogs.microsoft.com/directx/directstorage-developer-preview-now-available/
  - **This fails the zero-dependency rule.**
- **GDeflate is open.** It is DEFLATE rearranged into 64 KiB tiles so 32 lanes can decode in parallel, with Apache-2.0 compressor, decompressor and shader code — https://github.com/microsoft/DirectStorage/tree/main/GDeflate
  - It is the reference to study for our own GPU compression of cold log segments. Throughput claims are **unverified**: NVIDIA's figures are in images.
- **Implication:** our own path is plain `ReadFile` into pinned memory, then a GPU copy through the Driver API or a Vulkan transfer queue. It is the same two hops DirectStorage takes, with nothing to redistribute. E: reads plus 22 GiB/s copies (above) put the ceiling at the drive.

## Parallel tokenizing on a GPU (prior work)

- **Data-parallel finite-state machines.** Enumerate every start state per chunk and combine chunks with a parallel prefix over transition functions. Reported up to 3× on one core and 21× on 8 cores for regex, Huffman and HTML tokenization — Mytkowicz, Musuvathi, Schulte, ASPLOS 2014, https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/asplos302-mytkowicz.pdf
- **GPU structural indexing for JSON (GpJSON).** At least 2.9× faster end to end than the best parallel parsers on an A100 — PVLDB 18(9), 2025, https://vldb.org/pvldb/vol18/p3216-bonetta.pdf
- **The VT500 parser has 14 states** — https://vt100.net/emu/dec_ansi_parser
  - `nativelite-ansi` uses 6, which is 11 once pending-ESC and UTF-8 states are counted ([`fsm.rs`](../crates/fwbench/src/fsm.rs)).
  - No published GPU VT parser was found (**unverified** absence).

## GPU text and regex search

- **BitGen** (MICRO 2025), geometric-mean speedups — https://dl.acm.org/doi/10.1145/3725843.3756052

  | Versus | Speedup |
  | --- | --- |
  | single-thread Hyperscan | 3.0× |
  | multi-thread Hyperscan | 1.7× |
  | icgrep | 25.3× |

  - Hardware: RTX 3090 (also H100 NVL, L40S), against a 32-core Xeon Platinum 8562Y+.
  - Kernels compiled with NVRTC.
  - **Compilation and data transfer time are excluded.**
  - Absolute MB/s: **unverified** (the table couldn't be extracted).
- **HybridSA** (OOPSLA 2024): 4–60× over the best CPU engine and 4–233× over the best GPU engine; hardware **unverified** — https://dl.acm.org/doi/10.1145/3689771
- **Transfer cost decides small inputs:**
  - counting memory transfer, kernels took 2–50× longer than GPU compute alone (Gregg & Hazelwood, ISPASS 2011, abstract only) — https://ieeexplore.ieee.org/document/5762730/
  - NVIDIA advises batching transfers and pinned memory — https://developer.nvidia.com/blog/how-optimize-data-transfers-cuda-cc/
- **Design consequence:**
  - The index ([design §8.5](design.md)) answers most queries on the CPU in milliseconds.
  - GPU scanning is a fallback for unindexed, huge, batched scans only.
  - It is kept only if an end-to-end benchmark, transfer included, beats a multithreaded CPU scan on the reference machine.

## Local AI over agent output

| Engine | License | How the terminal talks to it | Source |
| --- | --- | --- | --- |
| llama.cpp `llama-server` | MIT | HTTP on `127.0.0.1:8080`, OpenAI-compatible chat and embeddings routes; CUDA via build flag `-DGGML_CUDA=ON` | https://github.com/ggml-org/llama.cpp/blob/master/LICENSE, https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md, https://github.com/ggml-org/llama.cpp/blob/master/docs/build.md |
| Ollama | MIT | HTTP API on `localhost:11434` | https://github.com/ollama/ollama/blob/main/LICENSE, https://github.com/ollama/ollama/blob/main/docs/api.md |
| ONNX Runtime | MIT | In-process library (CUDA and DirectML execution providers); no official server found | https://github.com/microsoft/onnxruntime/blob/main/LICENSE, https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html |

**Design:**
- The terminal talks HTTP to a local model server the user runs, over our own zero-dependency HTTP client (std sockets). It links no CUDA.
- This matches the product line's process-boundary policy ([agent-terminal-suite.md](https://github.com/nativelite/ops/blob/main/roadmap/agent-terminal-suite.md)).
- The server may use CUDA, ROCm, Metal or the CPU; the terminal doesn't care.

**Local AI features:**
- summarize a flood
- cluster repeated errors across panes
- embeddings for semantic search over the log
- explain a failing block

**Caveat:** an 8 GB card limits model size; running inference while the renderer uses the same GPU needs the governor ([design §8.8](design.md)) to share it.

## Vendor-neutral compute

| Option | Status | Source |
| --- | --- | --- |
| DirectML | Maintenance mode: no new features, security fixes only; Windows ML is the successor (Windows 11 24H2+) | https://github.com/microsoft/DirectML, https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/overview |
| Vulkan compute | Compute pipelines are core spec | https://docs.vulkan.org/spec/latest/chapters/pipelines.html |
| Metal compute | Apple-documented GPU compute on macOS | https://developer.apple.com/documentation/metal/performing-calculations-on-a-gpu |
| WebGPU | W3C Candidate Recommendation Draft, 2026-09-01 | https://www.w3.org/TR/webgpu/ |
| wgpu | Vulkan/Metal/D3D12/GL, but a crate (conflicts with zero-dependency) | https://github.com/gfx-rs/wgpu |
| AMD ROCm on Windows | Subset only; "the entire ROCm stack is not yet supported on Windows" (2026-01-21) | https://rocm.docs.amd.com/projects/install-on-windows/en/latest/conceptual/component-support.html, https://www.amd.com/en/resources/support-articles/release-notes/RN-AMDGPU-WINDOWS-PYTORCH-7-2.html |

Implication: for GPU scanning, one backend per platform, just as for rendering:

| Platform | Backend |
| --- | --- |
| NVIDIA | CUDA Driver API |
| AMD / Intel | Vulkan compute |
| macOS | Metal |
| everything | CPU fallback |

AI stays out of process, so it needs none of these.

## Sharing one GPU between compute and rendering (Windows)

- **CUDA can share buffers with Direct3D 11** through the Driver API (`cuD3D11GetDevice`, `cuGraphicsD3D11RegisterResource`) — https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__D3D11.html
  - Registering a buffer is "potentially high-overhead and should not be called every frame".
  - The primary render target can't be shared.
- **Hybrid laptops:** if D3D starts before CUDA, rendering may land on the integrated GPU and interop breaks — https://docs.nvidia.com/cuda/archive/12.4.1/optimus-developer-guide/index.html
- **Display GPUs run under WDDM;** compute-only (TCC) mode can't drive a display and isn't available on GeForce — https://docs.nvidia.com/cuda/cuda-installation-guide-microsoft-windows/index.html
- **Timeout Detection and Recovery (TDR)** is Windows' guard against a frozen
  screen. It is about a *single* GPU task, not total work time:
  - The GPU scheduler "detects when the GPU takes more than the permitted amount
    of time to execute a particular task" and tries to preempt it.
  - "If the GPU can't complete or preempt the current task within the TDR
    timeout period", 2 s by default, "the OS diagnoses that the GPU is frozen".
  - The driver then resets the GPU so the desktop doesn't hang, and five or
    more hangs within a minute bug-check the machine.
  - https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery
- **Why AI and long compute still work on Windows:**
  - A long job, such as LLM inference for minutes or hours, is thousands of
    short GPU tasks, each finishing in milliseconds.
  - Since Windows 8 the GPU scheduler can also preempt work mid-task, at a
    granularity the driver declares for compute and graphics —
    https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-preemption
  - So TDR only bites when **one** task can neither finish nor be paused for 2
    seconds.
  - Apps shouldn't raise `TdrDelay` to avoid it —
    https://learn.microsoft.com/en-us/windows-hardware/drivers/display/tdr-registry-keys
- **What it means for us:** only kernels *we* write for GPU scanning need care:
  - process a bounded amount of data per launch, well under the limit
  - loop in chunks, the way inference engines already do
- **How much compute slows rendering on a shared GPU is unknown.** No official
  guidance was found. We must measure it ourselves: experiment E1 below.

## Experiment E1: GPU contention (to run before any GPU compute decision)

The question: on one GPU driving the display (the founder's RTX 3070), how much
does AI inference or GPU scanning cost the terminal's frames and input latency,
and what should the governor do about it?

| Step | Measure |
| --- | --- |
| Baseline | A GPU-rendered terminal under a flood, with nothing else on the GPU. Frame time p50/p99 and dropped frames with PresentMon (Intel, MIT license, https://github.com/GameTechDev/PresentMon), plus key echo latency from our harness. Before our renderer exists, Windows Terminal (AtlasEngine, D3D) stands in. |
| + inference | The same, while `llama-server` (CUDA build) streams tokens from a model that fits in 8 GB. Also record tokens/s. |
| + scan | The same, while a CUDA text-scan kernel runs, at several data sizes per launch. |
| + memory pressure | Repeat with the model filling most of the 8 GB, to see what VRAM contention does to the renderer's atlas and buffers. |
| Governor trial | Throttle inference (fewer parallel requests, smaller batches) when frame p99 exceeds the target; record the tokens/s traded for frames. |

Output: a table on the reference machine, and a governor rule backed by those
numbers (for example "when the focused pane's frame p99 passes X ms, cap
inference to N requests"). Until E1 runs, the design assumes nothing about
contention.

**E1 setup (2026-09-24).**

- **Renderer:** fwterm's own Vulkan renderer now exists, so it replaces
  Windows Terminal as the baseline. Frame gaps come from `fwterm --stats`; the
  window must sit on the monitor the RTX 3070 drives (the reference machine has
  two).
- **Inference:** llama.cpp `llama-server` build b11160, CUDA 13.4 x64 (MIT,
  https://github.com/ggml-org/llama.cpp/releases/tag/b11160), in
  `E:\fwbench-e1\llama`, with Qwen2.5-7B-Instruct Q4_K_M (4.68 GB, Apache-2.0)
  read in place from the founder's Ollama store. Smoke test: 77 tokens/s,
  5.8 GB of 8 GB VRAM in use.
- **Scan:** `fwbench gpuscan` runs `fwcuda::ByteCounter`, a byte-count kernel
  written in PTX and JIT-compiled by the driver (nothing from the CUDA toolkit
  is built against or shipped). Every result is checked against the CPU's
  count. Alone on the RTX 3070:

  | Data per launch | Scanned | Launch p50 / p99 |
  | --- | --- | --- |
  | 1 MiB | 20.7 GiB/s | 0.04 / 0.09 ms |
  | 32 MiB | 150.2 GiB/s | 0.20 / 0.38 ms |
  | 256 MiB | 332.1 GiB/s (~80 % of the card's memory bandwidth) | 0.74 / 1.20 ms |
  | 32 MiB, uploaded from pinned memory each launch | 19.4 GiB/s (PCIe-bound) | 1.59 / 1.82 ms |

  Every launch is far below the 2 s TDR limit.
- **Frame timing from outside:** PresentMon 2.6.0 (MIT) is in
  `E:\fwbench-e1\presentmon`. It needs administrator rights or membership in
  the "Performance Log Users" group to start a trace; not granted yet.

**E1 result (measured 2026-09-24, local 164 Hz display, both monitors on the
RTX 3070, quiet machine).** Scripts: `E:\fwbench-e1\e1.ps1`, `e1b.ps1`.

- **Frames:** fwterm, Vulkan, one pane, 1600x900, pipe lane, 3 GiB of
  `fwbench --emit`, `--no-log --governor off`, 3 runs per condition, from
  `fwterm --stats`. PresentMon could not run (no trace permission), so these
  are fwterm's own frame gaps, not presents seen by the compositor.
- **Key echo:** `fwprobe latency`, 300 samples per condition, cmd under the
  bundled OpenConsole. One screen read costs 6.0 ms, which is also the
  resolution of every sample.

| Load on the GPU | GPU busy, VRAM | Frames: fps, gap p99 / max | Flood span | Key echo p50 / p90 / p99 | The load's own rate |
| --- | --- | --- | --- | --- | --- |
| none | 0–3 %, 1.2 GB | 149–152, 6.99–7.42 / 7.20–8.13 ms | 11.5–12.0 s | 6.16 / 9.62 / 13.51 ms | — |
| inference, 1 stream (Qwen2.5-7B Q4_K_M) | 96 %, 5.8 GB | 151, 7.01–7.05 / 7.26–7.68 ms | 11.8 s | — | 70.6 tok/s (77 alone) |
| inference, 4 streams | 93 %, 6.4 GB | 151–152, 7.01–7.06 / 7.19–7.55 ms | 11.9 s | 5.40 / 8.25 / 12.03 ms | 236 tok/s |
| inference, 4 streams, VRAM nearly full (32k context) | 92 %, 7.3 GB | 152, 7.01–7.04 / 7.19–7.48 ms | 11.9–12.0 s | 5.43 / 8.45 / 13.75 ms | 235 tok/s |
| scan, 1 MiB per launch | 52 %, 1.2 GB | 125–129, 13.57–14.93 / 15.76–18.63 ms | 27.0–30.7 s | 8.44 / 14.29 / 25.32 ms | 21.5 GiB/s, 22,000 launches/s |
| scan, 4 MiB per launch | — | 151, 7.12 / 7.45 ms | 15.4 s | — | 45.6 GiB/s, 11,700 launches/s |
| scan, 32 MiB per launch | 91 %, 1.2 GB | 152–153, 6.95–7.02 / 7.13–7.54 ms | 12.5–12.7 s | — | 146 GiB/s |
| scan, 256 MiB per launch | 97 %, 1.5 GB | 152–153, 6.95–6.98 / 7.16–7.32 ms | 11.9 s | 5.33 / 8.01 / 13.40 ms | 325 GiB/s |

What it says:

- **LLM inference does not cost the terminal anything measurable on this
  machine.** With the GPU 92–96 % busy generating tokens, and even with 7.3 GB
  of its 8 GB taken, frames stayed at one refresh (p99 7.0 ms against the
  6.1 ms refresh) and key echo stayed at idle's numbers or better. Inference
  kept ~92 % of its solo speed (70.6 against 77 tok/s). Local AI (§8.7a) needs
  no throttling rule for the renderer's sake.
- **Bulk compute is harmless in large launches and harmful in small ones.**
  At 32 or 256 MiB per launch the GPU was 91–97 % busy and frames did not move.
  At 1 MiB per launch, about 22,000 synchronous launches a second, frame p99
  doubled (to 13.6–14.9 ms), key echo p99 almost doubled (25.3 ms), and the
  flood through fwterm took 2.3× as long. At 4 MiB per launch (11,700 a second)
  the harm was gone from frames, with the flood span only slightly longer. The
  scanner's CPU was one busy core in every case (43.8–45.2 s over 45 s), so the
  cost is the GPU switching between compute and graphics on every launch, not
  CPU.
- **The governor rule this gives:** GPU compute runs in batches of at least
  4 MiB and at most ~10,000 launches a second. There is no rule for inference,
  and no VRAM-pressure rule at up to 7.3 of 8 GB.
- **Key echo:** 300 samples each, none missed. The idle p99 of 13.5 ms is
  instrument-limited (6.0 ms per screen read, plus the shell); §4's
  p99 < 5 ms still needs a finer instrument. The founder's bar, parity with
  Windows Terminal, is met: on the same display, fwterm measured 7.08 / 12.48 ms
  (p50 / p99, 30 samples) against Windows Terminal's 9.34 / 14.45 ms
  (20 samples).
- **Not covered:** PresentMon's view (permission), and other GPUs. This is one
  RTX 3070 on one driver (616.92).

**GPU indexer trial (2026-09-24; design §8.7a).** `fwbench gpuindex` and
`gpuindex-load`: panelog's search signatures built on the GPU by
`fwcuda::TrigramSigs` (PTX, JIT-compiled by the driver), equal bit for bit to
panelog's own. At 64 MiB per launch: 13.8 GiB/s with transfers, 2.2× all 16
CPU threads, at about 1/30 of their CPU time, and fwterm's frames unchanged
beside it. Numbers are in design §8.7a. This follows E1's rule: whole batches
of at least 4 MiB.
