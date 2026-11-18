//! The Vulkan backend against the CPU reference, pixel for pixel, plus a
//! swapchain smoke test on a real (tiny) window.
//!
//! A machine without Vulkan (or without a usable device) prints why and
//! passes: these tests prove correctness where a GPU exists, and must not
//! make the suite fail where none does.

use super::*;
use crate::{CpuRenderer, Metrics, Palette, TestRasterizer};
use ansi::{Cell, Color, Cursor, Screen, Style};

/// Odd sizes, so an off-by-one in the cell or slot maths shows.
const M: Metrics = Metrics {
    cell_w: 9,
    cell_h: 17,
    baseline: 13,
};

/// Errors that mean "no usable Vulkan here", not "the backend is wrong".
fn unavailable(e: &Error) -> bool {
    matches!(
        e,
        Error::LibraryNotFound(_)
            | Error::MissingSymbol(_)
            | Error::NoSuitableDevice
            | Error::Unsupported(_)
            | Error::Vulkan {
                call: "vkCreateInstance" | "vkCreateWin32SurfaceKHR",
                ..
            }
    )
}

fn styled(fg: Color, bg: Color) -> Style {
    Style {
        fg,
        bg,
        ..Style::default()
    }
}

/// 6 x 20 with indexed and RGB colours, reverse, bold, dim, italic,
/// underline, strike, a wide glyph and its continuation, and a cursor.
fn screen() -> Screen {
    let mut s = Screen::new(6, 20);
    s.write_str(0, 0, "hello, vulkan!", Style::default());
    s.write_str(1, 0, "red", styled(Color::Indexed(1), Color::Default));
    s.write_str(
        1,
        4,
        "rgb",
        styled(Color::Rgb(12, 200, 99), Color::Rgb(40, 0, 120)),
    );
    s.write_str(
        1,
        8,
        "cube",
        styled(Color::Indexed(208), Color::Indexed(236)),
    );
    s.write_str(
        2,
        0,
        "reverse",
        Style {
            reverse: true,
            ..styled(Color::Indexed(4), Color::Indexed(3))
        },
    );
    s.write_str(
        2,
        8,
        "bold",
        Style {
            bold: true,
            ..styled(Color::Indexed(2), Color::Default)
        },
    );
    s.write_str(
        2,
        13,
        "dim",
        Style {
            dim: true,
            italic: true,
            ..Style::default()
        },
    );
    s.write_str(
        3,
        0,
        "underline",
        Style {
            underline: true,
            ..styled(Color::Rgb(255, 255, 0), Color::Rgb(0, 0, 80))
        },
    );
    s.write_str(
        3,
        10,
        "strike",
        Style {
            strike: true,
            ..Style::default()
        },
    );
    s.write_str(
        4,
        0,
        "both",
        Style {
            underline: true,
            strike: true,
            ..styled(Color::Indexed(13), Color::Indexed(22))
        },
    );
    let wide = styled(Color::Indexed(11), Color::Rgb(90, 30, 30));
    s.set(4, 6, Cell::wide('界', wide));
    s.set(4, 7, Cell::continuation(wide));
    // An underlined space: the line with no glyph under it.
    s.write_str(
        5,
        0,
        "   ",
        Style {
            underline: true,
            ..styled(Color::Indexed(9), Color::Indexed(17))
        },
    );
    s.write_str(5, 19, "z", Style::default());
    s.set_cursor(Cursor::new(5, 5));
    s
}

/// Compare within one step per channel; report the first mismatch.
fn assert_matches(what: &str, cpu: &[u32], gpu: &[u32], width: u32) {
    assert_eq!(cpu.len(), gpu.len(), "{what}: sizes differ");
    let mut worst = 0;
    for (i, (&c, &g)) in cpu.iter().zip(gpu).enumerate() {
        for shift in [0, 8, 16, 24] {
            let d = (((c >> shift) & 0xFF) as i32 - ((g >> shift) & 0xFF) as i32).abs();
            worst = worst.max(d);
            assert!(
                d <= 1,
                "{what}: pixel ({}, {}) differs: cpu {c:#010X}, vulkan {g:#010X}",
                i as u32 % width,
                i as u32 / width,
            );
        }
    }
    println!(
        "{what}: {} pixels match (max channel difference {worst})",
        cpu.len()
    );
}

