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

## OS mechanisms for placing work (researched 2026-11-16)

Input to [`philosophy.md`](philosophy.md). The question: beyond "which device",
what does each operating system offer for saying *how cheaply* a piece of work
should run, for giving memory back, and for getting a frame to the display
sooner? Every row says whether our own FFI to a library the OS ships reaches it.

**How these were checked.** The session that researched this could reach only
GitHub, so Microsoft Learn pages were read from the `MicrosoftDocs` repositories
they are built from, and Khronos pages from `KhronosGroup/Vulkan-Docs`. The URL
given is the one fetched. **Unverified** marks a fact seen only in a search
summary. Nothing in this section has been measured on the reference machine;
the experiments that would are in [`philosophy.md`](philosophy.md).

### Windows

**Efficiency classes (all `kernel32.dll`).**

| Mechanism | What the documentation says | Fit for foldwave | Source |
| --- | --- | --- | --- |
| EcoQoS, per thread: `SetThreadInformation(ThreadPowerThrottling)` with `THREAD_POWER_THROTTLING_EXECUTION_SPEED` | Eco "always selects most efficient CPU frequency and schedules to efficient cores" (Windows 11) | indexer, compressor, catch-up replay and retention threads: E-cores and low clocks without pinning | https://raw.githubusercontent.com/MicrosoftDocs/win32/docs/desktop-src/ProcThread/quality-of-service.md, https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/processthreadsapi/nf-processthreadsapi-setthreadinformation.md |
| EcoQoS, per process: `SetProcessInformation(ProcessPowerThrottling)` | for work "not contributing to the foreground user experience"; before Windows 11 it maps to LowQoS | a hidden pane's host | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/processthreadsapi/nf-processthreadsapi-setprocessinformation.md |
| QoS follows the window | processes that own a window "or are descendants of window owning processes" get High in focus, Medium when visible, Low when minimized or fully occluded | every agent under fwterm inherits fwterm's window state: a minimized foldwave may slow its agents. Whether this crosses a ConPTY or job boundary is **unverified** and worth measuring | quality-of-service.md above |
| Background mode: `SetThreadPriority(THREAD_MODE_BACKGROUND_BEGIN)` | lowers the thread's "resource scheduling priorities"; such a thread should avoid shared locks, heaps and handles (priority inversion) | an alternative to EcoQoS for threads that also do disk I/O | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/processthreadsapi/nf-processthreadsapi-setthreadpriority.md |
| I/O priority per handle: `SetFileInformationByHandle(FileIoPriorityHintInfo)`, `IoPriorityHintVeryLow` | "the system uses this value for background I/O"; a hint the driver may ignore | retention deletes, signature rebuilds, cold-segment compression | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/winbase/ns-winbase-file_io_priority_hint_info.md |
| Timer resolution | from Windows 11, a fully occluded or minimized window-owning process is not guaranteed better than the default resolution; `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` controls it | frame pacing uses a timer (§11 row 4); behaviour while occluded is untested | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/timeapi/nf-timeapi-timebeginperiod.md |

- Microsoft's own energy claim for EcoQoS, "up to 90 % reduction in CPU power
  consumption", is **unverified** here (search summary of
  https://devblogs.microsoft.com/sustainable-software/introducing-ecoqos/).
- Design §8.8 measured that *pinning* threads to P-cores did not help. EcoQoS is
  the opposite request (keep this work off the fast cores) and is untested.

**Memory give-back (all `kernel32.dll`).**

| Mechanism | Contract | Fit | Source |
| --- | --- | --- | --- |
| `OfferVirtualMemory` / `ReclaimVirtualMemory` | offered pages leave the working set and are never written to the pagefile; reclaim says whether the system discarded them. "Only… if the application does not need or can regenerate the data" | a shed pane's frozen emulator and history, which the log rebuilds (§8.8) | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/memoryapi/nf-memoryapi-offervirtualmemory.md |
| `DiscardVirtualMemory` | contents become undefined, the region stays committed | freed history chunks | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/memoryapi/nf-memoryapi-discardvirtualmemory.md |
| Memory priority (`ProcessMemoryPriority`, `ThreadMemoryPriority`, 1–5) | "pages with lower memory priority are trimmed before pages with higher" | hidden panes' hosts, the indexer | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/processthreadsapi/ns-processthreadsapi-memory_priority_information.md |
| `CreateMemoryResourceNotification` | a waitable handle that signals low memory | the governor's memory input, with no polling | https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/memoryapi/nf-memoryapi-creatememoryresourcenotification.md |
| Large pages (`MEM_LARGE_PAGES`) | needs `SeLockMemoryPrivilege`; nonpageable and "not subject to job limits" | **out**: it cannot be given back and escapes the pane host's job | https://raw.githubusercontent.com/MicrosoftDocs/win32/docs/desktop-src/Memory/large-page-support.md |

