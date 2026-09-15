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
| **Reading from SSD straight to GPU** (GPUDirect Storage) | ❌ Out | Linux-only, datacenter GPUs (Tesla/Quadro), extra kernel modules |
| **VT parsing, input, rendering** | ❌ Out | Sequential and small (parsing), sub-millisecond already (input), and rendering belongs to D3D/Metal/Vulkan, not CUDA |

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