#[test]
fn offscreen_matches_the_cpu_reference() {
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut s = screen();
    let mut frame = Frame::new();
    frame.update(&s, true, &palette, &mut atlas, &mut raster);

    // Larger than the 180 x 102 grid on both axes: margins are tested.
    let (w, h) = (20 * 9 + 13, 6 * 17 + 7);
    let mut gpu = match VkRenderer::offscreen(w, h, &atlas) {
        Ok(r) => r,
        Err(e) if unavailable(&e) => {
            println!("skipping: Vulkan unavailable: {e}");
            return;
        }
        Err(e) => panic!("VkRenderer::offscreen: {e}"),
    };
    println!("vulkan device: {}", gpu.device_name());
    let mut cpu = CpuRenderer::new(w, h);

    cpu.draw(&frame, &atlas);
    let stats = gpu.draw(&frame, &mut atlas).expect("draw");
    assert!(!stats.skipped);
    assert_eq!(stats.rows_uploaded, 6, "first draw uploads every row");
    frame.clear_damage();
    assert_matches("first frame", cpu.pixels(), &gpu.read_pixels().unwrap(), w);

    // One row changes, with glyphs new to the atlas: only that row and the
    // atlas band travel, and the picture still matches.
    s.write_str(
        3,
        2,
        "Q#",
        Style {
            bold: true,
            ..styled(Color::Rgb(250, 128, 3), Color::Indexed(19))
        },
    );
    assert_eq!(frame.update(&s, true, &palette, &mut atlas, &mut raster), 1);
    cpu.draw(&frame, &atlas);
    let stats = gpu.draw(&frame, &mut atlas).expect("second draw");
    assert!(!stats.skipped);
    assert_eq!(stats.rows_uploaded, 1);
    assert!(stats.atlas_rows_uploaded > 0, "new glyphs upload a band");
    frame.clear_damage();
    assert_matches(
        "after one row",
        cpu.pixels(),
        &gpu.read_pixels().unwrap(),
        w,
    );

    // No damage and a clean atlas: nothing to do.
    let stats = gpu.draw(&frame, &mut atlas).expect("idle draw");
    assert!(stats.skipped, "an undamaged frame is skipped");
    assert_eq!(stats.rows_uploaded, 0);

    // A resize redraws (never skips) and still matches, margins included.
    let (w2, h2) = (150, 120);
    gpu.resize(w2, h2).expect("resize");
    cpu.resize(w2, h2);
    frame.damage_all();
    cpu.draw(&frame, &atlas);
    let stats = gpu.draw(&frame, &mut atlas).expect("draw after resize");
    assert!(!stats.skipped);
    frame.clear_damage();
    assert_matches(
        "after resize",
        cpu.pixels(),
        &gpu.read_pixels().unwrap(),
        w2,
    );
}

#[test]
fn the_wrong_atlas_is_rejected() {
    let atlas = Atlas::new(M, 256, 256);
    let mut gpu = match VkRenderer::offscreen(32, 32, &atlas) {
        Ok(r) => r,
        Err(e) if unavailable(&e) => {
            println!("skipping: Vulkan unavailable: {e}");
            return;
        }
        Err(e) => panic!("VkRenderer::offscreen: {e}"),
    };
    let mut other = Atlas::new(M, 128, 256);
    let frame = Frame::new();
    assert!(matches!(
        gpu.draw(&frame, &mut other),
        Err(Error::InvalidArgument(_))
    ));
    assert!(
        matches!(gpu.read_pixels(), Err(Error::InvalidArgument(_))),
        "nothing drawn yet"
    );
}