**Storage.**

- **Dev Drive.** A ReFS volume on which Defender scans asynchronously once the
  drive is trusted ("performance mode"). 50 GB minimum; an existing volume
  cannot be converted; not C: —
  https://raw.githubusercontent.com/MicrosoftDocs/windows-dev-docs/docs/hub/dev-drive/index.md
  - An application can detect one: `FSCTL_QUERY_PERSISTENT_VOLUME_STATE`
    returns `PERSISTENT_VOLUME_STATE_DEV_VOLUME` (0x2000) and
    `PERSISTENT_VOLUME_STATE_TRUSTED_VOLUME` (0x4000), through
    `DeviceIoControl` in `kernel32.dll` —
    https://raw.githubusercontent.com/MicrosoftDocs/windows-driver-docs-ddi/staging/wdk-ddi-src/content/ntifs/ns-ntifs-_file_fs_persistent_volume_information.md
  - Microsoft's "up to 30 %" faster builds is **unverified** (search summary of
    https://blogs.windows.com/windowsdeveloper/2023/06/01/dev-drive-performance-security-and-control-for-developers/).
  - Why it matters: E3 ([design §8.7b](design.md)) measured Defender at ~14 %
    of the machine during an 8-agent session. Those are the user's worktrees,
    not foldwave's files, so foldwave can report it, not change it.
  - Putting *logs* on ReFS would change the measured basis: E6 and the
    write-through figures are NTFS.
- **IoRing** (`CreateIoRing`, `BuildIoRingReadFile`; Windows 11 build 22000;
  `kernel32.dll`): the fetched documentation lists read, register and cancel
  operations only. Write and flush operations in later builds are
  **unverified**. A candidate for batched reads in search and replay, not for
  the writer, which keeps one write in flight by design (§4.1) —
  https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/ioringapi/nf-ioringapi-createioring.md
- **`SetFileValidData`: out.** Needs an administrator privilege, and the page
  itself says there is usually no gain outside random writes —
  https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/fileapi/nf-fileapi-setfilevaliddata.md
- **Hole punching** (`FSCTL_SET_SPARSE`, `FSCTL_SET_ZERO_DATA`): buys nothing
  while generations are deleted whole —
  https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/winioctl/ni-winioctl-fsctl_set_zero_data.md

**Display path.**

- Microsoft's ordering of windowed latency, lowest first: a tearing present on
  a multi-plane overlay, then a waitable swap chain at latency 1 in independent
  flip, then composed flip. The size of each step is not published —
  https://raw.githubusercontent.com/MicrosoftDocs/win32/docs/desktop-src/direct3ddxgi/for-best-performance--use-dxgi-flip-model.md
- The waitable object also "ensures that the application will only render
  after each VSync, minimizing power consumption" —
  https://raw.githubusercontent.com/MicrosoftDocs/windows-dev-docs/docs/uwp/gaming/reduce-latency-with-dxgi-1-3-swap-chains.md
