//! The Vulkan backend: a hand-bound, runtime-loaded Vulkan 1.0 renderer that
//! draws a [`Frame`] with one instanced draw.
//!
//! `vulkan-1.dll` is loaded at run time (see `sys`); a machine without a
//! Vulkan driver gets an [`Error`], never a failed link or a panic. The
//! shaders are the SPIR-V words of [`crate::spirv`]; there is no shader
//! compiler and no third-party crate.
//!
//! # How a frame is drawn
//!
//! * Every cell is one instance of a 6-vertex quad. The instance buffer holds
//!   the frame's [`crate::CellInstance`]s (16 bytes each, format
//!   `R32G32B32A32_UINT`) in host-visible, coherent memory; only **damaged
//!   rows** are rewritten.
//! * The atlas is a device-local `R8_UNORM` image. Its **dirty band**
//!   ([`Atlas::take_dirty`]) is copied into a host-visible staging buffer
//!   that mirrors the whole atlas, and from there into the image with a
//!   buffer-to-image copy of just those pixel rows.
//! * Swapchain images do not keep their contents, so every frame that is
//!   drawn draws the whole grid over a clear to [`Frame::background`]. Damage
//!   decides what is uploaded and whether anything is drawn at all: with no
//!   damaged row, a clean atlas and nothing else pending, [`VkRenderer::draw`]
//!   returns at once with [`DrawStats::skipped`] set.
//!
//! # One frame in flight
//!
//! This is a spike: a single command buffer and a single fence. Every draw
//! first waits for the previous one's fence, and only then rewrites the
//! instance buffer and the staging buffer, which the GPU may still have been
//! reading. That costs CPU/GPU overlap and is the first thing to change if
//! the measurements call for it.
//!
//! # Exactness
//!
//! The target is `B8G8R8A8_UNORM` (or `R8G8B8A8_UNORM` if a surface offers
//! only that), never an sRGB format, and the fragment shader computes the
//! crate's pixel rule in integers (see [`crate::spirv`]), so the output
//! matches [`crate::CpuRenderer`] byte for byte in practice; the tests allow
//! the documented one step per channel.

#![allow(clippy::too_many_arguments)]

mod sys;
#[cfg(test)]
mod tests;

use crate::atlas::{Atlas, Metrics};
use crate::frame::{CellInstance, Frame};
use crate::spirv;
use std::ffi::c_void;
use std::fmt;
use std::ops::Range;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use sys::*;

/// How a window's swapchain presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    /// Wait for vertical blank; never tears. Always supported.
    Fifo,
    /// Replace the queued image at vertical blank; never tears, never blocks.
    Mailbox,
    /// Present at once; may tear.
    Immediate,
}

impl PresentMode {
    fn raw(self) -> VkEnum {
        match self {
            PresentMode::Fifo => PRESENT_MODE_FIFO_KHR,
            PresentMode::Mailbox => PRESENT_MODE_MAILBOX_KHR,
            PresentMode::Immediate => PRESENT_MODE_IMMEDIATE_KHR,
        }
    }
}

/// What one [`VkRenderer::draw`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrawStats {
    /// Nothing was drawn: no damage, a clean atlas, nothing pending (or a
    /// minimized window with no swapchain to draw to).
    pub skipped: bool,
    /// Rows of instance data written to the GPU-visible buffer.
    pub rows_uploaded: usize,
    /// Atlas pixel rows copied to the atlas image in this frame.
    pub atlas_rows_uploaded: u32,
    /// Host uploads, command recording and `vkQueueSubmit`.
    pub record_submit: Duration,
    /// `vkAcquireNextImageKHR` plus `vkQueuePresentKHR` (zero offscreen).
    pub present: Duration,
}

/// Everything that can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `vulkan-1.dll` could not be loaded (no Vulkan driver). Win32 error code.
    LibraryNotFound(u32),
    /// The loader lacks a required entry point (too old).
    MissingSymbol(&'static str),
    /// A Vulkan call returned a failure `VkResult`.
    Vulkan {
        /// The Vulkan function that failed, e.g. `vkCreateDevice`.
        call: &'static str,
        /// The raw `VkResult`.
        code: i32,
    },
    /// No physical device has a graphics queue (and, for a window, can
    /// present to its surface and has `VK_KHR_swapchain`).
    NoSuitableDevice,
    /// The platform or surface lacks something this backend needs, such as
    /// `VK_KHR_win32_surface` or a UNORM surface format.
    Unsupported(&'static str),
    /// An argument this backend rejects before calling the driver.
    InvalidArgument(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::LibraryNotFound(code) => write!(
                f,
                "vulkan-1.dll could not be loaded (Win32 error {code}); is a Vulkan driver installed?"
            ),
            Error::MissingSymbol(s) => write!(f, "the Vulkan loader has no entry point {s}"),
            Error::Vulkan { call, code } => {
                write!(f, "{call} failed: {} ({code})", result_name(*code))
            }
            Error::NoSuitableDevice => f.write_str("no suitable Vulkan device"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::InvalidArgument(why) => write!(f, "invalid argument: {why}"),
        }
    }
}

impl std::error::Error for Error {}

fn check(call: &'static str, r: VkResult) -> Result<(), Error> {
    if r == VK_SUCCESS {
        Ok(())
    } else {
        Err(Error::Vulkan { call, code: r })
    }
}

/// The two-call enumeration idiom: ask for the count, then fill.
fn enumerate<T: Clone>(
    call: &'static str,
    zero: T,
    mut f: impl FnMut(*mut u32, *mut T) -> VkResult,
) -> Result<Vec<T>, Error> {
    let mut n = 0u32;
    check(call, f(&mut n, null_mut()))?;
    let mut v = vec![zero; n as usize];
    let r = f(&mut n, v.as_mut_ptr());
    if r != VK_SUCCESS && r != VK_INCOMPLETE {
        return Err(Error::Vulkan { call, code: r });
    }
    v.truncate(n as usize);
    Ok(v)
}

/// A NUL-terminated C char array as a `String`.
fn c_name(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The instance (and the window surface, if any). Declared last in
/// [`VkRenderer`], so it is destroyed after the device.
struct Instance {
    fns: InstanceFns,
    surface_fns: Option<SurfaceFns>,
    handle: VkInstance,
    surface: VkSurfaceKHR,
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: both handles were created by this instance's functions, and
        // every child object has been destroyed by the time this runs (the
        // device is destroyed in `VkRenderer::drop`, before its fields drop).
        unsafe {
            if let Some(sf) = &self.surface_fns {
                if self.surface != NULL_HANDLE {
                    (sf.destroy_surface)(self.handle, self.surface, null());
                }
            }
            (self.fns.destroy_instance)(self.handle, null());
        }
    }
}

fn create_instance(window: bool) -> Result<Instance, Error> {
    let gipa = sys::load().map_err(|e| match e {
        LoadError::Library(code) => Error::LibraryNotFound(code),
        LoadError::Symbol(s) => Error::MissingSymbol(s),
    })?;
    // SAFETY (all `gipa` calls): `name` is NUL-terminated ASCII and the
    // instance argument is null (global commands) or a live instance.
    let entry = EntryFns::load(|name| unsafe { gipa(null_mut(), name.as_ptr()) })
        .map_err(Error::MissingSymbol)?;

    let mut extensions: Vec<*const u8> = Vec::new();
    if window {
        let available = enumerate(
            "vkEnumerateInstanceExtensionProperties",
            VkExtensionProperties {
                extensionName: [0; 256],
                specVersion: 0,
            },
            // SAFETY: the pointers are the count/array out-parameters the
            // two-call idiom passes; the layer name is null (the loader).
            |n, p| unsafe { (entry.enumerate_instance_extension_properties)(null(), n, p) },
        )?;
        for want in [KHR_SURFACE, KHR_WIN32_SURFACE] {
            let name = want.trim_end_matches('\0');
            if !available.iter().any(|e| c_name(&e.extensionName) == name) {
                return Err(Error::Unsupported("VK_KHR_surface / VK_KHR_win32_surface"));
            }
            extensions.push(want.as_ptr());
        }
    }

    let app = VkApplicationInfo {
        sType: ST_APPLICATION_INFO,
        pNext: null(),
        pApplicationName: b"foldwave\0".as_ptr(),
        applicationVersion: 0,
        pEngineName: b"fwrender\0".as_ptr(),
        engineVersion: 0,
        apiVersion: VK_API_VERSION_1_0,
    };
    let info = VkInstanceCreateInfo {
        sType: ST_INSTANCE_CREATE_INFO,
        pNext: null(),
        flags: 0,
        pApplicationInfo: &app,
        enabledLayerCount: 0,
        ppEnabledLayerNames: null(),
        enabledExtensionCount: extensions.len() as u32,
        ppEnabledExtensionNames: extensions.as_ptr(),
    };
    let mut handle: VkInstance = null_mut();
    // SAFETY: `info` and everything it points to live across the call.
    check("vkCreateInstance", unsafe {
        (entry.create_instance)(&info, null(), &mut handle)
    })?;

    // SAFETY: as above, with the live instance.
    let lookup = |name: &'static str| unsafe { gipa(handle, name.as_ptr()) };
    let fns = match InstanceFns::load(lookup) {
        Ok(f) => f,
        Err(sym) => {
            if let Some(destroy) = lookup("vkDestroyInstance\0") {
                // SAFETY: the loader's vkDestroyInstance, whose prototype is
                // `void (VkInstance, const VkAllocationCallbacks*)`.
                unsafe {
                    let destroy: unsafe extern "system" fn(VkInstance, Alloc) =
                        std::mem::transmute(destroy);
                    destroy(handle, null());
                }
            }
            return Err(Error::MissingSymbol(sym));
        }
    };
    let mut instance = Instance {
        fns,
        surface_fns: None,
        handle,
        surface: NULL_HANDLE,
    };
    if window {
        instance.surface_fns = Some(SurfaceFns::load(lookup).map_err(Error::MissingSymbol)?);
    }
    Ok(instance)
}

/// A buffer in host-visible, host-coherent memory, persistently mapped.
struct HostBuffer {
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    ptr: *mut u8,
    size: u64,
}

impl HostBuffer {
    const EMPTY: HostBuffer = HostBuffer {
        buffer: NULL_HANDLE,
        memory: NULL_HANDLE,
        ptr: null_mut(),
        size: 0,
    };
}

/// The offscreen colour target and its readback buffer.
struct Offscreen {
    image: VkImage,
    memory: VkDeviceMemory,
    view: VkImageView,
    framebuffer: VkFramebuffer,
    readback: HostBuffer,
}

impl Offscreen {
    const EMPTY: Offscreen = Offscreen {
        image: NULL_HANDLE,
        memory: NULL_HANDLE,
        view: NULL_HANDLE,
        framebuffer: NULL_HANDLE,
        readback: HostBuffer::EMPTY,
    };
}

/// The window's swapchain and everything sized by it.
struct Window {
    fns: SwapchainFns,
    mode: VkEnum,
    color_space: VkEnum,
    swapchain: VkSwapchainKHR,
    extent: VkExtent2D,
    images: Vec<VkImage>,
    views: Vec<VkImageView>,
    framebuffers: Vec<VkFramebuffer>,
    /// One per swapchain image: signalled by the draw, waited on by present.
    render_done: Vec<VkSemaphore>,
    /// Signalled by acquire, waited on by the draw.
    image_available: VkSemaphore,
    /// The surface changed (suboptimal / out of date / resized): rebuild
    /// before the next acquire.
    stale: bool,
}

/// The renderer.
///
/// It owns its own Vulkan instance and device. It holds raw handles and is
/// neither `Send` nor `Sync`: use it on the thread that created it (and, for
/// a window, the thread that owns the window).
pub struct VkRenderer {
    dev: DeviceFns,
    device: VkDevice,
    queue: VkQueue,
    physical: VkPhysicalDevice,
    queue_family: u32,
    memory: VkPhysicalDeviceMemoryProperties,
    device_name: String,
    /// Colour target format.
    format: VkEnum,
    width: u32,
    height: u32,

    render_pass: VkRenderPass,
    set_layout: VkDescriptorSetLayout,
    descriptor_pool: VkDescriptorPool,
    descriptor_set: VkDescriptorSet,
    pipeline_layout: VkPipelineLayout,
    pipeline: VkPipeline,
    command_pool: VkCommandPool,
    cmd: VkCommandBuffer,
    fence: VkFence,
    /// Work was submitted with `fence` and not yet waited for. Only a
    /// successful submit sets it: a submit that fails after the fence was
    /// reset leaves the fence unsignalled forever, and waiting on it then
    /// would hang.
    fence_pending: std::cell::Cell<bool>,

    atlas_image: VkImage,
    atlas_memory: VkDeviceMemory,
    atlas_view: VkImageView,
    atlas_width: u32,
    atlas_height: u32,
    metrics: Metrics,
    slots_per_row: u32,
    /// Mirrors the whole atlas, so any band of it can be copied from here.
    staging: HostBuffer,
    /// Staged atlas rows not yet copied to the image.
    pending_band: Option<Range<u32>>,

    instances: HostBuffer,
    /// `(rows, cols)` the instance buffer holds; `None` until first filled.
    grid: Option<(usize, usize)>,

    offscreen: Option<Offscreen>,
    window: Option<Window>,
    /// Something invisible to the frame changed (resize, new swapchain, a
    /// frame that never reached the screen): the next draw must not skip.
    needs_draw: bool,
    /// Offscreen: an image has been drawn, so there is something to read.
    drawn: bool,

    /// Last field: dropped after `Drop::drop` has destroyed the device.
    instance: Instance,
}

impl fmt::Debug for VkRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VkRenderer")
            .field("device", &self.device_name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("offscreen", &self.offscreen.is_some())
            .finish_non_exhaustive()
    }
}