#[cfg(windows)]
#[test]
fn a_window_swapchain_draws_resizes_and_draws_again() {
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut s = Screen::new(4, 8);
    let mut frame = Frame::new();
    frame.update(&s, true, &palette, &mut atlas, &mut raster);

    let mut window = match crate::win::Window::new("fwrender vk test", 96, 80) {
        Ok(w) => w,
        Err(e) => {
            println!("skipping: could not create a window: {e}");
            return;
        }
    };
    let mut events = Vec::new();
    let (w, h) = window.client_size();
    // SAFETY: the window outlives the renderer (dropped after it below).
    let made = unsafe {
        VkRenderer::for_window(
            window.hinstance(),
            window.hwnd(),
            w,
            h,
            &atlas,
            PresentMode::Fifo,
        )
    };
    let mut gpu = match made {
        Ok(r) => r,
        Err(e) if unavailable(&e) => {
            println!("skipping: Vulkan presentation unavailable: {e}");
            return;
        }
        Err(e) => panic!("VkRenderer::for_window: {e}"),
    };
    println!(
        "vulkan device: {}, target {:?}",
        gpu.device_name(),
        gpu.size()
    );

    let mut drawn = 0;
    for i in 0..10 {
        window.poll(&mut events);
        let text = format!("frame {i}");
        s.write_str(i % 4, 0, &text, Style::default());
        frame.update(&s, i % 3 == 0, &palette, &mut atlas, &mut raster);
        let stats = gpu.draw(&frame, &mut atlas).expect("draw");
        frame.clear_damage();
        if !stats.skipped {
            drawn += 1;
        }
    }
    assert!(drawn > 0, "at least one frame reached the swapchain");

    window.set_client_size(130, 70).expect("resize the window");
    window.poll(&mut events);
    gpu.resize(130, 70).expect("resize");
    s.write_str(0, 0, "resized", Style::default());
    frame.update(&s, true, &palette, &mut atlas, &mut raster);
    let stats = gpu.draw(&frame, &mut atlas).expect("draw after resize");
    assert!(!stats.skipped, "a resize is followed by a real draw");
    assert_eq!(gpu.size(), (130, 70), "the swapchain follows the window");
    frame.clear_damage();
    for _ in 0..3 {
        window.poll(&mut events);
        gpu.draw(&frame, &mut atlas).expect("draw");
    }
    drop(gpu);
    drop(window);
}

/// A Vulkan window on Wayland needs a driver that exports its frames and a
/// compositor that takes them. Where either refuses, the window comes back,
/// which is what lets a caller fall back to the CPU renderer; where both
/// agree, a frame is drawn and shown.
#[cfg(target_os = "linux")]
#[test]
fn a_vulkan_window_on_wayland_draws_or_hands_the_window_back() {
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut s = Screen::new(4, 8);
    s.write_str(0, 0, "wayland", Style::default());
    let mut frame = Frame::new();
    frame.update(&s, true, &palette, &mut atlas, &mut raster);
    let window = match crate::wl::Window::new("fwrender vk test", 96, 80) {
        Ok(w) => w,
        Err(e) => {
            println!("skipping: could not create a window: {e}");
            return;
        }
    };
    match VkWindow::try_new(window, &atlas, PresentMode::Fifo) {
        Err((e, window)) => {
            assert!(
                unavailable(&e),
                "refused for a reason that is not a fault: {e}"
            );
            assert_eq!(window.client_size(), (96, 80));
            println!("no Vulkan frames here ({e}); the window came back");
        }
        Ok(mut v) => {
            println!("vulkan device: {}", v.renderer().device_name());
            let stats = v.renderer().draw(&frame, &mut atlas).expect("draw");
            assert!(!stats.skipped, "the first frame is drawn and handed over");
            let mut events = Vec::new();
            v.window_mut().poll(&mut events);
        }
    }
}

/// A sink that hands out its images in turn and remembers what it was shown.
#[cfg(target_os = "linux")]
struct Recorder {
    next: usize,
    images: usize,
    shown: std::rc::Rc<std::cell::RefCell<Vec<usize>>>,
    rebuilt: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(target_os = "linux")]
impl FrameSink for Recorder {
    fn acquire(&mut self) -> Option<usize> {
        let i = self.next;
        self.next = (self.next + 1) % self.images;
        Some(i)
    }
    fn present(&mut self, index: usize) -> Result<(), Error> {
        self.shown.borrow_mut().push(index);
        Ok(())
    }
    fn rebuild(&mut self, frames: Vec<Dmabuf>) -> Result<(), Error> {
        self.rebuilt.set(frames.len());
        Ok(())
    }
}

