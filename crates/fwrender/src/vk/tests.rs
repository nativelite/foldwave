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