- **Which path does fwterm's Vulkan swapchain get?** NVIDIA's "Vulkan/OpenGL
  present method" setting chooses between a native (blt-style) surface and one
  layered on a DXGI swapchain (flip). It is a driver setting, not a Vulkan
  call, and this description is **unverified** (search summary of
  https://community.khronos.org/t/vulkan-present-mode-preferred-native-vs-layered-on-dxgi-swapchain/112055).
  It may explain why FIFO did not pace to the display (§11 row 4); that is a
  hypothesis. PresentMon's `PresentMode` column tells the modes apart —
  https://raw.githubusercontent.com/GameTechDev/PresentMon/main/README-ConsoleApplication.md
- Vulkan extensions, all through `vulkan-1.dll` and all driver-dependent, to be
  queried at runtime:

  | Extension | Gives | Source |
  | --- | --- | --- |
  | `VK_KHR_swapchain_maintenance1` | switch present mode per present without recreating the swapchain (FIFO when idle, mailbox or immediate while typing); deferred image allocation | https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_KHR_swapchain_maintenance1.adoc |
  | `VK_KHR_present_wait2` | block until a present has taken effect, bounding queued frames | https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_KHR_present_wait2.adoc |
  | `VK_EXT_present_timing` | actual present times, and "no earlier than" scheduling. Driver support on 616.92 unchecked | https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_EXT_present_timing.adoc |
  | `VK_NV_low_latency2` | the Reflex sleep-before-input mechanism with no SDK; NVIDIA only | https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_NV_low_latency2.adoc |

- **NVIDIA Reflex SDK: out.** Proprietary, object-form redistribution only, and
  its licence forbids use that would put it under an open-source licence —
  https://raw.githubusercontent.com/NVIDIA-RTX/Reflex/main/license.txt

**Two GPUs.**

- The reference desktop is not a WDDM "hybrid system": that term needs the
  panel on the integrated GPU. With both monitors on the RTX 3070, drawing on
  the Intel GPU would add a cross-adapter copy per present —
  https://raw.githubusercontent.com/MicrosoftDocs/windows-driver-docs/staging/windows-driver-docs-pr/display/using-cross-adapter-resources-in-a-hybrid-system.md
- E1 above already found inference costs the renderer nothing on the shared
  card. So "draw on the integrated GPU, keep the discrete one for compute" has
  no case on this machine. On a laptop whose panel is on the integrated GPU the
  integrated GPU is the right place to draw: the discrete one can stay asleep.
- A Vulkan application chooses its `VkPhysicalDevice` itself; the loader only
  sorts the list by system preference —
  https://raw.githubusercontent.com/KhronosGroup/Vulkan-Loader/main/docs/LoaderDriverInterface.md

**System inference surfaces.** None ships in-box as a plain DLL. Windows ML and
the Windows AI APIs (Phi Silica) come with the Windows App SDK —
https://raw.githubusercontent.com/MicrosoftDocs/windows-ai-docs/docs/docs/new-windows-ml/distributing-your-app.md.
Foundry Local is a user-installed server with an OpenAI-compatible HTTP
endpoint that picks NPU, GPU or CPU itself, so it is a third server beside
`llama-server` and Ollama in the table above —
https://raw.githubusercontent.com/microsoft/Foundry-Local/main/README.md

### CPU vector instructions and compression

- `std::arch` x86 intrinsics and `is_x86_feature_detected!` are stable Rust
  (AVX2 since 1.27, AVX-512 since 1.89), so a vector path needs no dependency —
  https://raw.githubusercontent.com/rust-lang/rust/master/RELEASES.md
- Published single-core figures: UTF-8 validation at 13 GB/s (simdjson,
  https://raw.githubusercontent.com/simdjson/simdjson/master/README.md);
  `memcpy` at 13.7 GB/s on an i7-9700K (lzbench, in
  https://raw.githubusercontent.com/lz4/lz4/dev/README.md). So design §8.7b's
  "CPU SIMD does it at tens of GB/s" is an all-cores figure; per core it is
  about 10+ GB/s, still ~40× the §4 target.
- Compression, one thread, Silesia corpus, i7-9700K (lzbench; mixed data, not
  log text, and not an E-core):

  | Codec | Ratio | Compress | Decompress | Source |
  | --- | --- | --- | --- | --- |
  | zstd 1.5.7 level 1 | 2.90 | 510 MB/s | 1,550 MB/s | https://raw.githubusercontent.com/facebook/zstd/dev/README.md |
  | LZ4 1.11 | 2.11 | 770 MB/s | 4,950 MB/s | https://raw.githubusercontent.com/lz4/lz4/dev/README.md |

  Both formats are open; under the zero-dependency rule either would be our own
  code, and the LZ4 block format is by far the smaller to write. No open GPU
  compressor with reproducible compress-side numbers was found (absence
  **unverified**), which supports the "probably CPU" verdict above.

### Linux

Read from GitHub copies of the primary sources: the kernel tree
(`torvalds/linux`), a 2021 snapshot of the man pages (`mkerrisk/man-pages`),
`axboe/liburing`, `systemd/systemd`, and mirrors of Mesa and the wayland.app
registry. Everything here is a plain system call or a file under `/proc` or
`/sys` unless a library is named. Kernel versions marked **unverified** are
from memory.

**Transport.** A POSIX pty has no console host in between
([design §8.9](design.md)), but it is not free:

- Each side of a pty gets an 8,192-byte buffer limit, the line discipline's
  read buffer is 4,096 bytes, and every write takes a workqueue hop before the
  reader sees it. A pipe holds 65,536 bytes by default and can be resized
  (`F_SETPIPE_SZ`) —
  https://github.com/torvalds/linux/blob/master/drivers/tty/pty.c,
  https://github.com/torvalds/linux/blob/master/drivers/tty/n_tty.c,
  https://github.com/mkerrisk/man-pages/blob/master/man7/pipe.7
- So the two-lane decision (design §11 row 6: agents over pipes where they
  document it) carries over to Linux. No published pty-against-pipe figure was
  found; it needs `fwbench flood` on Linux.

**Efficiency classes.**

| Mechanism | What the documentation says | Fit | Source |
| --- | --- | --- | --- |
| `SCHED_IDLE`, `SCHED_BATCH` | idle is "lower even than a +19 nice value"; neither needs privilege to enter | derived work: signatures, catch-up, retention, compression | https://github.com/mkerrisk/man-pages/blob/master/man7/sched.7 |
| cgroup v2 `cpu.idle`, `cpu.weight`, `cpu.max` | `cpu.idle = 1` makes a whole cgroup idle-class relative to its peers | a hidden pane's process tree, if the founder allows acting on it | https://github.com/torvalds/linux/blob/master/Documentation/admin-guide/cgroup-v2.rst |
| Utilization clamping (`SCHED_FLAG_UTIL_CLAMP`) | steers frequency only with the schedutil governor | weak on Intel machines running `intel_pstate` (our inference) | https://github.com/torvalds/linux/blob/master/Documentation/scheduler/sched-util-clamp.rst |
| Hybrid CPUs | `/sys/devices/system/cpu/cpuX/cpu_capacity`, and `cpu_core/cpus` and `cpu_atom/cpus` under `/sys/bus/event_source/devices`, tell P-cores from E-cores | `resmon::cpu` on Linux | https://github.com/torvalds/linux/blob/master/tools/perf/Documentation/intel-hybrid.txt |
| I/O priority, `ioprio_set(IOPRIO_CLASS_IDLE)` | unprivileged, but "currently supported by bfq and mq-deadline" only | does nothing on an NVMe drive with scheduler `none`; throttle ourselves or use `io.max` | https://github.com/torvalds/linux/blob/master/Documentation/block/ioprio.rst |
| Timer slack, `prctl(PR_SET_TIMERSLACK)` | default 50 µs; larger slack "can help reduce system power consumption by minimizing CPU wake-ups" | every thread off the input path | https://github.com/mkerrisk/man-pages/blob/master/man2/prctl.2 |

**Pressure, the governor's input.** `/proc/pressure/cpu`, `memory` and `io`
(and the same per cgroup) report the share of time work was stalled. A trigger
written to the file wakes a `poll` when a threshold is crossed, and
"unprivileged users can also create monitors", with a window that is a
multiple of 2 s. This is a direct measure of contention, where Windows gives
only utilization —
https://github.com/torvalds/linux/blob/master/Documentation/accounting/psi.rst

**A job per pane.** There is no single kill-on-close handle. The nearest:

- A delegated cgroup per pane: `clone3` with `CLONE_INTO_CGROUP` starts the
  program inside it, `cgroup.kill` ends the whole tree, `memory.high` and
  `memory.max`, `pids.max` and `cpu.max` bound it, and `cpu.stat`,
  `memory.peak` and `io.stat` account for it (cgroup-v2.rst above).
- Without privilege the route to a delegated cgroup is systemd: a transient
  scope with `Delegate=yes`. The user manager delegates `pids memory cpu` by
  default; `io` and `cpuset` are not delegated —
  https://github.com/systemd/systemd/blob/main/docs/CGROUP_DELEGATION.md,
  https://github.com/systemd/systemd/blob/main/units/user@.service.in
- `PR_SET_CHILD_SUBREAPER` makes the pane host adopt orphans, and a pidfd
  (`pidfd_open`) is a pollable, race-free handle on a process —
  https://github.com/mkerrisk/man-pages/blob/master/man2/prctl.2,
  https://github.com/mkerrisk/man-pages/blob/master/man2/pidfd_open.2
- Landlock lets "any process, including unprivileged ones… securely restrict
  themselves": a pane host can drop what it does not need —
  https://github.com/torvalds/linux/blob/master/Documentation/userspace-api/landlock.rst

**Memory give-back.** `MADV_FREE` (lazy free), `MADV_COLD` and `MADV_PAGEOUT`
(reclaim now); `process_madvise` applies the last two to another process
through a pidfd, so the window can push a hidden pane host's memory out; and a
delegated cgroup's `memory.reclaim` shrinks a pane on request —
https://github.com/mkerrisk/man-pages/blob/master/man2/madvise.2,
https://github.com/mkerrisk/man-pages/blob/master/man2/process_madvise.2

**The log.**

- The equivalent of the Windows writer is `O_DIRECT` with `O_DSYNC`, or
  `pwritev2` with `RWF_DSYNC` per write. `O_DIRECT` alone "does not give the
  guarantees of the O_SYNC flag" —
  https://github.com/mkerrisk/man-pages/blob/master/man2/open.2,
  https://github.com/mkerrisk/man-pages/blob/master/man2/readv.2
- io_uring can append and link a write to its sync, but it must be optional:
  `kernel.io_uring_disabled` can turn it off, and Docker's default seccomp
  profile does not allow it —
  https://github.com/torvalds/linux/blob/master/Documentation/admin-guide/sysctl/kernel.rst,
  https://github.com/moby/profiles/blob/main/seccomp/default.json
- **Btrfs is Linux's NTFS-compression trap.** Its transparent compression
  needs copy-on-write, and direct writes on checksummed files fall back to
  buffered ones on current kernels. A log directory opts out with the `C` (no
  copy-on-write) and `m` (no compression) attributes, set through
  `FS_IOC_SETFLAGS` on the empty directory. The log writer should check, as it
  does for NTFS compression (design §4.1) —
  https://github.com/kdave/btrfs-progs/blob/devel/Documentation/ch-compression.rst,
  https://github.com/kdave/btrfs-progs/blob/devel/Documentation/ch-file-attributes.rst

**Energy.**

- The CPU's energy counters (RAPL) are root-only: the powercap `energy_uj`
  file is created mode 0400, and the perf `power` events count system-wide
  only and need `CAP_PERFMON` at the default `perf_event_paranoid` —
  https://github.com/torvalds/linux/blob/master/drivers/powercap/powercap_sys.c,
  https://github.com/torvalds/linux/blob/master/arch/x86/events/rapl.c
- So joules are a figure the *harness* reports, run with that capability:
  package energy before and after a fixed flood and a fixed idle window. The
  application itself reports proxies: CPU time, and wakeups from
  `voluntary_ctxt_switches` in `/proc/<pid>/status`.
- GPU: NVML's `nvmlDeviceGetTotalEnergyConsumption` through
  `libnvidia-ml.so.1`, which the driver ships; Intel's hwmon `energy1_input` —
  https://github.com/NVIDIA/go-nvml/blob/main/gen/nvml/nvml.h,
  https://github.com/torvalds/linux/blob/master/Documentation/ABI/testing/sysfs-driver-intel-i915-hwmon

**Display path.**

- Mesa's Vulkan on Wayland always offers MAILBOX and FIFO, and IMMEDIATE only
  when the compositor has `tearing-control-v1`; FIFO uses `fifo-v1` and
  `commit-timing-v1` where present. Mesa lists `VK_EXT_present_timing`,
  `VK_KHR_present_wait2` and `VK_KHR_swapchain_maintenance1` for the Intel,
  AMD and open NVIDIA drivers —
  https://github.com/chaotic-cx/mesa-mirror/blob/main/src/vulkan/wsi/wsi_common_wayland.c,
  https://github.com/chaotic-cx/mesa-mirror/blob/main/docs/features.txt
- Compositor support differs: KWin 6.7 has `fifo-v1` and tearing control but
  not commit timing; Mutter 51 has `fifo-v1` and commit timing but not tearing
  control; Sway 1.11 has tearing control only —
  https://github.com/vially/wayland-explorer/tree/main/src/data/compositors
- **A Wayland window needs `libwayland-client`** if it uses Vulkan's own
  surface: `VK_KHR_wayland_surface` takes a `wl_display` and Mesa calls the
  library on it. Every Wayland desktop ships that library, as it ships
  `libvulkan`. The alternative that needs no library, our own wire protocol
  with frames exported as dmabufs, is a prototype, not a plan; the CPU
  renderer over `wl_shm` needs no library at all —
  https://github.com/KhronosGroup/Vulkan-Docs/blob/main/chapters/VK_KHR_surface/wsi.adoc
- foot, the latency reference on Wayland, delays a draw by 0.5 ms after
  output arrives (at most 8.3 ms) so it never draws half an update, and warns
  that its shared-server mode lets one busy window starve the others: the
  trade foldwave's pane hosts already avoid —
  https://github.com/DanteAlighierin/foot/blob/master/doc/foot.ini.5.scd,
  https://github.com/DanteAlighierin/foot/blob/master/README.md

**Two GPUs, and memory the GPU shares.**

- `VK_EXT_physical_device_drm` matches a Vulkan device to its DRM node, so the
  integrated GPU can be chosen on purpose. Opening a device on an idle
  discrete GPU is likely to keep it awake (our inference from the runtime
  power-management documentation), so foldwave should open only the GPU it
  uses —
  https://github.com/KhronosGroup/Vulkan-Docs/blob/main/appendices/VK_EXT_physical_device_drm.adoc,
  https://github.com/torvalds/linux/blob/master/Documentation/power/runtime_pm.rst
- `VK_EXT_external_memory_host` imports host memory as Vulkan memory. Mesa
  lists it for Intel (anv) and AMD (radv); NVIDIA's driver is **unverified**.
  On an integrated GPU a compute pass could then read log pages with no
  staging copy, the Vulkan counterpart of Metal's no-copy buffers below —
  https://github.com/KhronosGroup/Vulkan-Docs/blob/main/appendices/VK_EXT_external_memory_host.adoc

**What loads the session.** There is no always-on scanner by default
(on-access scanning through fanotify needs `CAP_SYS_ADMIN`). The desktops'
file indexers are the nearest thing: GNOME's LocalSearch and KDE's Baloo. No
controlled measurement of their cost on builds was found —
https://github.com/GNOME/localsearch/blob/main/README.md,
https://github.com/KDE/baloo/blob/master/README.md

### macOS

Read from Apple's developer documentation and the XNU sources Apple publishes.
All of it is plain C in `libSystem`, or Metal and AppKit through the
Objective-C runtime (`objc_msgSend`, `sel_registerName`, `objc_getClass`),
which needs no Apple compiler; that reachability is our assessment and is
**unverified** until tried on a Mac.

| Area | Mechanism | What the documentation says | Source |
| --- | --- | --- | --- |
| Efficiency class | QoS classes, `pthread_set_qos_class_self_np` | background "focuses on energy efficiency"; Apple's target is utility or lower "at least 90% of the time when user activity is not occurring" | https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html |
| Efficiency class | QoS and core type | on Apple silicon "the system is more likely to run background tasks on lower performance cores". A hard background-to-E-core rule has no Apple source: **unverified** | https://developer.apple.com/tutorials/data/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon.json |
| Efficiency class | `setpriority(PRIO_DARWIN_BG)` | lowest scheduling priority, and "disk IO is throttled" | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/man/man2/getpriority.2 |
| Hazard | App Nap | a process with no visible window updates is throttled (timers, I/O, priority). A pane host has no window, so it needs an activity token (`NSProcessInfo beginActivityWithOptions:reason:`) while output flows | https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html |
| Idle | timers | "more than one wakeup per second when it should be idle" is Apple's threshold to investigate | https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html |
| Shared memory | Metal, `MTLStorageModeShared` | "Apple GPUs have a unified memory model in which the CPU and the GPU share system memory" | https://developer.apple.com/tutorials/data/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus.json |
| Shared memory | `newBufferWithBytesNoCopy:length:options:deallocator:` | "wraps an existing contiguous memory allocation"; page-aligned, one VM region. So a compute pass over host memory needs no upload | https://developer.apple.com/tutorials/data/documentation/metal/mtldevice/makebuffer(bytesnocopy:length:options:deallocator:).json |
| Shaders | `newLibraryWithSource:options:error:` | compiles Metal source at runtime, so nothing is needed at build time | https://developer.apple.com/tutorials/data/documentation/metal/mtldevice/makelibrary(source:options:).json |
| Display | `CAMetalLayer.displaySyncEnabled`, `presentsWithTransaction` | sync off "presents new content more quickly, but possibly with… screen tearing" | https://developer.apple.com/tutorials/data/documentation/quartzcore/cametallayer/displaysyncenabled.json |
| Display | direct-to-display | needs full screen and Apple silicon; a windowed terminal is always composited | https://developer.apple.com/tutorials/data/documentation/metal/managing-your-game-window-for-metal-in-macos.json |
| Log | `fsync` | flushes to the drive, but "the drive itself may not physically write the data… for quite some time"; use `F_FULLFSYNC` | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/man/man2/fsync.2 |
| Log | `F_FULLFSYNC`, `F_BARRIERFSYNC`, `F_NOCACHE` | full flush; a barrier (ordering without a full flush); caching off | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/man/man2/fcntl.2 |
| Memory | `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` | normal, warn, critical; takes a C function pointer, so no blocks are needed | https://raw.githubusercontent.com/apple/swift-corelibs-libdispatch/main/dispatch/source.h |
| Memory | purgeable memory, `vm_purgable_control` | volatile pages the kernel may discard rather than compress | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/osfmk/mach/vm_purgable.h |
| Energy | `proc_pid_rusage`, `rusage_info_v6` | per process: `ri_energy_nj`, `ri_billed_energy`, `ri_interrupt_wkups`, `ri_pkg_idle_wkups`, CPU time per QoS. Readable for the same user's processes without root | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/sys/resource.h, https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/kern/proc_info.c |
| Process tree | `proc_listchildpids`, `proc_pidinfo`, kqueue `EVFILT_PROC` | accounting and exit events; no `NOTE_TRACK`, so descendants are polled. No job or cgroup analogue was found (**unverified** absence) | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/libsyscall/wrappers/libproc/libproc.h |
| Pty | kernel tty buffers | `TTYHOG` 1024; a report that reads from a pty return at most 1024 bytes is **unverified** (https://debbugs.gnu.org/db/75/75574.html). If true, ingest costs a system call per KiB | https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/sys/tty.h |

- **Vulkan: MoltenVK is now deprecated by Khronos** in favour of KosmicKrisp
  on Apple silicon, and either is a library an application bundles, not one
  macOS ships. Design §8.6's "Metal for macOS" stands —
  https://raw.githubusercontent.com/KhronosGroup/MoltenVK/main/README.md
- **`F_FULLFSYNC` is slow.** Apple's engineers say it is "basically 'always'
  going to be relatively slow" (https://developer.apple.com/forums/thread/800906);
  a community figure of 46 flushes a second on an M1 is **unverified**
  (https://mjtsai.com/blog/2022/02/17/apple-ssd-benchmarks-and-f_fullsync).
  The Windows writer's "durable when the write returns" will not carry over
  unchanged: the durability tier on macOS is a measurement and a decision.
- **The session's own tax on macOS is exec scanning.** Every `exec` is
  scanned (https://developer.apple.com/videos/play/wwdc2019/701/). A terminal
  listed under Privacy & Security > Developer Tools has its child processes
  excluded, per nextest's documentation; Apple's own description was not
  found, and whether the exemption passes through a pane host is
  **unverified** —
  https://raw.githubusercontent.com/nextest-rs/nextest/main/site/src/docs/installation/macos.md

### The BSDs

- **Vulkan is a package, not base.** FreeBSD's `graphics/mesa-dri` port builds
  the Intel and AMD Vulkan drivers by default; OpenBSD and NetBSD have a
  loader port, drivers **unverified**. Strictly, only the CPU renderer is
  zero-dependency there —
  https://raw.githubusercontent.com/freebsd/freebsd-ports/main/graphics/mesa-dri/Makefile
- **A job analogue on FreeBSD:** `procctl(PROC_REAP_ACQUIRE)` adopts every
  descendant, `PROC_REAP_KILL` signals them; it tracks and kills, it does not
  limit —
  https://raw.githubusercontent.com/freebsd/freebsd-src/main/lib/libsys/procctl.2
- **kqueue `NOTE_TRACK`** follows a process across `fork` on FreeBSD and
  OpenBSD (Darwin lacks it) —
  https://raw.githubusercontent.com/freebsd/freebsd-src/main/lib/libsys/kqueue.2
- **No unprivileged efficiency class:** FreeBSD's idle priority needs root by
  default —
  https://raw.githubusercontent.com/freebsd/freebsd-src/main/lib/libsys/rtprio.2
- **ZFS:** with the default `sync=standard`, a synchronous write is flushed to
  stable storage, and dataset compression (lz4, zstd) may already be on, so
  our own compression of cold segments could be doing the work twice —
  https://raw.githubusercontent.com/openzfs/zfs/master/man/man7/zfsprops.7