/// Read a frame out of its dmabuf the way a compositor without a GPU would:
/// by mapping it. `None` when the driver's dmabuf cannot be mapped.
#[cfg(target_os = "linux")]
fn mapped(frame: &Dmabuf) -> Option<Vec<u32>> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn mmap(
            addr: *mut std::ffi::c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut std::ffi::c_void;
        fn munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
    }
    let len = frame.offset as usize + frame.stride as usize * frame.height as usize;
    // SAFETY: a read-only shared mapping of a descriptor we hold, checked
    // for failure, read within its length and unmapped before returning.
    unsafe {
        let map = mmap(std::ptr::null_mut(), len, 1, 1, frame.fd.as_raw_fd(), 0);
        if map as isize == -1 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(map.cast::<u8>(), len);
        let mut out = Vec::with_capacity((frame.width * frame.height) as usize);
        for y in 0..frame.height as usize {
            let row = frame.offset as usize + y * frame.stride as usize;
            for x in 0..frame.width as usize {
                let p = &bytes[row + x * 4..row + x * 4 + 4];
                out.push(u32::from_le_bytes([p[0], p[1], p[2], p[3]]));
            }
        }
        munmap(map, len);
        Some(out)
    }
}

#[cfg(target_os = "linux")]
#[test]
fn exported_frames_hold_what_the_cpu_reference_draws() {
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut s = Screen::new(5, 12);
    let (w, h) = (12 * M.cell_w, 5 * M.cell_h);
    s.write_str(0, 0, "exported", Style::default());
    s.write_str(2, 3, "frames", styled(Color::Indexed(2), Color::Indexed(4)));
    let mut frame = Frame::new();
    frame.update(&s, true, &palette, &mut atlas, &mut raster);

    let mut gpu = match VkRenderer::exporting(w, h, &atlas, 2) {
        Ok(r) => r,
        Err(e) if unavailable(&e) => {
            println!("skipping: no exporting renderer: {e}");
            return;
        }
        Err(e) => panic!("VkRenderer::exporting: {e}"),
    };
    let shown = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let rebuilt = std::rc::Rc::new(std::cell::Cell::new(0));
    gpu.set_sink(Box::new(Recorder {
        next: 0,
        images: 2,
        shown: shown.clone(),
        rebuilt: rebuilt.clone(),
    }));
    let frames = gpu.frames().expect("frames");
    assert_eq!(frames.len(), 2);
    assert!(frames
        .iter()
        .all(|f| f.stride >= w * 4 && (f.width, f.height) == (w, h)));

    let mut cpu = CpuRenderer::new(w, h);
    let mut compare = |gpu: &mut VkRenderer, frame: &mut Frame, atlas: &mut Atlas, what: &str| {
        let stats = gpu.draw(frame, atlas).expect("draw");
        assert!(!stats.skipped, "{what}: drawn");
        cpu.draw(frame, atlas);
        frame.clear_damage();
        let index = *shown.borrow().last().expect("the sink was shown a frame");
        let Some(got) = mapped(&gpu.frames().expect("frames")[index]) else {
            println!("skipping the comparison: this driver's dmabuf cannot be mapped");
            return;
        };
        let worst = got
            .iter()
            .zip(cpu.pixels())
            .flat_map(|(a, b)| {
                (0..3).map(move |c| (a >> (8 * c) & 0xFF).abs_diff(b >> (8 * c) & 0xFF))
            })
            .max()
            .unwrap();
        println!(
            "{what}: image {index}, {} pixels, max channel difference {worst}",
            got.len()
        );
        assert!(worst <= 1, "{what}: channel difference {worst}");
    };
    compare(&mut gpu, &mut frame, &mut atlas, "first frame");
    // The second frame goes to the other image, which has never been drawn:
    // it must be whole, not only the row that changed.
    s.write_str(4, 0, "second", Style::default());
    frame.update(&s, false, &palette, &mut atlas, &mut raster);
    compare(&mut gpu, &mut frame, &mut atlas, "second frame");
    assert_eq!(*shown.borrow(), [0, 1]);

    // Nothing changed: nothing is drawn or shown.
    assert!(gpu.draw(&frame, &mut atlas).expect("draw").skipped);
    assert_eq!(shown.borrow().len(), 2);

    // A new size: new images, and the sink is told.
    let (w2, h2) = (10 * M.cell_w, 4 * M.cell_h);
    gpu.resize(w2, h2).expect("resize");
    assert_eq!(rebuilt.get(), 2);
    assert_eq!(gpu.size(), (w2, h2));
}
