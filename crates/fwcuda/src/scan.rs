//! A text-scan kernel: count one byte value across a device buffer.
//!
//! Built for experiment E1 (docs/compute.md): a memory-bound bulk scan that
//! runs beside the renderer on the display GPU, so its cost to frames can be
//! measured at several data sizes per launch. Counting a byte is the core of
//! line indexing and of every substring scan's first pass.
//!
//! The kernel is hand-written PTX, compiled by the driver on first load, so
//! nothing from the CUDA toolkit is built against or shipped. Each thread
//! reads 4 bytes at a time in a grid-stride loop, counts matching bytes with
//! an exact zero-byte test, and adds its total to the result once.

use crate::sys::{CuFunction, CuModule};
use crate::{Context, DeviceBuffer, Error};
use std::ffi::c_void;

/// The kernel: `count_byte(data, n, byte, out)` adds to `*out` the number of
/// bytes in `data[0..n]` equal to `byte`.
///
/// Zero-byte test on `x = word ^ (byte * 0x01010101)`: with
/// `y = ~(((x & 0x7F7F7F7F) + 0x7F7F7F7F) | x | 0x7F7F7F7F)`, bit 7 of each
/// byte of `y` is set exactly when that byte of `x` is zero, so `popc(y)`
/// counts matches with no false positives.
const PTX: &str = "\
.version 7.0
.target sm_52
.address_size 64

.visible .entry count_byte(
    .param .u64 p_data,
    .param .u64 p_n,
    .param .u32 p_byte,
    .param .u64 p_out
)
{
    .reg .pred %p<4>;
    .reg .b32 %r<16>;
    .reg .b64 %rd<20>;

    ld.param.u64 %rd1, [p_data];
    ld.param.u64 %rd2, [p_n];
    ld.param.u32 %r1, [p_byte];
    ld.param.u64 %rd3, [p_out];
    cvta.to.global.u64 %rd1, %rd1;
    cvta.to.global.u64 %rd3, %rd3;
    and.b32 %r1, %r1, 255;
    mul.lo.u32 %r6, %r1, 16843009;

    mov.u32 %r2, %ctaid.x;
    mov.u32 %r3, %ntid.x;
    mov.u32 %r4, %tid.x;
    mov.u32 %r5, %nctaid.x;
    mul.wide.u32 %rd4, %r2, %r3;
    cvt.u64.u32 %rd5, %r4;
    add.u64 %rd4, %rd4, %rd5;
    mov.u64 %rd11, %rd4;
    mul.wide.u32 %rd6, %r5, %r3;
    shr.u64 %rd7, %rd2, 2;
    mov.u64 %rd8, 0;

WORDS:
    setp.ge.u64 %p1, %rd4, %rd7;
    @%p1 bra TAIL_START;
    shl.b64 %rd9, %rd4, 2;
    add.u64 %rd9, %rd1, %rd9;
    ld.global.u32 %r7, [%rd9];
    xor.b32 %r8, %r7, %r6;
    and.b32 %r9, %r8, 2139062143;
    add.u32 %r9, %r9, 2139062143;
    or.b32 %r9, %r9, %r8;
    or.b32 %r9, %r9, 2139062143;
    not.b32 %r9, %r9;
    popc.b32 %r10, %r9;
    cvt.u64.u32 %rd10, %r10;
    add.u64 %rd8, %rd8, %rd10;
    add.u64 %rd4, %rd4, %rd6;
    bra WORDS;

TAIL_START:
    setp.ne.u64 %p2, %rd11, 0;
    @%p2 bra ADD;
    shl.b64 %rd12, %rd7, 2;
TAIL:
    setp.ge.u64 %p3, %rd12, %rd2;
    @%p3 bra ADD;
    add.u64 %rd13, %rd1, %rd12;
    ld.global.u8 %r11, [%rd13];
    setp.eq.u32 %p1, %r11, %r1;
    selp.u64 %rd14, 1, 0, %p1;
    add.u64 %rd8, %rd8, %rd14;
    add.u64 %rd12, %rd12, 1;
    bra TAIL;

ADD:
    setp.eq.u64 %p1, %rd8, 0;
    @%p1 bra END;
    atom.global.add.u64 %rd15, [%rd3], %rd8;
END:
    ret;
}
";

/// Threads per block, and blocks per launch: enough to cover every SM on
/// current GPUs several times over; the grid-stride loop does the rest.
const BLOCK: u32 = 256;
const GRID: u32 = 1024;

/// The byte-count kernel, loaded into one context.
pub struct ByteCounter<'c> {
    ctx: &'c Context<'c>,
    module: CuModule,
    func: CuFunction,
    out: DeviceBuffer<'c>,
}

