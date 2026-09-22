# nativelite-fwcuda

A minimal, runtime-loaded binding to the NVIDIA CUDA **Driver API**, built for
foldwave's M0 harness to measure host-to-GPU copy speed. Zero third-party
dependencies.

`nvcuda.dll` is installed with the NVIDIA display driver. This crate loads it
at run time from System32 (`LoadLibraryExW` + `GetProcAddress`), so nothing
NVIDIA is linked against or redistributed. A machine without the driver gets an
`Error`, not a panic or a link failure.

## Scope

- **Windows x64 only.** On any other target the crate builds, and
  `Cuda::load()` returns `Error::Unsupported`.
- Covers devices, contexts, device memory, pinned host memory and synchronous
  copies. No kernels, streams, or async copies.

## Example

Inside a function that returns `Result<_, fwcuda::Error>`:

```rust,no_run
let cuda = fwcuda::Cuda::load()?;
let dev = cuda.device(0)?;
println!("{} ({} bytes)", dev.name(), dev.total_mem());

let ctx = fwcuda::Context::new(&cuda, &dev)?;
let buf = fwcuda::DeviceBuffer::new(&ctx, 1 << 20)?;
let src = vec![7u8; 1 << 20];
ctx.copy_h2d(&buf, &src)?;

let mut back = fwcuda::PinnedBuffer::new(&ctx, 1 << 20)?;
ctx.copy_d2h(back.as_mut_slice(), &buf)?;
assert_eq!(back.as_slice(), &src[..]);

// 20 timed copies of 64 MiB each way, pinned host memory.
let stats = fwcuda::copy_bench(&ctx, 64 << 20, 20, true)?;
```

## API

| Item | What it does |
| --- | --- |
| `Cuda::load()` | Load `nvcuda.dll`, resolve entry points, `cuInit(0)` |
| `cuda.device_count()`, `cuda.device(n)` | Enumerate; `Device::name()`, `total_mem()` |
| `Context::new(&cuda, &device)` | `cuCtxCreate_v2`; destroyed on drop |
| `DeviceBuffer::new(&ctx, bytes)` | `cuMemAlloc_v2`; freed on drop |
| `PinnedBuffer::new(&ctx, bytes)` | `cuMemHostAlloc` (flags 0), zero-filled; `as_slice` / `as_mut_slice` |
| `ctx.copy_h2d`, `ctx.copy_d2h` | Synchronous copies; lengths must match exactly |
| `ctx.synchronize()` | `cuCtxSynchronize` |
| `copy_bench(&ctx, bytes, iters, pinned)` | Per-copy H2D and D2H durations |

Contexts and buffers are neither `Send` nor `Sync`: use them on the thread that
created them. Buffers borrow their context, so they cannot outlive it.

## How `copy_bench` measures

1. It verifies one round trip first (patterned H2D, then D2H into a second
   buffer, then compare). A mismatch returns `Error::Corrupted`.
2. It does one unrecorded warm-up copy each way.
3. It times each copy on its own with `std::time::Instant`. Each sample
   includes a `cuCtxSynchronize` after the copy. NVIDIA documents that a
   pageable host-to-device copy can return before its DMA finishes, so the sync
   makes each sample cover the whole transfer.

It returns raw samples. Summarising them (median, p90, p99) is the caller's
job; `fwbench` has the stats module for that.

## Tests

`cargo test` runs the pure-logic tests on any machine. One test does a real
1 MiB round trip on the GPU. It skips with a note when `Cuda::load()` fails.

## License

MIT.
