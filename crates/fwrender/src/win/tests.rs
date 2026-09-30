//! Tests for the Windows layer. They need an interactive desktop session and
//! the Consolas font, which ships with every Windows since Vista.

use super::*;
use crate::{Atlas, CpuRenderer, Frame, Palette, Rasterizer, BOLD};
use ansi::{Screen, Style};
use std::time::{Duration, Instant};

fn raster(r: &mut GdiRasterizer, ch: char, style: u8, columns: u32) -> Vec<u8> {
    let m = r.metrics();
    let mut out = vec![0u8; (columns * m.cell_w * m.cell_h) as usize];
    r.rasterize(ch, style, columns, &mut out);
    out
}

#[test]
fn gdi_rasterizer_metrics_and_coverage() {
    let mut r = GdiRasterizer::new("Consolas", 16).expect("Consolas at 16 px");
    let m = r.metrics();
    assert!((6..=12).contains(&m.cell_w), "{m:?}");
    assert!((14..=24).contains(&m.cell_h), "{m:?}");
    assert!(m.baseline < m.cell_h, "{m:?}");

    let regular = raster(&mut r, 'M', 0, 1);
    assert!(regular.iter().any(|&a| a > 128), "'M' has solid ink");
    assert!(raster(&mut r, ' ', 0, 1).iter().all(|&a| a == 0), "space");
    let bold = raster(&mut r, 'M', BOLD, 1);
    assert_ne!(regular, bold, "bold differs");
    let ink = |v: &[u8]| v.iter().map(|&a| a as u32).sum::<u32>();
    assert!(ink(&bold) > ink(&regular), "bold has more ink");
    // Deterministic: the DIB is cleared between glyphs.
    assert_eq!(raster(&mut r, 'M', 0, 1), regular);
}

#[test]
fn gdi_rasterizer_wide_glyph_stays_in_its_buffer() {
    let mut r = GdiRasterizer::new("Consolas", 16).unwrap();
    let m = r.metrics();
    // A guard byte past the end catches any write outside the slot.
    let len = (2 * m.cell_w * m.cell_h) as usize;
    let mut out = vec![0u8; len + 1];
    out[len] = 0xA5;
    r.rasterize('\u{754C}', 0, 2, &mut out[..len]);
    assert_eq!(out[len], 0xA5);
    // A non-BMP character goes out as a surrogate pair without trouble.
    let _ = raster(&mut r, '\u{1F600}', 0, 2);
}

#[test]
fn gdi_rasterizer_rejects_bad_input() {
    assert!(GdiRasterizer::new("a face name far longer than LF_FACESIZE", 16).is_err());
    assert!(GdiRasterizer::new("Consolas", 0).is_err());
}