impl<'c> ByteCounter<'c> {
    /// Load (JIT-compile) the kernel into `ctx`.
    pub fn new(ctx: &'c Context<'c>) -> Result<ByteCounter<'c>, Error> {
        let ptx = format!("{PTX}\0");
        ctx.make_current()?;
        let mut module: CuModule = std::ptr::null_mut();
        // SAFETY: `ptx` is a NUL-terminated PTX image, as cuModuleLoadData
        // requires; `module` is a valid out-pointer.
        let r = unsafe { (ctx.api().module_load_data)(&mut module, ptx.as_ptr().cast()) };
        ctx.cuda.check("cuModuleLoadData", r)?;
        let mut func: CuFunction = std::ptr::null_mut();
        // SAFETY: `module` was just loaded; the name is NUL-terminated.
        let r =
            unsafe { (ctx.api().module_get_function)(&mut func, module, b"count_byte\0".as_ptr()) };
        if let Err(e) = ctx.cuda.check("cuModuleGetFunction", r) {
            // SAFETY: unload the module loaded above, once.
            unsafe { (ctx.api().module_unload)(module) };
            return Err(e);
        }
        let out = DeviceBuffer::new(ctx, 8)?;
        Ok(ByteCounter {
            ctx,
            module,
            func,
            out,
        })
    }

    /// Count `byte` in `data`. Launches once and waits for the result.
    pub fn count(&self, data: &DeviceBuffer<'_>, byte: u8) -> Result<u64, Error> {
        self.ctx.check_owner(data.owner.raw)?;
        self.ctx.copy_h2d(&self.out, &[0u8; 8])?;
        self.ctx.make_current()?;
        let (mut p_data, mut p_n, mut p_byte, mut p_out) =
            (data.ptr, data.len as u64, u32::from(byte), self.out.ptr);
        let mut params: [*mut c_void; 4] = [
            (&mut p_data as *mut u64).cast(),
            (&mut p_n as *mut u64).cast(),
            (&mut p_byte as *mut u32).cast(),
            (&mut p_out as *mut u64).cast(),
        ];
        // SAFETY: `func` is `count_byte`, whose four parameters are exactly
        // the four values `params` points at, alive across the call (it copies
        // them); `data` and `out` are live allocations in this context, and
        // the kernel reads `data[0..len]` and writes 8 bytes of `out`.
        let r = unsafe {
            (self.ctx.api().launch_kernel)(
                self.func,
                GRID,
                1,
                1,
                BLOCK,
                1,
                1,
                0,
                std::ptr::null_mut(),
                params.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        self.ctx.cuda.check("cuLaunchKernel", r)?;
        self.ctx.sync_current()?;
        let mut result = [0u8; 8];
        self.ctx.copy_d2h(&mut result, &self.out)?;
        Ok(u64::from_le_bytes(result))
    }
}

impl Drop for ByteCounter<'_> {
    fn drop(&mut self) {
        let _ = self.ctx.make_current();
        // SAFETY: the module was loaded in `new` and is unloaded once.
        let _ = unsafe { (self.ctx.api().module_unload)(self.module) };
    }
}

/// Bytes of log per signature, and bytes per signature (2^16 bits).
pub const SIG_BLOCK: usize = 1 << 20;
pub const SIG_BYTES: usize = 1 << 13;

/// The kernel: `trigram_sigs(data, lead, stride, out)`. CTA `k` builds the
/// signature of the 1 MiB block at `data[lead + k * stride ..]`: one bit for every trigram
/// (ASCII case-folded) that ends in the block, at
/// `((a | b << 8 | c << 16) * 0x9E3779B1) >> 16`, the index panelog's search
/// signatures use. `lead` bytes before the first block (0 or 2) supply the
/// trigrams that straddle into it; with 0, the first two bytes start none.
/// The bitmap is built in shared memory with atomic ORs and written out as
/// 2048 little-endian words, which is panelog's byte layout.
const SIGS_PTX: &str = "\
.version 7.0
.target sm_52
.address_size 64

