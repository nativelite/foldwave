//! The window against a real compositor, where there is one.
//!
//! A machine with no compositor (or no `libwayland-client`) prints why and
//! passes: these tests prove the window where it can exist, and must not
//! fail the suite where it cannot.

use super::*;
use crate::{Atlas, CpuRenderer, Frame, Metrics, Palette, TestRasterizer};
use ansi::{Screen, Style};
use std::time::{Duration, Instant};

const M: Metrics = Metrics {
    cell_w: 9,
    cell_h: 17,
    baseline: 13,
};

fn window(title: &str, w: u32, h: u32) -> Option<Window> {
    match Window::new(title, w, h) {
        Ok(window) => Some(window),
        Err(e) => {
            println!("skipping: no Wayland window: {e}");
            None
        }
    }
}

#[test]
fn a_window_opens_at_its_size_and_asks_for_a_frame() {
    let Some(mut window) = window("fwrender wl test", 320, 200) else {
        return;
    };
    assert_eq!(window.client_size(), (320, 200));
    let mut events = Vec::new();
    window.poll(&mut events);
    assert!(events.contains(&Event::Paint), "{events:?}");
    assert!(!window.display().is_null() && !window.surface().is_null());
    assert_eq!(window.dpi(), 96);
}

#[test]
fn a_waker_ends_a_wait_from_another_thread() {
    let Some(mut window) = window("fwrender wl wake", 64, 64) else {
        return;
    };
    let mut events = Vec::new();
    window.poll(&mut events);
    events.clear();

    let started = Instant::now();
    window.wait(Some(Duration::from_millis(30)), &mut events);
    assert!(events.is_empty(), "nothing happened: {events:?}");
    assert!(started.elapsed() >= Duration::from_millis(25), "it slept");

    let waker = window.waker();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        waker.wake();
    });
    let started = Instant::now();
    window.wait(Some(Duration::from_secs(5)), &mut events);
    thread.join().unwrap();
    assert_eq!(events, [Event::Woken]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "woken, not timed out"
    );

    // A waker for a window that is gone does nothing.
    let waker = window.waker();
    drop(window);
    waker.wake();
}

#[test]
fn cpu_frames_reach_the_compositor_and_follow_a_resize() {
    let Some(mut window) = window("fwrender wl cpu", 9 * 20, 17 * 4) else {
        return;
    };
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut screen = Screen::new(4, 20);
    let mut frame = Frame::new();
    let mut presenter = CpuPresenter::new();
    let mut events = Vec::new();

    let (w, h) = window.client_size();
    let mut cpu = CpuRenderer::new(w, h);
    for i in 0..6 {
        screen.write_str(i % 4, 0, &format!("frame {i}"), Style::default());
        frame.update(&screen, i == 0, &palette, &mut atlas, &mut raster);
        cpu.draw(&frame, &atlas);
        presenter.present(&window, &cpu, 0..h).expect("present");
        frame.clear_damage();
        // Let the compositor take the frame and hand a buffer back.
        window.wait(Some(Duration::from_millis(20)), &mut events);
    }
    assert!(
        !events.contains(&Event::CloseRequested),
        "the compositor kept the window: {events:?}"
    );
    // Buffers are reused once released: six frames need only a few.
    assert!(presenter.buffers_in_use() <= 3, "{presenter:?}");

    window.set_client_size(9 * 30, 17 * 6).expect("resize");
    events.clear();
    window.poll(&mut events);
    assert!(events.contains(&Event::Resized(270, 102)), "{events:?}");
    let (w, h) = window.client_size();
    let mut cpu = CpuRenderer::new(w, h);
    frame.update(&screen, true, &palette, &mut atlas, &mut raster);
    cpu.draw(&frame, &atlas);
    presenter.present(&window, &cpu, 0..h).expect("present");
    window.wait(Some(Duration::from_millis(20)), &mut events);
    // A presenter may outlive its window, in either order of dropping.
    drop(window);
    drop(presenter);
}