#[test]
fn window_events_waker_and_present() {
    let mut w = Window::new("fwrender test", 320, 200).expect("window");
    assert!(!w.hwnd().is_null() && !w.hinstance().is_null());
    // Per-monitor DPI awareness makes the client size exact physical pixels.
    assert_eq!(w.client_size(), (320, 200));

    let mut events = Vec::new();
    for _ in 0..5 {
        w.poll(&mut events);
    }
    assert!(!events.contains(&Event::Woken));

    // A GDI-rasterized frame, drawn by the CPU renderer and presented.
    let mut gdi = GdiRasterizer::new("Consolas", 16).unwrap();
    let m = gdi.metrics();
    let mut atlas = Atlas::new(m, 512, 512);
    let mut screen = Screen::new(4, 20);
    screen.write_str(0, 0, "foldwave", Style::default());
    let mut frame = Frame::new();
    frame.update(&screen, false, &Palette::default(), &mut atlas, &mut gdi);
    let mut cpu = CpuRenderer::new(320, 200);
    cpu.draw(&frame, &atlas);
    let mut presenter = CpuPresenter::new();
    presenter.present(&w, &cpu, 0..200).expect("present all");
    presenter.present(&w, &cpu, 10..30).expect("present a band");
    presenter.present(&w, &cpu, 150..999).expect("clamped");
    presenter.present(&w, &cpu, 5..5).expect("empty");

    // Read back: the window shows the framebuffer, and a band lands on its
    // own rows. A second framebuffer with another background is presented
    // for rows 50..60 only.
    presenter.present(&w, &cpu, 0..200).unwrap();
    let pal2 = Palette {
        bg: 0xFF12_3456,
        ..Palette::default()
    };
    let mut frame2 = Frame::new();
    frame2.update(&screen, false, &pal2, &mut atlas, &mut gdi);
    let mut cpu2 = CpuRenderer::new(320, 200);
    cpu2.draw(&frame2, &atlas);
    presenter.present(&w, &cpu2, 50..60).unwrap();
    let colorref = |argb: u32| (argb & 0xFF) << 16 | (argb & 0xFF00) | (argb >> 16) & 0xFF;
    // SAFETY (test): the window is live on this thread; the DC is released.
    let read = |x: i32, y: i32| unsafe {
        let dc = sys::GetDC(w.hwnd());
        let c = sys::GetPixel(dc, x, y);
        sys::ReleaseDC(w.hwnd(), dc);
        c
    };
    let at = |c: &CpuRenderer, x: u32, y: u32| colorref(c.pixels()[(y * 320 + x) as usize]);
    // The first glyph pixel with ink, to check glyphs arrive too.
    let inked = (0..m.cell_h)
        .flat_map(|y| (0..m.cell_w).map(move |x| (x, y)))
        .find(|&(x, y)| at(&cpu, x, y) != colorref(Palette::default().bg))
        .expect("'f' has ink");
    for (x, y, want) in [
        (inked.0, inked.1, at(&cpu, inked.0, inked.1)),
        (300, 49, at(&cpu, 300, 49)),
        (300, 50, colorref(pal2.bg)),
        (300, 59, colorref(pal2.bg)),
        (300, 60, at(&cpu, 300, 60)),
        (319, 199, at(&cpu, 319, 199)),
    ] {
        assert_eq!(read(x as i32, y as i32), want, "pixel ({x}, {y})");
    }

    // A waker on another thread ends a blocked wait promptly.
    events.clear();
    w.poll(&mut events);
    let waker = w.waker();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        waker.wake();
    });
    let start = Instant::now();
    events.clear();
    while !events.contains(&Event::Woken) && start.elapsed() < Duration::from_secs(5) {
        w.wait(Some(Duration::from_secs(5)), &mut events);
    }
    let took = start.elapsed();
    t.join().unwrap();
    assert!(events.contains(&Event::Woken), "{events:?}");
    assert!(took < Duration::from_secs(1), "woke after {took:?}");

    // A wait with nothing to do times out instead of returning early or
    // hanging (other messages may still produce events; drain those first).
    for _ in 0..3 {
        events.clear();
        w.poll(&mut events);
    }
    let start = Instant::now();
    events.clear();
    w.wait(Some(Duration::from_millis(30)), &mut events);
    if events.is_empty() {
        assert!(start.elapsed() >= Duration::from_millis(29));
    }

    let late = w.waker();
    drop(w);
    late.wake(); // Waking a dropped window is a no-op.
}

#[test]
fn the_display_reports_a_plausible_refresh_rate() {
    let hz = super::display_refresh_hz();
    println!("display refresh: {hz:?}");
    if let Some(hz) = hz {
        assert!((24..=1000).contains(&hz), "{hz} Hz");
    }
}

#[test]
fn alt_alone_does_not_open_the_menu_but_alt_space_does() {
    use super::window::alt_alone_menu;
    // SC_KEYMENU from Alt alone: no character.
    assert!(alt_alone_menu(0xF100, 0));
    // The low four bits of wparam are the system's; they do not change it.
    assert!(alt_alone_menu(0xF100 | 0x3, 0));
    // Alt+Space: the window menu, as always.
    assert!(!alt_alone_menu(0xF100, b' ' as isize));
    // Other system commands pass through.
    assert!(!alt_alone_menu(0xF060, 0)); // SC_CLOSE
}