.visible .entry trigram_sigs(
    .param .u64 p_data,
    .param .u32 p_lead,
    .param .u64 p_stride,
    .param .u64 p_out
)
{
    .reg .pred %p<4>;
    .reg .b32 %r<32>;
    .reg .b64 %rd<16>;
    .shared .align 4 .b32 sig[2048];

    ld.param.u64 %rd1, [p_data];
    ld.param.u32 %r1, [p_lead];
    ld.param.u64 %rd2, [p_out];
    ld.param.u64 %rd11, [p_stride];
    cvta.to.global.u64 %rd1, %rd1;
    cvta.to.global.u64 %rd2, %rd2;
    mov.u32 %r2, %tid.x;
    mov.u32 %r3, %ntid.x;
    mov.u32 %r4, %ctaid.x;
    mov.u32 %r7, sig;

    mov.u32 %r5, %r2;
ZERO:
    setp.ge.u32 %p1, %r5, 2048;
    @%p1 bra ZEROED;
    shl.b32 %r6, %r5, 2;
    add.u32 %r6, %r7, %r6;
    st.shared.u32 [%r6], 0;
    add.u32 %r5, %r5, %r3;
    bra ZERO;
ZEROED:
    bar.sync 0;

    cvt.u64.u32 %rd3, %r4;
    mul.lo.u64 %rd3, %rd3, %rd11;
    cvt.u64.u32 %rd4, %r1;
    add.u64 %rd3, %rd3, %rd4;
    add.u64 %rd5, %rd3, 1048576;
    cvt.u64.u32 %rd6, %r2;
    add.u64 %rd6, %rd3, %rd6;
    cvt.u64.u32 %rd7, %r3;
BYTES:
    setp.ge.u64 %p1, %rd6, %rd5;
    @%p1 bra BUILT;
    setp.lt.u64 %p2, %rd6, 2;
    @%p2 bra NEXT;
    add.u64 %rd8, %rd1, %rd6;
    ld.global.u8 %r10, [%rd8+-2];
    ld.global.u8 %r11, [%rd8+-1];
    ld.global.u8 %r12, [%rd8];
    sub.u32 %r13, %r10, 65;
    setp.le.u32 %p3, %r13, 25;
    @%p3 add.u32 %r10, %r10, 32;
    sub.u32 %r13, %r11, 65;
    setp.le.u32 %p3, %r13, 25;
    @%p3 add.u32 %r11, %r11, 32;
    sub.u32 %r13, %r12, 65;
    setp.le.u32 %p3, %r13, 25;
    @%p3 add.u32 %r12, %r12, 32;
    shl.b32 %r14, %r11, 8;
    shl.b32 %r15, %r12, 16;
    or.b32 %r16, %r10, %r14;
    or.b32 %r16, %r16, %r15;
    mul.lo.u32 %r17, %r16, -1640531535;
    shr.u32 %r17, %r17, 16;
    shr.u32 %r18, %r17, 5;
    and.b32 %r19, %r17, 31;
    mov.u32 %r20, 1;
    shl.b32 %r20, %r20, %r19;
    shl.b32 %r18, %r18, 2;
    add.u32 %r18, %r7, %r18;
    atom.shared.or.b32 %r21, [%r18], %r20;
NEXT:
    add.u64 %rd6, %rd6, %rd7;
    bra BYTES;
BUILT:
    bar.sync 0;

    cvt.u64.u32 %rd9, %r4;
    shl.b64 %rd9, %rd9, 13;
    add.u64 %rd9, %rd2, %rd9;
    mov.u32 %r5, %r2;
WRITE:
    setp.ge.u32 %p1, %r5, 2048;
    @%p1 bra END;
    shl.b32 %r6, %r5, 2;
    add.u32 %r22, %r7, %r6;
    ld.shared.u32 %r23, [%r22];
    cvt.u64.u32 %rd10, %r6;
    add.u64 %rd10, %rd9, %rd10;
    st.global.u32 [%rd10], %r23;
    add.u32 %r5, %r5, %r3;
    bra WRITE;
END:
    ret;
}
";

/// Load `ptx` into `ctx` and find `name` in it.
fn load_kernel(ctx: &Context<'_>, ptx: &str, name: &[u8]) -> Result<(CuModule, CuFunction), Error> {
    let image = format!("{ptx}\0");
    ctx.make_current()?;
    let mut module: CuModule = std::ptr::null_mut();
    // SAFETY: `image` is a NUL-terminated PTX image; `module` is a valid
    // out-pointer.
    let r = unsafe { (ctx.api().module_load_data)(&mut module, image.as_ptr().cast()) };
    ctx.cuda.check("cuModuleLoadData", r)?;
    let mut func: CuFunction = std::ptr::null_mut();
    // SAFETY: `module` was just loaded; `name` is NUL-terminated.
    let r = unsafe { (ctx.api().module_get_function)(&mut func, module, name.as_ptr()) };
    if let Err(e) = ctx.cuda.check("cuModuleGetFunction", r) {
        // SAFETY: unload the module loaded above, once.
        unsafe { (ctx.api().module_unload)(module) };
        return Err(e);
    }
    Ok((module, func))
}