impl VkRenderer {
    /// An offscreen renderer drawing into a `width x height` image that
    /// [`VkRenderer::read_pixels`] reads back. `atlas` is uploaded in full;
    /// every later [`VkRenderer::draw`] must be given the same atlas.
    pub fn offscreen(width: u32, height: u32, atlas: &Atlas) -> Result<VkRenderer, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidArgument("offscreen size must be non-zero"));
        }
        let instance = create_instance(false)?;
        let mut r = VkRenderer::build(instance, FORMAT_B8G8R8A8_UNORM, atlas, false)?;
        r.width = width;
        r.height = height;
        let o = r.make_offscreen(width, height)?;
        r.offscreen = Some(o);
        Ok(r)
    }

    /// A renderer presenting to a Win32 window through a swapchain.
    ///
    /// `width x height` is used only if the surface leaves the size to the
    /// application; normally the swapchain takes the window's client size. A
    /// `mode` the surface does not offer falls back to [`PresentMode::Fifo`].
    ///
    /// # Safety
    /// `hinstance` and `hwnd` must be the raw Win32 `HINSTANCE` and `HWND` of
    /// a live window, owned by the caller, that outlives the renderer.
    pub unsafe fn for_window(
        hinstance: *mut c_void,
        hwnd: *mut c_void,
        width: u32,
        height: u32,
        atlas: &Atlas,
        mode: PresentMode,
    ) -> Result<VkRenderer, Error> {
        let mut instance = create_instance(true)?;
        let info = VkWin32SurfaceCreateInfoKHR {
            sType: ST_WIN32_SURFACE_CREATE_INFO_KHR,
            pNext: null(),
            flags: 0,
            hinstance,
            hwnd,
        };
        let mut surface = NULL_HANDLE;
        {
            let sf = instance.surface_fns.as_ref().expect("loaded for a window");
            // SAFETY: the caller guarantees a live window; `info` lives
            // across the call. On success the surface is owned by `instance`.
            check("vkCreateWin32SurfaceKHR", unsafe {
                (sf.create_win32_surface)(instance.handle, &info, null(), &mut surface)
            })?;
        }
        instance.surface = surface;
        let sf = instance.surface_fns.as_ref().expect("loaded for a window");

        // The format decides the render pass, so pick it before building:
        // query it on the device `build` will pick.
        let (physical, _, _) = pick_device(&instance, true)?;
        let formats = enumerate(
            "vkGetPhysicalDeviceSurfaceFormatsKHR",
            VkSurfaceFormatKHR::default(),
            // SAFETY: live physical device and surface; two-call idiom.
            |n, p| unsafe { (sf.get_physical_device_surface_formats)(physical, surface, n, p) },
        )?;
        let pick = [FORMAT_B8G8R8A8_UNORM, FORMAT_R8G8B8A8_UNORM]
            .iter()
            .find_map(|&want| formats.iter().find(|f| f.format == want).copied())
            .ok_or(Error::Unsupported(
                "no B8G8R8A8/R8G8B8A8 UNORM surface format",
            ))?;
        let modes = enumerate(
            "vkGetPhysicalDeviceSurfacePresentModesKHR",
            0 as VkEnum,
            // SAFETY: as above.
            |n, p| unsafe {
                (sf.get_physical_device_surface_present_modes)(physical, surface, n, p)
            },
        )?;
        let mode = if modes.contains(&mode.raw()) {
            mode.raw()
        } else {
            PRESENT_MODE_FIFO_KHR
        };

        let mut r = VkRenderer::build(instance, pick.format, atlas, true)?;
        r.width = width;
        r.height = height;
        let fns = {
            let dev = r.device;
            let gdpa = r.instance.fns.get_device_proc_addr;
            // SAFETY: a live device and a NUL-terminated name.
            SwapchainFns::load(|name| unsafe { gdpa(dev, name.as_ptr()) })
                .map_err(Error::MissingSymbol)?
        };
        let mut w = Window {
            fns,
            mode,
            color_space: pick.colorSpace,
            swapchain: NULL_HANDLE,
            extent: VkExtent2D::default(),
            images: Vec::new(),
            views: Vec::new(),
            framebuffers: Vec::new(),
            render_done: Vec::new(),
            image_available: NULL_HANDLE,
            stale: false,
        };
        let made = match r.semaphore() {
            Ok(s) => {
                w.image_available = s;
                r.rebuild_swapchain(&mut w)
            }
            Err(e) => Err(e),
        };
        // Owned by `r` from here, so `Drop` releases it on error too.
        r.window = Some(w);
        made?;
        Ok(r)
    }

    /// The physical device's name, e.g. `NVIDIA GeForce RTX 3070`.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Current target size in pixels (for a window, the swapchain's extent
    /// once one exists).
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Resize the target. Offscreen, the image is recreated (and its
    /// contents lost); for a window, the swapchain is rebuilt before the
    /// next draw at the window's new client size. The next draw is never
    /// skipped.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Error> {
        self.needs_draw = true;
        if let Some(w) = self.window.as_mut() {
            self.width = width;
            self.height = height;
            w.stale = true;
            return Ok(());
        }
        if width == 0 || height == 0 {
            return Err(Error::InvalidArgument("offscreen size must be non-zero"));
        }
        self.wait_fence()?;
        if let Some(mut o) = self.offscreen.take() {
            self.free_offscreen(&mut o);
        }
        self.drawn = false;
        self.width = width;
        self.height = height;
        let o = self.make_offscreen(width, height)?;
        self.offscreen = Some(o);
        Ok(())
    }

    /// Upload the atlas's dirty band ([`Atlas::take_dirty`]) and the
    /// instance data of the frame's damaged rows, then draw the whole grid
    /// (one instanced draw) and, for a window, present.
    ///
    /// When no row is damaged, the atlas is clean and nothing else is pending
    /// (see [`VkRenderer::resize`]), nothing happens and
    /// [`DrawStats::skipped`] is set. The frame's damage is **not** cleared;
    /// the caller does that once every consumer has drawn.
    pub fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> Result<DrawStats, Error> {
        if atlas.width() != self.atlas_width
            || atlas.height() != self.atlas_height
            || atlas.metrics() != self.metrics
            || atlas.slots_per_row() != self.slots_per_row
        {
            return Err(Error::InvalidArgument(
                "draw needs the atlas the renderer was created with",
            ));
        }
        let mut stats = DrawStats::default();
        let band = atlas.take_dirty();
        let regrid = self.grid != Some((frame.rows(), frame.cols()));
        if band.is_none() && frame.damaged_rows() == 0 && !regrid && !self.needs_draw {
            stats.skipped = true;
            return Ok(stats);
        }

        // One frame in flight: after this, the GPU reads none of the host
        // memory written below.
        self.wait_fence()?;

        let mut image_index = 0;
        let mut window = self.window.take();
        if let Some(w) = window.as_mut() {
            let t = Instant::now();
            let acquired = self.acquire(w);
            stats.present += t.elapsed();
            match acquired {
                Ok(Some(i)) => image_index = i,
                Ok(None) => {
                    // Minimized: stage the uploads, draw later.
                    self.stage(frame, atlas, band, regrid, &mut stats)?;
                    self.window = window;
                    self.needs_draw = true;
                    stats.skipped = true;
                    return Ok(stats);
                }
                Err(e) => {
                    self.window = window;
                    return Err(e);
                }
            }
        }
        let t = Instant::now();
        let result = self
            .stage(frame, atlas, band, regrid, &mut stats)
            .and_then(|_| self.record_and_submit(frame, window.as_ref(), image_index, &mut stats));
        stats.record_submit = t.elapsed();
        if let Err(e) = result {
            self.window = window;
            return Err(e);
        }
        self.needs_draw = false;
        self.drawn = true;

        if let Some(w) = window.as_mut() {
            let t = Instant::now();
            let info = VkPresentInfoKHR {
                sType: ST_PRESENT_INFO_KHR,
                pNext: null(),
                waitSemaphoreCount: 1,
                pWaitSemaphores: &w.render_done[image_index as usize],
                swapchainCount: 1,
                pSwapchains: &w.swapchain,
                pImageIndices: &image_index,
                pResults: null_mut(),
            };
            // SAFETY: every handle is live; `info` lives across the call.
            let r = unsafe { (w.fns.queue_present)(self.queue, &info) };
            stats.present += t.elapsed();
            match r {
                VK_SUCCESS => {}
                VK_SUBOPTIMAL_KHR | VK_ERROR_OUT_OF_DATE_KHR => {
                    w.stale = true;
                    // The image may not have reached the screen: redraw.
                    self.needs_draw = r == VK_ERROR_OUT_OF_DATE_KHR;
                }
                code => {
                    self.window = window;
                    return Err(Error::Vulkan {
                        call: "vkQueuePresentKHR",
                        code,
                    });
                }
            }
        }
        self.window = window;
        Ok(stats)
    }

    /// Offscreen only: wait for the GPU and read the last drawn image back as
    /// `0xAARRGGBB`, `width * height` pixels, row-major.
    pub fn read_pixels(&mut self) -> Result<Vec<u32>, Error> {
        let (image, buffer, ptr) = match &self.offscreen {
            Some(o) => (o.image, o.readback.buffer, o.readback.ptr),
            None => {
                return Err(Error::InvalidArgument(
                    "read_pixels needs an offscreen renderer",
                ))
            }
        };
        if !self.drawn {
            return Err(Error::InvalidArgument("nothing has been drawn yet"));
        }
        self.wait_fence()?;
        self.begin()?;
        let region = VkBufferImageCopy {
            bufferOffset: 0,
            bufferRowLength: 0, // tightly packed
            bufferImageHeight: 0,
            imageSubresource: color_layers(),
            imageOffset: VkOffset3D::default(),
            imageExtent: VkExtent3D {
                width: self.width,
                height: self.height,
                depth: 1,
            },
        };
        let barrier = VkMemoryBarrier {
            sType: ST_MEMORY_BARRIER,
            pNext: null(),
            srcAccessMask: ACCESS_TRANSFER_WRITE_BIT,
            dstAccessMask: ACCESS_HOST_READ_BIT,
        };
        // SAFETY: the command buffer is recording; the image is in
        // TRANSFER_SRC_OPTIMAL (the render pass's final layout, made visible
        // to transfer by its outgoing dependency); the buffer holds w*h*4.
        unsafe {
            (self.dev.cmd_copy_image_to_buffer)(
                self.cmd,
                image,
                IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                buffer,
                1,
                &region,
            );
            // Make the copy's writes visible to the host read below.
            (self.dev.cmd_pipeline_barrier)(
                self.cmd,
                PIPELINE_STAGE_TRANSFER_BIT,
                PIPELINE_STAGE_HOST_BIT,
                0,
                1,
                &barrier,
                0,
                null(),
                0,
                null(),
            );
        }
        self.submit_and_wait()?;
        let n = (self.width * self.height) as usize;
        let mut out = vec![0u32; n];
        // SAFETY: the mapping holds at least n*4 bytes (the buffer's size),
        // the GPU finished writing it (fence) and the barrier made the writes
        // host-visible; `out` holds n u32s. Bytes are B, G, R, A per pixel,
        // which read as a little-endian u32 are 0xAARRGGBB.
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr().cast::<u8>(), n * 4);
        }
        if self.format == FORMAT_R8G8B8A8_UNORM {
            for p in &mut out {
                *p = (*p & 0xFF00_FF00) | (*p & 0xFF) << 16 | (*p >> 16) & 0xFF;
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ building

    /// Device, pipeline, atlas and command objects: everything but the
    /// target. On error, `Drop` releases whatever was made.
    fn build(
        instance: Instance,
        format: VkEnum,
        atlas: &Atlas,
        window: bool,
    ) -> Result<VkRenderer, Error> {
        let (physical, queue_family, device_name) = pick_device(&instance, window)?;
        let ifn = &instance.fns;
        let mut memory = VkPhysicalDeviceMemoryProperties {
            memoryTypeCount: 0,
            memoryTypes: [VkMemoryType::default(); 32],
            memoryHeapCount: 0,
            memoryHeaps: [VkMemoryHeap::default(); 16],
        };
        // SAFETY: a live physical device and a correctly sized out-struct.
        unsafe { (ifn.get_physical_device_memory_properties)(physical, &mut memory) };

        let priority = 1.0f32;
        let queue_info = VkDeviceQueueCreateInfo {
            sType: ST_DEVICE_QUEUE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            queueFamilyIndex: queue_family,
            queueCount: 1,
            pQueuePriorities: &priority,
        };
        let extensions = [KHR_SWAPCHAIN.as_ptr()];
        let info = VkDeviceCreateInfo {
            sType: ST_DEVICE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &queue_info,
            enabledLayerCount: 0,
            ppEnabledLayerNames: null(),
            enabledExtensionCount: if window { 1 } else { 0 },
            ppEnabledExtensionNames: extensions.as_ptr(),
            pEnabledFeatures: null(),
        };
        let mut device: VkDevice = null_mut();
        // SAFETY: `info` and what it points to live across the call.
        check("vkCreateDevice", unsafe {
            (ifn.create_device)(physical, &info, null(), &mut device)
        })?;
        let gdpa = ifn.get_device_proc_addr;
        // SAFETY: a live device and NUL-terminated names.
        let dev = match DeviceFns::load(|name| unsafe { gdpa(device, name.as_ptr()) }) {
            Ok(d) => d,
            Err(sym) => {
                // SAFETY: nothing was created from the device yet.
                unsafe { (ifn.destroy_device)(device, null()) };
                return Err(Error::MissingSymbol(sym));
            }
        };
        let mut queue: VkQueue = null_mut();
        // SAFETY: family and index 0 were requested at device creation.
        unsafe { (dev.get_device_queue)(device, queue_family, 0, &mut queue) };

        let mut r = VkRenderer {
            dev,
            device,
            queue,
            physical,
            queue_family,
            memory,
            device_name,
            format,
            width: 0,
            height: 0,
            render_pass: NULL_HANDLE,
            set_layout: NULL_HANDLE,
            descriptor_pool: NULL_HANDLE,
            descriptor_set: NULL_HANDLE,
            pipeline_layout: NULL_HANDLE,
            pipeline: NULL_HANDLE,
            command_pool: NULL_HANDLE,
            cmd: null_mut(),
            fence: NULL_HANDLE,
            fence_pending: std::cell::Cell::new(false),
            atlas_image: NULL_HANDLE,
            atlas_memory: NULL_HANDLE,
            atlas_view: NULL_HANDLE,
            atlas_width: atlas.width(),
            atlas_height: atlas.height(),
            metrics: atlas.metrics(),
            slots_per_row: atlas.slots_per_row(),
            staging: HostBuffer::EMPTY,
            pending_band: None,
            instances: HostBuffer::EMPTY,
            grid: None,
            offscreen: None,
            window: None,
            needs_draw: true,
            drawn: false,
            instance,
        };
        r.create_commands()?;
        r.create_pipeline(window)?;
        r.create_atlas(atlas)?;
        Ok(r)
    }

    fn create_commands(&mut self) -> Result<(), Error> {
        let info = VkCommandPoolCreateInfo {
            sType: ST_COMMAND_POOL_CREATE_INFO,
            pNext: null(),
            flags: COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
            queueFamilyIndex: self.queue_family,
        };
        // SAFETY (this function): live device; each info struct lives across
        // its call; out-pointers are valid.
        check("vkCreateCommandPool", unsafe {
            (self.dev.create_command_pool)(self.device, &info, null(), &mut self.command_pool)
        })?;
        let alloc = VkCommandBufferAllocateInfo {
            sType: ST_COMMAND_BUFFER_ALLOCATE_INFO,
            pNext: null(),
            commandPool: self.command_pool,
            level: COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: 1,
        };
        check("vkAllocateCommandBuffers", unsafe {
            (self.dev.allocate_command_buffers)(self.device, &alloc, &mut self.cmd)
        })?;
        // Created signalled, so the first draw's wait returns at once.
        let fence = VkFenceCreateInfo {
            sType: ST_FENCE_CREATE_INFO,
            pNext: null(),
            flags: FENCE_CREATE_SIGNALED_BIT,
        };
        check("vkCreateFence", unsafe {
            (self.dev.create_fence)(self.device, &fence, null(), &mut self.fence)
        })
    }

    fn create_pipeline(&mut self, window: bool) -> Result<(), Error> {
        let d = self.device;
        // Set 0, binding 0: the atlas as a sampled image (texelFetch, no
        // sampler).
        let binding = VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptorType: DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            descriptorCount: 1,
            stageFlags: SHADER_STAGE_FRAGMENT_BIT,
            pImmutableSamplers: null(),
        };
        let layout_info = VkDescriptorSetLayoutCreateInfo {
            sType: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            pNext: null(),
            flags: 0,
            bindingCount: 1,
            pBindings: &binding,
        };
        // SAFETY (this function): live device; each info struct and what it
        // points to lives across its call; out-pointers are valid.
        check("vkCreateDescriptorSetLayout", unsafe {
            (self.dev.create_descriptor_set_layout)(d, &layout_info, null(), &mut self.set_layout)
        })?;
        let size = VkDescriptorPoolSize {
            ty: DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            descriptorCount: 1,
        };
        let pool_info = VkDescriptorPoolCreateInfo {
            sType: ST_DESCRIPTOR_POOL_CREATE_INFO,
            pNext: null(),
            flags: 0,
            maxSets: 1,
            poolSizeCount: 1,
            pPoolSizes: &size,
        };
        check("vkCreateDescriptorPool", unsafe {
            (self.dev.create_descriptor_pool)(d, &pool_info, null(), &mut self.descriptor_pool)
        })?;
        let alloc = VkDescriptorSetAllocateInfo {
            sType: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
            pNext: null(),
            descriptorPool: self.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &self.set_layout,
        };
        check("vkAllocateDescriptorSets", unsafe {
            (self.dev.allocate_descriptor_sets)(d, &alloc, &mut self.descriptor_set)
        })?;

        let push = VkPushConstantRange {
            stageFlags: SHADER_STAGE_VERTEX_BIT | SHADER_STAGE_FRAGMENT_BIT,
            offset: 0,
            size: spirv::PUSH_CONSTANT_BYTES,
        };
        let pl_info = VkPipelineLayoutCreateInfo {
            sType: ST_PIPELINE_LAYOUT_CREATE_INFO,
            pNext: null(),
            flags: 0,
            setLayoutCount: 1,
            pSetLayouts: &self.set_layout,
            pushConstantRangeCount: 1,
            pPushConstantRanges: &push,
        };
        check("vkCreatePipelineLayout", unsafe {
            (self.dev.create_pipeline_layout)(d, &pl_info, null(), &mut self.pipeline_layout)
        })?;

        // One colour attachment, cleared, stored, and left in the layout its
        // consumer wants: presentation, or the readback copy.
        let attachment = VkAttachmentDescription {
            flags: 0,
            format: self.format,
            samples: SAMPLE_COUNT_1_BIT,
            loadOp: ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: ATTACHMENT_STORE_OP_STORE,
            stencilLoadOp: ATTACHMENT_LOAD_OP_DONT_CARE,
            stencilStoreOp: ATTACHMENT_STORE_OP_DONT_CARE,
            initialLayout: IMAGE_LAYOUT_UNDEFINED,
            finalLayout: if window {
                IMAGE_LAYOUT_PRESENT_SRC_KHR
            } else {
                IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL
            },
        };
        let color_ref = VkAttachmentReference {
            attachment: 0,
            layout: IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        };
        let subpass = VkSubpassDescription {
            flags: 0,
            pipelineBindPoint: PIPELINE_BIND_POINT_GRAPHICS,
            inputAttachmentCount: 0,
            pInputAttachments: null(),
            colorAttachmentCount: 1,
            pColorAttachments: &color_ref,
            pResolveAttachments: null(),
            pDepthStencilAttachment: null(),
            preserveAttachmentCount: 0,
            pPreserveAttachments: null(),
        };
        let deps = [
            // In: the layout transition and the clear wait for the acquire
            // semaphore (waited at COLOR_ATTACHMENT_OUTPUT) and for any
            // earlier readback copy of the same image (TRANSFER).
            VkSubpassDependency {
                srcSubpass: SUBPASS_EXTERNAL,
                dstSubpass: 0,
                srcStageMask: PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT
                    | PIPELINE_STAGE_TRANSFER_BIT,
                dstStageMask: PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
                srcAccessMask: 0,
                dstAccessMask: ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                dependencyFlags: 0,
            },
            // Out: the colour writes are visible to a later readback copy.
            VkSubpassDependency {
                srcSubpass: 0,
                dstSubpass: SUBPASS_EXTERNAL,
                srcStageMask: PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
                dstStageMask: PIPELINE_STAGE_TRANSFER_BIT,
                srcAccessMask: ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                dstAccessMask: ACCESS_TRANSFER_READ_BIT,
                dependencyFlags: 0,
            },
        ];
        let rp_info = VkRenderPassCreateInfo {
            sType: ST_RENDER_PASS_CREATE_INFO,
            pNext: null(),
            flags: 0,
            attachmentCount: 1,
            pAttachments: &attachment,
            subpassCount: 1,
            pSubpasses: &subpass,
            dependencyCount: deps.len() as u32,
            pDependencies: deps.as_ptr(),
        };
        check("vkCreateRenderPass", unsafe {
            (self.dev.create_render_pass)(d, &rp_info, null(), &mut self.render_pass)
        })?;

        let vert = self.shader_module(&spirv::vertex_shader())?;
        let frag = match self.shader_module(&spirv::fragment_shader()) {
            Ok(f) => f,
            Err(e) => {
                // SAFETY: an unused module from this device.
                unsafe { (self.dev.destroy_shader_module)(d, vert, null()) };
                return Err(e);
            }
        };
        let result = self.create_graphics_pipeline(vert, frag);
        // SAFETY: modules may be destroyed once pipeline creation returned.
        unsafe {
            (self.dev.destroy_shader_module)(d, vert, null());
            (self.dev.destroy_shader_module)(d, frag, null());
        }
        result
    }

    fn shader_module(&self, words: &[u32]) -> Result<VkShaderModule, Error> {
        let info = VkShaderModuleCreateInfo {
            sType: ST_SHADER_MODULE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            codeSize: words.len() * 4,
            pCode: words.as_ptr(),
        };
        let mut m = NULL_HANDLE;
        // SAFETY: `words` is a SPIR-V module of codeSize bytes, u32-aligned.
        check("vkCreateShaderModule", unsafe {
            (self.dev.create_shader_module)(self.device, &info, null(), &mut m)
        })?;
        Ok(m)
    }

    fn create_graphics_pipeline(
        &mut self,
        vert: VkShaderModule,
        frag: VkShaderModule,
    ) -> Result<(), Error> {
        let stages = [
            VkPipelineShaderStageCreateInfo {
                sType: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                pNext: null(),
                flags: 0,
                stage: SHADER_STAGE_VERTEX_BIT,
                module: vert,
                pName: b"main\0".as_ptr(),
                pSpecializationInfo: null(),
            },
            VkPipelineShaderStageCreateInfo {
                sType: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                pNext: null(),
                flags: 0,
                stage: SHADER_STAGE_FRAGMENT_BIT,
                module: frag,
                pName: b"main\0".as_ptr(),
                pSpecializationInfo: null(),
            },
        ];
        // Binding 0: one CellInstance (16 bytes) per instance, read as a
        // uvec4 at location 0.
        let binding = VkVertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<CellInstance>() as u32,
            inputRate: VERTEX_INPUT_RATE_INSTANCE,
        };
        let attribute = VkVertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: FORMAT_R32G32B32A32_UINT,
            offset: 0,
        };
        let vertex_input = VkPipelineVertexInputStateCreateInfo {
            sType: ST_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &binding,
            vertexAttributeDescriptionCount: 1,
            pVertexAttributeDescriptions: &attribute,
        };
        let assembly = VkPipelineInputAssemblyStateCreateInfo {
            sType: ST_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            topology: PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            primitiveRestartEnable: 0,
        };
        // Viewport and scissor are dynamic, so a resize keeps the pipeline.
        let viewport = VkPipelineViewportStateCreateInfo {
            sType: ST_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            viewportCount: 1,
            pViewports: null(),
            scissorCount: 1,
            pScissors: null(),
        };
        let raster = VkPipelineRasterizationStateCreateInfo {
            sType: ST_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            depthClampEnable: 0,
            rasterizerDiscardEnable: 0,
            polygonMode: POLYGON_MODE_FILL,
            cullMode: CULL_MODE_NONE,
            frontFace: FRONT_FACE_COUNTER_CLOCKWISE,
            depthBiasEnable: 0,
            depthBiasConstantFactor: 0.0,
            depthBiasClamp: 0.0,
            depthBiasSlopeFactor: 0.0,
            lineWidth: 1.0,
        };
        let multisample = VkPipelineMultisampleStateCreateInfo {
            sType: ST_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            rasterizationSamples: SAMPLE_COUNT_1_BIT,
            sampleShadingEnable: 0,
            minSampleShading: 0.0,
            pSampleMask: null(),
            alphaToCoverageEnable: 0,
            alphaToOneEnable: 0,
        };
        let blend_attachment = VkPipelineColorBlendAttachmentState {
            blendEnable: 0,
            srcColorBlendFactor: 0,
            dstColorBlendFactor: 0,
            colorBlendOp: 0,
            srcAlphaBlendFactor: 0,
            dstAlphaBlendFactor: 0,
            alphaBlendOp: 0,
            colorWriteMask: COLOR_COMPONENT_RGBA,
        };
        let blend = VkPipelineColorBlendStateCreateInfo {
            sType: ST_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            logicOpEnable: 0,
            logicOp: 0,
            attachmentCount: 1,
            pAttachments: &blend_attachment,
            blendConstants: [0.0; 4],
        };
        let dynamic_states = [DYNAMIC_STATE_VIEWPORT, DYNAMIC_STATE_SCISSOR];
        let dynamic = VkPipelineDynamicStateCreateInfo {
            sType: ST_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            dynamicStateCount: dynamic_states.len() as u32,
            pDynamicStates: dynamic_states.as_ptr(),
        };
        let info = VkGraphicsPipelineCreateInfo {
            sType: ST_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            stageCount: 2,
            pStages: stages.as_ptr(),
            pVertexInputState: &vertex_input,
            pInputAssemblyState: &assembly,
            pTessellationState: null(),
            pViewportState: &viewport,
            pRasterizationState: &raster,
            pMultisampleState: &multisample,
            pDepthStencilState: null(),
            pColorBlendState: &blend,
            pDynamicState: &dynamic,
            layout: self.pipeline_layout,
            renderPass: self.render_pass,
            subpass: 0,
            basePipelineHandle: NULL_HANDLE,
            basePipelineIndex: -1,
        };
        // SAFETY: every struct `info` points to lives until the call returns.
        check("vkCreateGraphicsPipelines", unsafe {
            (self.dev.create_graphics_pipelines)(
                self.device,
                NULL_HANDLE,
                1,
                &info,
                null(),
                &mut self.pipeline,
            )
        })
    }

    /// The atlas image, its view and descriptor, the staging mirror, and a
    /// full first upload (so the image never holds undefined texels).
    fn create_atlas(&mut self, atlas: &Atlas) -> Result<(), Error> {
        let (w, h) = (atlas.width(), atlas.height());
        let (image, memory) = self.device_image(
            w,
            h,
            FORMAT_R8_UNORM,
            IMAGE_USAGE_TRANSFER_DST_BIT | IMAGE_USAGE_SAMPLED_BIT,
        )?;
        self.atlas_image = image;
        self.atlas_memory = memory;
        self.atlas_view = self.image_view(image, FORMAT_R8_UNORM)?;
        let image_info = VkDescriptorImageInfo {
            sampler: NULL_HANDLE,
            imageView: self.atlas_view,
            imageLayout: IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = VkWriteDescriptorSet {
            sType: ST_WRITE_DESCRIPTOR_SET,
            pNext: null(),
            dstSet: self.descriptor_set,
            dstBinding: 0,
            dstArrayElement: 0,
            descriptorCount: 1,
            descriptorType: DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            pImageInfo: &image_info,
            pBufferInfo: null(),
            pTexelBufferView: null(),
        };
        // SAFETY: a live set, not in use by any command buffer yet.
        unsafe { (self.dev.update_descriptor_sets)(self.device, 1, &write, 0, null()) };

        self.staging =
            self.host_buffer((w as u64 * h as u64).max(4), BUFFER_USAGE_TRANSFER_SRC_BIT)?;
        let pixels = atlas.pixels();
        // SAFETY: the mapping holds w*h bytes and `pixels` is w*h bytes.
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.staging.ptr, pixels.len()) };

        self.begin()?;
        self.image_barrier(
            self.atlas_image,
            IMAGE_LAYOUT_UNDEFINED,
            IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            PIPELINE_STAGE_TOP_OF_PIPE_BIT,
            0,
            PIPELINE_STAGE_TRANSFER_BIT,
            ACCESS_TRANSFER_WRITE_BIT,
        );
        self.copy_atlas_rows(0, h);
        self.image_barrier(
            self.atlas_image,
            IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            PIPELINE_STAGE_TRANSFER_BIT,
            ACCESS_TRANSFER_WRITE_BIT,
            PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
            ACCESS_SHADER_READ_BIT,
        );
        self.submit_and_wait()
    }

    fn make_offscreen(&self, width: u32, height: u32) -> Result<Offscreen, Error> {
        let mut o = Offscreen::EMPTY;
        match self.fill_offscreen(&mut o, width, height) {
            Ok(()) => Ok(o),
            Err(e) => {
                self.free_offscreen(&mut o);
                Err(e)
            }
        }
    }

    fn fill_offscreen(&self, o: &mut Offscreen, width: u32, height: u32) -> Result<(), Error> {
        let (image, memory) = self.device_image(
            width,
            height,
            self.format,
            IMAGE_USAGE_COLOR_ATTACHMENT_BIT | IMAGE_USAGE_TRANSFER_SRC_BIT,
        )?;
        o.image = image;
        o.memory = memory;
        o.view = self.image_view(image, self.format)?;
        o.framebuffer = self.framebuffer(o.view, width, height)?;
        o.readback = self.host_buffer(
            width as u64 * height as u64 * 4,
            BUFFER_USAGE_TRANSFER_DST_BIT,
        )?;
        Ok(())
    }

    fn free_offscreen(&self, o: &mut Offscreen) {
        // SAFETY: handles from this device (or null, a no-op) that no pending
        // command uses (the callers wait first).
        unsafe {
            (self.dev.destroy_framebuffer)(self.device, o.framebuffer, null());
            (self.dev.destroy_image_view)(self.device, o.view, null());
            (self.dev.destroy_image)(self.device, o.image, null());
            (self.dev.free_memory)(self.device, o.memory, null());
        }
        self.free_host_buffer(&mut o.readback);
        *o = Offscreen::EMPTY;
    }

    /// (Re)build the swapchain at the surface's current size. With a zero
    /// size (minimized) the swapchain is released and left null.
    fn rebuild_swapchain(&mut self, w: &mut Window) -> Result<(), Error> {
        let sf = self.instance.surface_fns.as_ref().expect("window");
        let surface = self.instance.surface;
        // SAFETY (this function): live device, surface and swapchain handles;
        // every info struct lives across its call. The device is idle before
        // anything in use is destroyed.
        check("vkDeviceWaitIdle", unsafe {
            (self.dev.device_wait_idle)(self.device)
        })?;
        self.free_swapchain_images(w);
        let mut caps = VkSurfaceCapabilitiesKHR::default();
        check("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", unsafe {
            (sf.get_physical_device_surface_capabilities)(self.physical, surface, &mut caps)
        })?;
        let extent = if caps.currentExtent.width == u32::MAX {
            VkExtent2D {
                width: self
                    .width
                    .clamp(caps.minImageExtent.width, caps.maxImageExtent.width),
                height: self
                    .height
                    .clamp(caps.minImageExtent.height, caps.maxImageExtent.height),
            }
        } else {
            caps.currentExtent
        };
        let old = w.swapchain;
        w.swapchain = NULL_HANDLE;
        // `stale` stays set until every per-image resource exists: a failure
        // part way leaves a swapchain the next acquire must rebuild, never a
        // half-built one it would index past.
        w.stale = true;
        if extent.width == 0 || extent.height == 0 {
            unsafe { (w.fns.destroy_swapchain)(self.device, old, null()) };
            w.stale = false;
            return Ok(());
        }
        let mut count = caps.minImageCount + 1;
        if caps.maxImageCount > 0 {
            count = count.min(caps.maxImageCount);
        }
        let composite = if caps.supportedCompositeAlpha & COMPOSITE_ALPHA_OPAQUE_BIT_KHR != 0 {
            COMPOSITE_ALPHA_OPAQUE_BIT_KHR
        } else {
            // Lowest supported bit; the spec guarantees at least one.
            caps.supportedCompositeAlpha & caps.supportedCompositeAlpha.wrapping_neg()
        };
        let info = VkSwapchainCreateInfoKHR {
            sType: ST_SWAPCHAIN_CREATE_INFO_KHR,
            pNext: null(),
            flags: 0,
            surface,
            minImageCount: count,
            imageFormat: self.format,
            imageColorSpace: w.color_space,
            imageExtent: extent,
            imageArrayLayers: 1,
            imageUsage: IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
            imageSharingMode: SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: null(),
            preTransform: caps.currentTransform,
            compositeAlpha: composite,
            presentMode: w.mode,
            clipped: 1,
            oldSwapchain: old,
        };
        let mut swapchain = NULL_HANDLE;
        let r = unsafe { (w.fns.create_swapchain)(self.device, &info, null(), &mut swapchain) };
        // The old swapchain is retired either way; nothing uses it now.
        unsafe { (w.fns.destroy_swapchain)(self.device, old, null()) };
        check("vkCreateSwapchainKHR", r)?;
        w.swapchain = swapchain;
        w.extent = extent;
        self.width = extent.width;
        self.height = extent.height;
        let fns = &w.fns;
        w.images = enumerate("vkGetSwapchainImagesKHR", NULL_HANDLE, |n, p| unsafe {
            (fns.get_swapchain_images)(self.device, swapchain, n, p)
        })?;
        for i in 0..w.images.len() {
            let view = self.image_view(w.images[i], self.format)?;
            w.views.push(view);
            let fb = self.framebuffer(view, extent.width, extent.height)?;
            w.framebuffers.push(fb);
            let s = self.semaphore()?;
            w.render_done.push(s);
        }
        w.stale = false;
        Ok(())
    }

    /// Release what is sized by the swapchain (not the swapchain itself).
    fn free_swapchain_images(&self, w: &mut Window) {
        // SAFETY: handles from this device, idle (callers wait first).
        unsafe {
            for &fb in &w.framebuffers {
                (self.dev.destroy_framebuffer)(self.device, fb, null());
            }
            for &v in &w.views {
                (self.dev.destroy_image_view)(self.device, v, null());
            }
            for &s in &w.render_done {
                (self.dev.destroy_semaphore)(self.device, s, null());
            }
        }
        w.framebuffers.clear();
        w.views.clear();
        w.render_done.clear();
        w.images.clear();
    }

    // ------------------------------------------------------------- drawing

    /// Acquire the next swapchain image, rebuilding the swapchain when it is
    /// stale or out of date. `Ok(None)`: no swapchain (minimized).
    fn acquire(&mut self, w: &mut Window) -> Result<Option<u32>, Error> {
        for _ in 0..3 {
            if w.stale || w.swapchain == NULL_HANDLE {
                self.rebuild_swapchain(w)?;
                self.needs_draw = true;
            }
            if w.swapchain == NULL_HANDLE {
                return Ok(None);
            }
            let mut index = 0u32;
            // SAFETY: live swapchain; the semaphore is unsignalled (the draw
            // that last waited on it has completed: its fence was waited).
            let r = unsafe {
                (w.fns.acquire_next_image)(
                    self.device,
                    w.swapchain,
                    u64::MAX,
                    w.image_available,
                    NULL_HANDLE,
                    &mut index,
                )
            };
            match r {
                VK_SUCCESS => return Ok(Some(index)),
                // Usable now; rebuild after this frame.
                VK_SUBOPTIMAL_KHR => {
                    w.stale = true;
                    return Ok(Some(index));
                }
                VK_ERROR_OUT_OF_DATE_KHR => w.stale = true,
                code => {
                    return Err(Error::Vulkan {
                        call: "vkAcquireNextImageKHR",
                        code,
                    })
                }
            }
        }
        Ok(None)
    }

    /// Host-side uploads: the atlas band into the staging mirror, and the
    /// instance data (every row after a grid change, else damaged rows).
    /// The GPU is idle with respect to both buffers (the fence was waited).
    fn stage(
        &mut self,
        frame: &Frame,
        atlas: &Atlas,
        band: Option<Range<u32>>,
        regrid: bool,
        stats: &mut DrawStats,
    ) -> Result<(), Error> {
        if let Some(b) = band {
            let w = self.atlas_width as usize;
            let bytes = &atlas.pixels()[b.start as usize * w..b.end as usize * w];
            // SAFETY: the staging mapping mirrors the whole atlas (w*h
            // bytes), so rows start..end fit at the same offset.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.staging.ptr.add(b.start as usize * w),
                    bytes.len(),
                )
            };
            self.pending_band = Some(match self.pending_band.take() {
                None => b,
                Some(p) => p.start.min(b.start)..p.end.max(b.end),
            });
        }

        let (rows, cols) = (frame.rows(), frame.cols());
        let row_bytes = cols * std::mem::size_of::<CellInstance>();
        if regrid {
            let need = (rows * row_bytes).max(16) as u64;
            if self.instances.size < need {
                let mut old = std::mem::replace(&mut self.instances, HostBuffer::EMPTY);
                self.free_host_buffer(&mut old);
                self.instances = self.host_buffer(need, BUFFER_USAGE_VERTEX_BUFFER_BIT)?;
            }
            self.grid = Some((rows, cols));
        }
        for r in 0..rows {
            if regrid || frame.damage()[r] {
                let src = frame.row(r);
                // SAFETY: CellInstance is repr(C), 16 bytes of u32s, no
                // padding; the mapping holds rows*row_bytes bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        src.as_ptr().cast::<u8>(),
                        self.instances.ptr.add(r * row_bytes),
                        row_bytes,
                    )
                };
                stats.rows_uploaded += 1;
            }
        }
        Ok(())
    }

    fn record_and_submit(
        &mut self,
        frame: &Frame,
        window: Option<&Window>,
        image_index: u32,
        stats: &mut DrawStats,
    ) -> Result<(), Error> {
        let (framebuffer, width, height) = match (window, &self.offscreen) {
            (Some(w), _) => (
                w.framebuffers[image_index as usize],
                w.extent.width,
                w.extent.height,
            ),
            (None, Some(o)) => (o.framebuffer, self.width, self.height),
            (None, None) => unreachable!("a renderer has a window or an offscreen target"),
        };
        self.begin()?;
        if let Some(b) = self.pending_band.take() {
            // Round the start down to a multiple of 4 rows, so the buffer
            // offset (start * width) is 4-byte aligned whatever the width;
            // the staging mirror holds those rows too.
            let start = b.start - b.start % 4;
            self.image_barrier(
                self.atlas_image,
                IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                0,
                PIPELINE_STAGE_TRANSFER_BIT,
                ACCESS_TRANSFER_WRITE_BIT,
            );
            self.copy_atlas_rows(start, b.end);
            self.image_barrier(
                self.atlas_image,
                IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                PIPELINE_STAGE_TRANSFER_BIT,
                ACCESS_TRANSFER_WRITE_BIT,
                PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                ACCESS_SHADER_READ_BIT,
            );
            stats.atlas_rows_uploaded = b.end - start;
        }

        let bg = frame.background();
        let unorm = |shift: u32| ((bg >> shift) & 0xFF) as f32 / 255.0;
        let clear = VkClearValue {
            float32: [unorm(16), unorm(8), unorm(0), 1.0],
        };
        let area = VkRect2D {
            offset: VkOffset2D::default(),
            extent: VkExtent2D { width, height },
        };
        let begin = VkRenderPassBeginInfo {
            sType: ST_RENDER_PASS_BEGIN_INFO,
            pNext: null(),
            renderPass: self.render_pass,
            framebuffer,
            renderArea: area,
            clearValueCount: 1,
            pClearValues: &clear,
        };
        let viewport = VkViewport {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        let cells = (frame.rows() * frame.cols()) as u32;
        let push: [u32; 6] = [
            frame.cols() as u32,
            self.metrics.cell_w,
            self.metrics.cell_h,
            self.slots_per_row,
            width,
            height,
        ];
        let offset: VkDeviceSize = 0;
        let c = self.cmd;
        // SAFETY: the command buffer is recording; every handle is live and
        // every pointer refers to a local that outlives the call.
        unsafe {
            (self.dev.cmd_begin_render_pass)(c, &begin, SUBPASS_CONTENTS_INLINE);
            if cells > 0 {
                (self.dev.cmd_set_viewport)(c, 0, 1, &viewport);
                (self.dev.cmd_set_scissor)(c, 0, 1, &area);
                (self.dev.cmd_bind_pipeline)(c, PIPELINE_BIND_POINT_GRAPHICS, self.pipeline);
                (self.dev.cmd_bind_descriptor_sets)(
                    c,
                    PIPELINE_BIND_POINT_GRAPHICS,
                    self.pipeline_layout,
                    0,
                    1,
                    &self.descriptor_set,
                    0,
                    null(),
                );
                (self.dev.cmd_bind_vertex_buffers)(c, 0, 1, &self.instances.buffer, &offset);
                (self.dev.cmd_push_constants)(
                    c,
                    self.pipeline_layout,
                    SHADER_STAGE_VERTEX_BIT | SHADER_STAGE_FRAGMENT_BIT,
                    0,
                    spirv::PUSH_CONSTANT_BYTES,
                    push.as_ptr().cast(),
                );
                (self.dev.cmd_draw)(c, 6, cells, 0, 0);
            }
            (self.dev.cmd_end_render_pass)(c);
        }
        check("vkEndCommandBuffer", unsafe {
            (self.dev.end_command_buffer)(c)
        })?;

        let wait_stage = PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
        let submit = match window {
            Some(w) => VkSubmitInfo {
                sType: ST_SUBMIT_INFO,
                pNext: null(),
                waitSemaphoreCount: 1,
                pWaitSemaphores: &w.image_available,
                pWaitDstStageMask: &wait_stage,
                commandBufferCount: 1,
                pCommandBuffers: &self.cmd,
                signalSemaphoreCount: 1,
                pSignalSemaphores: &w.render_done[image_index as usize],
            },
            None => VkSubmitInfo {
                sType: ST_SUBMIT_INFO,
                pNext: null(),
                waitSemaphoreCount: 0,
                pWaitSemaphores: null(),
                pWaitDstStageMask: null(),
                commandBufferCount: 1,
                pCommandBuffers: &self.cmd,
                signalSemaphoreCount: 0,
                pSignalSemaphores: null(),
            },
        };
        self.submit(&submit)
    }

    // ------------------------------------------------------------- helpers

    /// Wait for the last successful submit, if any is outstanding.
    fn wait_fence(&self) -> Result<(), Error> {
        if !self.fence_pending.get() {
            return Ok(());
        }
        // SAFETY: a live fence of this device, with work submitted to signal
        // it (so the wait ends, or the device is lost and it returns).
        check("vkWaitForFences", unsafe {
            (self.dev.wait_for_fences)(self.device, 1, &self.fence, 1, u64::MAX)
        })?;
        self.fence_pending.set(false);
        Ok(())
    }

    /// Reset the fence and submit `info` with it. The fence counts as pending
    /// only once the submit succeeded.
    fn submit(&self, info: &VkSubmitInfo) -> Result<(), Error> {
        // SAFETY: the fence is not in use (waited, or never submitted);
        // `info` and what it points to live across the call.
        unsafe {
            check(
                "vkResetFences",
                (self.dev.reset_fences)(self.device, 1, &self.fence),
            )?;
            check(
                "vkQueueSubmit",
                (self.dev.queue_submit)(self.queue, 1, info, self.fence),
            )?;
        }
        self.fence_pending.set(true);
        Ok(())
    }

    /// Begin recording the (idle) command buffer; this implicitly resets it.
    fn begin(&self) -> Result<(), Error> {
        let info = VkCommandBufferBeginInfo {
            sType: ST_COMMAND_BUFFER_BEGIN_INFO,
            pNext: null(),
            flags: COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            pInheritanceInfo: null(),
        };
        // SAFETY: the buffer is not pending (callers waited on the fence) and
        // its pool allows individual resets. The explicit reset returns it to
        // the initial state even if an earlier failed vkEndCommandBuffer left
        // it recording or invalid, where begin would be invalid usage.
        unsafe {
            check(
                "vkResetCommandBuffer",
                (self.dev.reset_command_buffer)(self.cmd, 0),
            )?;
            check(
                "vkBeginCommandBuffer",
                (self.dev.begin_command_buffer)(self.cmd, &info),
            )
        }
    }

    /// End, submit with the fence, and wait for it.
    fn submit_and_wait(&self) -> Result<(), Error> {
        let submit = VkSubmitInfo {
            sType: ST_SUBMIT_INFO,
            pNext: null(),
            waitSemaphoreCount: 0,
            pWaitSemaphores: null(),
            pWaitDstStageMask: null(),
            commandBufferCount: 1,
            pCommandBuffers: &self.cmd,
            signalSemaphoreCount: 0,
            pSignalSemaphores: null(),
        };
        // SAFETY: a recording command buffer.
        check("vkEndCommandBuffer", unsafe {
            (self.dev.end_command_buffer)(self.cmd)
        })?;
        self.submit(&submit)?;
        self.wait_fence()
    }

    fn image_barrier(
        &self,
        image: VkImage,
        old: VkEnum,
        new: VkEnum,
        src_stage: VkFlags,
        src_access: VkFlags,
        dst_stage: VkFlags,
        dst_access: VkFlags,
    ) {
        let barrier = VkImageMemoryBarrier {
            sType: ST_IMAGE_MEMORY_BARRIER,
            pNext: null(),
            srcAccessMask: src_access,
            dstAccessMask: dst_access,
            oldLayout: old,
            newLayout: new,
            srcQueueFamilyIndex: QUEUE_FAMILY_IGNORED,
            dstQueueFamilyIndex: QUEUE_FAMILY_IGNORED,
            image,
            subresourceRange: color_range(),
        };
        // SAFETY: the command buffer is recording; `barrier` lives across it.
        unsafe {
            (self.dev.cmd_pipeline_barrier)(
                self.cmd,
                src_stage,
                dst_stage,
                0,
                0,
                null(),
                0,
                null(),
                1,
                &barrier,
            )
        };
    }

    /// Record a copy of atlas rows `start..end` from the staging mirror
    /// (same offsets) into the atlas image, which is in TRANSFER_DST_OPTIMAL.
    fn copy_atlas_rows(&self, start: u32, end: u32) {
        let region = VkBufferImageCopy {
            bufferOffset: start as u64 * self.atlas_width as u64,
            bufferRowLength: 0, // tightly packed: imageExtent.width
            bufferImageHeight: 0,
            imageSubresource: color_layers(),
            imageOffset: VkOffset3D {
                x: 0,
                y: start as i32,
                z: 0,
            },
            imageExtent: VkExtent3D {
                width: self.atlas_width,
                height: end - start,
                depth: 1,
            },
        };
        // SAFETY: the command buffer is recording; the region lies inside
        // both the buffer (w*h bytes) and the image.
        unsafe {
            (self.dev.cmd_copy_buffer_to_image)(
                self.cmd,
                self.staging.buffer,
                self.atlas_image,
                IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                1,
                &region,
            )
        };
    }

    /// The first memory type allowed by `bits` with all of `want`.
    fn memory_type(&self, bits: u32, want: VkFlags) -> Option<u32> {
        (0..self.memory.memoryTypeCount).find(|&i| {
            bits & (1 << i) != 0 && self.memory.memoryTypes[i as usize].propertyFlags & want == want
        })
    }

    /// Allocate memory for `req`, trying each property set in `prefs` in turn.
    fn allocate(&self, req: &VkMemoryRequirements, prefs: &[VkFlags]) -> Result<u64, Error> {
        let index = prefs
            .iter()
            .find_map(|&p| self.memory_type(req.memoryTypeBits, p))
            .ok_or(Error::Unsupported("no suitable memory type"))?;
        let info = VkMemoryAllocateInfo {
            sType: ST_MEMORY_ALLOCATE_INFO,
            pNext: null(),
            allocationSize: req.size,
            memoryTypeIndex: index,
        };
        let mut memory = NULL_HANDLE;
        // SAFETY: live device; `info` lives across the call.
        check("vkAllocateMemory", unsafe {
            (self.dev.allocate_memory)(self.device, &info, null(), &mut memory)
        })?;
        Ok(memory)
    }

    /// A host-visible, coherent, persistently mapped buffer.
    fn host_buffer(&self, size: u64, usage: VkFlags) -> Result<HostBuffer, Error> {
        let mut b = HostBuffer::EMPTY;
        let result: Result<(), Error> = (|| {
            let info = VkBufferCreateInfo {
                sType: ST_BUFFER_CREATE_INFO,
                pNext: null(),
                flags: 0,
                size,
                usage,
                sharingMode: SHARING_MODE_EXCLUSIVE,
                queueFamilyIndexCount: 0,
                pQueueFamilyIndices: null(),
            };
            // SAFETY (closure): live device; info structs live across their
            // calls; out-pointers are valid.
            check("vkCreateBuffer", unsafe {
                (self.dev.create_buffer)(self.device, &info, null(), &mut b.buffer)
            })?;
            let mut req = VkMemoryRequirements::default();
            unsafe { (self.dev.get_buffer_memory_requirements)(self.device, b.buffer, &mut req) };
            let host = MEMORY_PROPERTY_HOST_VISIBLE_BIT | MEMORY_PROPERTY_HOST_COHERENT_BIT;
            b.memory = self.allocate(&req, &[host])?;
            check("vkBindBufferMemory", unsafe {
                (self.dev.bind_buffer_memory)(self.device, b.buffer, b.memory, 0)
            })?;
            let mut p: *mut c_void = null_mut();
            check("vkMapMemory", unsafe {
                (self.dev.map_memory)(self.device, b.memory, 0, size, 0, &mut p)
            })?;
            b.ptr = p.cast();
            b.size = size;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(b),
            Err(e) => {
                self.free_host_buffer(&mut b);
                Err(e)
            }
        }
    }

    fn free_host_buffer(&self, b: &mut HostBuffer) {
        // SAFETY: handles from this device (or null, a no-op), idle. Freeing
        // mapped memory unmaps it implicitly.
        unsafe {
            (self.dev.destroy_buffer)(self.device, b.buffer, null());
            (self.dev.free_memory)(self.device, b.memory, null());
        }
        *b = HostBuffer::EMPTY;
    }

    /// A device-local 2D image with one mip level and layer.
    fn device_image(
        &self,
        width: u32,
        height: u32,
        format: VkEnum,
        usage: VkFlags,
    ) -> Result<(VkImage, VkDeviceMemory), Error> {
        let info = VkImageCreateInfo {
            sType: ST_IMAGE_CREATE_INFO,
            pNext: null(),
            flags: 0,
            imageType: IMAGE_TYPE_2D,
            format,
            extent: VkExtent3D {
                width,
                height,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: SAMPLE_COUNT_1_BIT,
            tiling: IMAGE_TILING_OPTIMAL,
            usage,
            sharingMode: SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: null(),
            initialLayout: IMAGE_LAYOUT_UNDEFINED,
        };
        let mut image = NULL_HANDLE;
        // SAFETY: live device; info lives across the call; out-pointers valid.
        check("vkCreateImage", unsafe {
            (self.dev.create_image)(self.device, &info, null(), &mut image)
        })?;
        let mut req = VkMemoryRequirements::default();
        unsafe { (self.dev.get_image_memory_requirements)(self.device, image, &mut req) };
        let bound = self
            .allocate(&req, &[MEMORY_PROPERTY_DEVICE_LOCAL_BIT, 0])
            .and_then(|memory| {
                // SAFETY: a fresh image and allocation sized by its
                // requirements.
                match check("vkBindImageMemory", unsafe {
                    (self.dev.bind_image_memory)(self.device, image, memory, 0)
                }) {
                    Ok(()) => Ok(memory),
                    Err(e) => {
                        unsafe { (self.dev.free_memory)(self.device, memory, null()) };
                        Err(e)
                    }
                }
            });
        match bound {
            Ok(memory) => Ok((image, memory)),
            Err(e) => {
                // SAFETY: an unused image of this device.
                unsafe { (self.dev.destroy_image)(self.device, image, null()) };
                Err(e)
            }
        }
    }

    fn image_view(&self, image: VkImage, format: VkEnum) -> Result<VkImageView, Error> {
        let info = VkImageViewCreateInfo {
            sType: ST_IMAGE_VIEW_CREATE_INFO,
            pNext: null(),
            flags: 0,
            image,
            viewType: IMAGE_VIEW_TYPE_2D,
            format,
            components: VkComponentMapping {
                r: COMPONENT_SWIZZLE_IDENTITY,
                g: COMPONENT_SWIZZLE_IDENTITY,
                b: COMPONENT_SWIZZLE_IDENTITY,
                a: COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_range(),
        };
        let mut view = NULL_HANDLE;
        // SAFETY: live device and image; info lives across the call.
        check("vkCreateImageView", unsafe {
            (self.dev.create_image_view)(self.device, &info, null(), &mut view)
        })?;
        Ok(view)
    }

    fn framebuffer(
        &self,
        view: VkImageView,
        width: u32,
        height: u32,
    ) -> Result<VkFramebuffer, Error> {
        let info = VkFramebufferCreateInfo {
            sType: ST_FRAMEBUFFER_CREATE_INFO,
            pNext: null(),
            flags: 0,
            renderPass: self.render_pass,
            attachmentCount: 1,
            pAttachments: &view,
            width,
            height,
            layers: 1,
        };
        let mut fb = NULL_HANDLE;
        // SAFETY: live device, render pass and view; info lives across it.
        check("vkCreateFramebuffer", unsafe {
            (self.dev.create_framebuffer)(self.device, &info, null(), &mut fb)
        })?;
        Ok(fb)
    }

    fn semaphore(&self) -> Result<VkSemaphore, Error> {
        let info = VkSemaphoreCreateInfo {
            sType: ST_SEMAPHORE_CREATE_INFO,
            pNext: null(),
            flags: 0,
        };
        let mut s = NULL_HANDLE;
        // SAFETY: live device; info lives across the call.
        check("vkCreateSemaphore", unsafe {
            (self.dev.create_semaphore)(self.device, &info, null(), &mut s)
        })?;
        Ok(s)
    }
}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        let d = self.device;
        // SAFETY: after vkDeviceWaitIdle nothing is in flight; every handle
        // was created from this device (null handles are no-ops for every
        // vkDestroy*/vkFree* call used here), and each is destroyed once, in
        // reverse order of creation, before the device itself. The instance
        // (and surface) drop afterwards, as the struct's last field.
        unsafe {
            (self.dev.device_wait_idle)(d);
            if let Some(mut w) = self.window.take() {
                self.free_swapchain_images(&mut w);
                (w.fns.destroy_swapchain)(d, w.swapchain, null());
                (self.dev.destroy_semaphore)(d, w.image_available, null());
            }
            if let Some(mut o) = self.offscreen.take() {
                self.free_offscreen(&mut o);
            }
            let mut instances = std::mem::replace(&mut self.instances, HostBuffer::EMPTY);
            self.free_host_buffer(&mut instances);
            let mut staging = std::mem::replace(&mut self.staging, HostBuffer::EMPTY);
            self.free_host_buffer(&mut staging);
            (self.dev.destroy_image_view)(d, self.atlas_view, null());
            (self.dev.destroy_image)(d, self.atlas_image, null());
            (self.dev.free_memory)(d, self.atlas_memory, null());
            (self.dev.destroy_pipeline)(d, self.pipeline, null());
            (self.dev.destroy_render_pass)(d, self.render_pass, null());
            (self.dev.destroy_pipeline_layout)(d, self.pipeline_layout, null());
            // Frees the descriptor set with it.
            (self.dev.destroy_descriptor_pool)(d, self.descriptor_pool, null());
            (self.dev.destroy_descriptor_set_layout)(d, self.set_layout, null());
            (self.dev.destroy_fence)(d, self.fence, null());
            // Frees the command buffer with it.
            (self.dev.destroy_command_pool)(d, self.command_pool, null());
            (self.dev.destroy_device)(d, null());
        }
    }
}

fn color_range() -> VkImageSubresourceRange {
    VkImageSubresourceRange {
        aspectMask: IMAGE_ASPECT_COLOR_BIT,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

fn color_layers() -> VkImageSubresourceLayers {
    VkImageSubresourceLayers {
        aspectMask: IMAGE_ASPECT_COLOR_BIT,
        mipLevel: 0,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

/// The device to use: one with a graphics queue family (that can present to
/// the instance's surface and has `VK_KHR_swapchain`, for a window),
/// preferring a discrete GPU, then an integrated one. Returns the device,
/// the queue family and the device's name.
fn pick_device(
    instance: &Instance,
    window: bool,
) -> Result<(VkPhysicalDevice, u32, String), Error> {
    let f = &instance.fns;
    let devices = enumerate("vkEnumeratePhysicalDevices", null_mut(), |n, p| {
        // SAFETY: a live instance; two-call idiom out-parameters.
        unsafe { (f.enumerate_physical_devices)(instance.handle, n, p) }
    })?;
    let mut best: Option<(u32, VkPhysicalDevice, u32, String)> = None;
    for &pd in &devices {
        // Boxed: the struct reserves 2 KiB for the limits it does not name.
        let mut props = Box::new(VkPhysicalDeviceProperties {
            apiVersion: 0,
            driverVersion: 0,
            vendorID: 0,
            deviceID: 0,
            deviceType: 0,
            deviceName: [0; 256],
            pipelineCacheUUID: [0; 16],
            rest: [0; 256],
        });
        // SAFETY: a live physical device; `props` is larger than the C struct.
        unsafe { (f.get_physical_device_properties)(pd, &mut *props) };
        let families = {
            let mut n = 0u32;
            // SAFETY: two-call idiom (this command returns void).
            unsafe { (f.get_physical_device_queue_family_properties)(pd, &mut n, null_mut()) };
            let mut v = vec![VkQueueFamilyProperties::default(); n as usize];
            unsafe { (f.get_physical_device_queue_family_properties)(pd, &mut n, v.as_mut_ptr()) };
            v.truncate(n as usize);
            v
        };
        if window {
            let exts = enumerate(
                "vkEnumerateDeviceExtensionProperties",
                VkExtensionProperties {
                    extensionName: [0; 256],
                    specVersion: 0,
                },
                // SAFETY: a live physical device; two-call idiom.
                |n, p| unsafe { (f.enumerate_device_extension_properties)(pd, null(), n, p) },
            )?;
            let name = KHR_SWAPCHAIN.trim_end_matches('\0');
            if !exts.iter().any(|e| c_name(&e.extensionName) == name) {
                continue;
            }
        }
        let mut family = None;
        for (i, q) in families.iter().enumerate() {
            if q.queueFlags & QUEUE_GRAPHICS_BIT == 0 || q.queueCount == 0 {
                continue;
            }
            if window {
                let sf = instance.surface_fns.as_ref().expect("window");
                let mut ok: VkBool32 = 0;
                // SAFETY: live device and surface; a valid family index.
                check("vkGetPhysicalDeviceSurfaceSupportKHR", unsafe {
                    (sf.get_physical_device_surface_support)(
                        pd,
                        i as u32,
                        instance.surface,
                        &mut ok,
                    )
                })?;
                if ok == 0 {
                    continue;
                }
            }
            family = Some(i as u32);
            break;
        }
        let Some(family) = family else { continue };
        let score = match props.deviceType {
            PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => 2,
            PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => 1,
            _ => 0,
        };
        if best.as_ref().map_or(true, |b| score > b.0) {
            best = Some((score, pd, family, c_name(&props.deviceName)));
        }
    }
    best.map(|(_, pd, family, name)| (pd, family, name))
        .ok_or(Error::NoSuitableDevice)
}

/// A window and the Vulkan renderer drawing into it, owned together so the
/// renderer can never outlive the window its surface was made for. The safe
/// way to put [`VkRenderer`] on screen.
pub struct VkWindow {
    // Declared first so it drops first: the surface belongs to `window`.
    renderer: VkRenderer,
    window: crate::win::Window,
}

impl VkWindow {
    /// Make a renderer for `window` at its current client size.
    pub fn new(
        window: crate::win::Window,
        atlas: &Atlas,
        mode: PresentMode,
    ) -> Result<VkWindow, Error> {
        VkWindow::try_new(window, atlas, mode).map_err(|(e, _)| e)
    }

    /// As [`VkWindow::new`], but a failure hands the window back, so the
    /// caller can draw into it another way (after a lost device, say).
    pub fn try_new(
        window: crate::win::Window,
        atlas: &Atlas,
        mode: PresentMode,
    ) -> Result<VkWindow, (Error, crate::win::Window)> {
        let (w, h) = window.client_size();
        // SAFETY: the handles are `window`'s, and `window` is moved into the
        // result next to the renderer, which drops first (field order); on
        // failure the partly built renderer is gone before `window` returns.
        match unsafe {
            VkRenderer::for_window(window.hinstance(), window.hwnd(), w, h, atlas, mode)
        } {
            Ok(renderer) => Ok(VkWindow { renderer, window }),
            Err(e) => Err((e, window)),
        }
    }

    /// Tear the renderer down and keep the window: after the device is lost
    /// (`VK_ERROR_DEVICE_LOST`), before building a new one on it.
    pub fn into_window(self) -> crate::win::Window {
        let VkWindow { renderer, window } = self;
        drop(renderer);
        window
    }

    /// The window.
    pub fn window(&self) -> &crate::win::Window {
        &self.window
    }

    /// The window, for pumping its events.
    pub fn window_mut(&mut self) -> &mut crate::win::Window {
        &mut self.window
    }

    /// The renderer.
    pub fn renderer(&mut self) -> &mut VkRenderer {
        &mut self.renderer
    }
}
