# resmon

Attribute **CPU, memory, disk I/O and GPU** use to process trees over a sampling
window.

One concern: measure where a machine's four resources actually go. Built for
foldwave's M0 harness, **experiment E3** ([`docs/design.md`](../../docs/design.md)
§8.7b), which asks the question that decides whether GPU offload is worth
building at all: *during an agentic session, what share of this machine is the
terminal itself?*

**Zero runtime dependencies.** Win32 is reached through this crate's own
`extern` blocks — no crates, and no library beyond the `kernel32` that std
already links.

## Use

```
cargo run --release --bin resmon -- [seconds] [--trees N]
```

```
$ resmon 15 --trees 8
```

Measured on the reference machine (Windows 11, 16 logical CPUs, 64 GB RAM,
RTX 3070), 2026-09-15, idle-ish desktop, not elevated:

```
resource attribution over 15.11 s | 16 logical CPUs | RAM 21.4 GiB used of 63.8 GiB

class         cpu-s    %machine        peak-rss    disk-read   disk-write       vram
foldwave      0.23     0.1%    102 MiB  1.05 KiB/s       0 B/s        0 B
agent         2.52     1.0%    680 MiB  2.17 MiB/s  16.7 KiB/s        0 B
compiler      0.03     0.0%   22.2 MiB       0 B/s      20 B/s        0 B
shell         0.09     0.0%    111 MiB       0 B/s  1.03 KiB/s        0 B
browser       7.92     3.3%    583 MiB  1.82 MiB/s  1.82 MiB/s        0 B
other        14.33     5.9%    268 MiB  6.41 MiB/s   132 KiB/s        0 B
TOTAL        25.12    10.4%

gpu: nvidia-smi backend
  [0] NVIDIA GeForce RTX 3070        0% util   2.04 GiB of 8.00 GiB vram
```

As a library:

```rust
use std::time::Duration;

let window = resmon::measure(Duration::from_secs(5))?;
for group in &window.by_class {
    println!("{:10} {:6.2}% of machine", group.label, group.cpu_pct_machine);
}
# Ok::<(), std::io::Error>(())
```

## What the numbers mean, and what they do not

- **CPU** is consumed time, so `%machine` of 100 means every logical core was
  busy with that group for the whole window. Percentages across classes sum to
  the machine's total load, not to 100.
- **Memory** is a peak gauge across the window's two ends, not an average.
- **Disk I/O** counts *every* read and write a process makes — pipes, sockets and
  console handles included, not only disk. It attributes I/O **pressure** to a
  process; it is not a measurement of SSD traffic on its own. A real
  disk-throughput and write-latency probe is a separate tool.
- **GPU** comes from whichever backend answered, and the report always names it.
  Today that is `nvidia-smi` (which ships with the NVIDIA driver, so it costs no
  dependency) or `none`. A vendor-neutral backend over the Windows `GPU Engine`
  performance counters is the natural follow-up.
- **Processes that could not be opened are counted and reported, not hidden.**
  Without elevation, protected and higher-integrity processes deny access and
  their counters read zero — zero because unknown, not because idle. Run
  elevated for full coverage, and read the `note:` line the report prints.

The classifier is a **documented heuristic over executable names**
(`src/proc.rs`), not ground truth. It answers "roughly where is this machine
going", and misfiles anything it has no rule for into `other`.

## Platforms

Windows, per `docs/design.md` §11 decision 5 (Windows first). Other platforms
compile and fail loudly as unsupported rather than reporting zeros, which would
read as "nothing is using this machine" — exactly the wrong answer for an
attribution tool. A Linux `/proc` backend is the obvious next one.

## Develop

From the repository root:

```
python dev.py check     # dependency guard + docs gate + cargo fmt --check + tests
python dev.py test
```

## License

MIT — the nativelite standard for library crates. The foldwave *application* is
FSL-1.1-Apache-2.0; the engine crates it is built on, including this one, stay
MIT.