/// panelog's search signatures, built on the GPU.
pub struct TrigramSigs<'c> {
    ctx: &'c Context<'c>,
    module: CuModule,
    func: CuFunction,
}

impl<'c> TrigramSigs<'c> {
    /// Load (JIT-compile) the kernel into `ctx`.
    pub fn new(ctx: &'c Context<'c>) -> Result<TrigramSigs<'c>, Error> {
        let (module, func) = load_kernel(ctx, SIGS_PTX, b"trigram_sigs\0")?;
        Ok(TrigramSigs { ctx, module, func })
    }

    /// Build the signatures of `blocks` whole 1 MiB blocks into `out`
    /// ([`SIG_BYTES`] each), and wait. Block `k` starts at
    /// `lead + k * stride` in `data`, with the two bytes before it the
    /// trigram lead-in (`lead` 0: the first block has none). A contiguous log
    /// has `stride` [`SIG_BLOCK`]; blocks gathered from several logs, each
    /// behind its own two lead bytes, have `SIG_BLOCK + 2` and `lead` 2.
    pub fn build(
        &self,
        data: &DeviceBuffer<'_>,
        lead: u32,
        stride: usize,
        blocks: usize,
        out: &DeviceBuffer<'_>,
    ) -> Result<(), Error> {
        self.ctx.check_owner(data.owner.raw)?;
        self.ctx.check_owner(out.owner.raw)?;
        let need = lead as usize + blocks.saturating_sub(1) * stride + SIG_BLOCK;
        if lead > 2 || stride < SIG_BLOCK || (blocks > 0 && data.len < need) {
            return Err(Error::InvalidArgument("data shorter than lead + blocks"));
        }
        if out.len < blocks * SIG_BYTES {
            return Err(Error::InvalidArgument(
                "out shorter than one signature a block",
            ));
        }
        if blocks == 0 {
            return Ok(());
        }
        self.ctx.make_current()?;
        let (mut p_data, mut p_lead, mut p_stride, mut p_out) =
            (data.ptr, lead, stride as u64, out.ptr);
        let mut params: [*mut c_void; 4] = [
            (&mut p_data as *mut u64).cast(),
            (&mut p_lead as *mut u32).cast(),
            (&mut p_stride as *mut u64).cast(),
            (&mut p_out as *mut u64).cast(),
        ];
        // SAFETY: `func` is `trigram_sigs`, whose four parameters are the
        // values `params` points at, alive across the call; CTA `k` reads
        // `data[lead + k * stride - 2 .. lead + k * stride + 1 MiB]` (bytes
        // below index 2 are never read) and writes `out[k * 8 KiB ..][..8 KiB]`,
        // all inside the checked lengths.
        let r = unsafe {
            (self.ctx.api().launch_kernel)(
                self.func,
                blocks as u32,
                1,
                1,
                BLOCK,
                1,
                1,
                0,
                std::ptr::null_mut(),
                params.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        self.ctx.cuda.check("cuLaunchKernel", r)?;
        self.ctx.sync_current()
    }
}

impl Drop for TrigramSigs<'_> {
    fn drop(&mut self) {
        let _ = self.ctx.make_current();
        // SAFETY: the module was loaded in `new` and is unloaded once.
        let _ = unsafe { (self.ctx.api().module_unload)(self.module) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cuda, DeviceBuffer};

    #[test]
    fn the_gpu_count_matches_the_cpu_count() {
        // Machines without an NVIDIA driver have nothing to test.
        let Ok(cuda) = Cuda::load() else { return };
        let Ok(dev) = cuda.device(0) else { return };
        let ctx = Context::new(&cuda, &dev).unwrap();
        let counter = ByteCounter::new(&ctx).unwrap();
        // Odd lengths exercise the tail; a pseudo-random fill with planted
        // newlines, including in the last bytes.
        for len in [1usize, 3, 4, 5, 1023, 4096, 1 << 20, (1 << 22) + 7] {
            let mut host = vec![0u8; len];
            let mut x: u32 = 0x1234_5678 ^ len as u32;
            for (i, b) in host.iter_mut().enumerate() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = if i % 37 == 0 || i + 1 == len {
                    b'\n'
                } else {
                    x as u8
                };
            }
            let buf = DeviceBuffer::new(&ctx, len).unwrap();
            ctx.copy_h2d(&buf, &host).unwrap();
            for byte in [b'\n', 0u8, 0xFF, b'a'] {
                let want = host.iter().filter(|&&b| b == byte).count() as u64;
                assert_eq!(
                    counter.count(&buf, byte).unwrap(),
                    want,
                    "len {len} byte {byte}"
                );
            }
        }
    }
}