/// How long a typed character takes from the message queue to `wait`
/// returning it: posted from another thread, 2,000 times. A measurement,
/// not a check; run it by hand (release):
/// `cargo test -p nativelite-fwrender --release --lib -- --ignored --nocapture input_to_wait`
#[test]
#[ignore = "timing; run by hand"]
fn input_to_wait_latency() {
    use std::sync::mpsc;
    let mut w = Window::new("fwrender latency", 200, 100).expect("window");
    let hwnd = w.hwnd() as usize;
    let mut events = Vec::new();
    for _ in 0..5 {
        w.poll(&mut events);
    }
    let (go, went) = mpsc::channel::<()>();
    let (sent_tx, sent_rx) = mpsc::channel::<Instant>();
    let sender = std::thread::spawn(move || {
        while went.recv().is_ok() {
            std::thread::sleep(Duration::from_millis(1));
            let at = Instant::now();
            // SAFETY (test): the window is live until the sender is joined.
            unsafe { sys::PostMessageW(hwnd as sys::Hwnd, sys::WM_CHAR, 'x' as usize, 0) };
            sent_tx.send(at).unwrap();
        }
    });
    let mut samples = Vec::new();
    for _ in 0..2000 {
        events.clear();
        go.send(()).unwrap();
        while !events.contains(&Event::Char('x')) {
            w.wait(Some(Duration::from_secs(1)), &mut events);
        }
        let got = Instant::now();
        samples.push(got - sent_rx.recv().unwrap());
    }
    drop(go);
    sender.join().unwrap();
    samples.sort();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let n = samples.len();
    println!(
        "input to wait: median {:.1} us, p90 {:.1} us, p99 {:.1} us, max {:.1} us (n = {n})",
        us(samples[n / 2]),
        us(samples[n * 90 / 100]),
        us(samples[n * 99 / 100]),
        us(samples[n - 1])
    );
}

/// The caller's side never waits on the window's thread: while that thread
/// is held busy (as Windows' modal loop holds it while an edge is dragged),
/// a waker still ends `wait` at once, and the events queued meanwhile (here
/// a resize) arrive when the thread is free again.
#[test]
fn waiting_goes_on_while_the_window_thread_is_busy() {
    use super::window::WM_TEST_STALL;
    let mut w = Window::new("fwrender busy", 240, 120).expect("window");
    let hwnd = w.hwnd();
    let mut events = Vec::new();
    for _ in 0..5 {
        w.poll(&mut events);
    }
    // SAFETY (test): the window is live; the message only sleeps.
    unsafe { sys::PostMessageW(hwnd, WM_TEST_STALL, 600, 0) };
    std::thread::sleep(Duration::from_millis(50));
    let waker = w.waker();
    let start = Instant::now();
    std::thread::spawn(move || waker.wake());
    events.clear();
    while !events.contains(&Event::Woken) && start.elapsed() < Duration::from_secs(2) {
        w.wait(Some(Duration::from_secs(2)), &mut events);
    }
    let took = start.elapsed();
    assert!(events.contains(&Event::Woken), "{events:?}");
    assert!(
        took < Duration::from_millis(200),
        "woke after {took:?} while the window thread slept 600 ms"
    );
    // A resize asked for now is handled once the thread is free.
    w.set_client_size(300, 150).unwrap();
    let start = Instant::now();
    events.clear();
    while !events.contains(&Event::Resized(300, 150)) && start.elapsed() < Duration::from_secs(2) {
        w.wait(Some(Duration::from_secs(2)), &mut events);
    }
    assert!(events.contains(&Event::Resized(300, 150)), "{events:?}");
    assert_eq!(w.client_size(), (300, 150));
}

/// The clipboard works from the caller's thread although the window lives on
/// its own. It replaces the clipboard's contents, so it runs only by hand,
/// with the clipboard saved and restored around it:
/// `cargo test -p nativelite-fwrender --lib -- --ignored clipboard_from`
#[test]
#[ignore = "replaces the clipboard; run by hand"]
fn clipboard_from_the_callers_thread() {
    let w = Window::new("fwrender clipboard", 200, 100).expect("window");
    let text = "foldwave clipboard check: Zoë ✓";
    w.set_clipboard(text).expect("set");
    assert_eq!(w.clipboard().expect("get").as_deref(), Some(text));
}
