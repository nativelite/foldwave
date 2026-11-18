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

/// A frame as an exporting renderer hands it over, but backed by an
/// ordinary file the CPU renderer's pixels are written to. A compositor
/// drawing in software maps a dmabuf like any file, so this stands in for
/// the GPU's on a machine with no graphics device.
fn file_frame(cpu: &CpuRenderer, n: usize) -> crate::vk::Dmabuf {
    use std::os::unix::fs::FileExt;
    let path = std::env::temp_dir().join(format!("fwrender-frame-{}-{n}", std::process::id()));
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let bytes: Vec<u8> = cpu.pixels().iter().flat_map(|p| p.to_le_bytes()).collect();
    file.write_all_at(&bytes, 0).unwrap();
    crate::vk::Dmabuf {
        fd: file.into(),
        width: cpu.width(),
        height: cpu.height(),
        offset: 0,
        stride: cpu.width() * 4,
    }
}

/// The Wayland half of GPU frames, against a real compositor: it is asked
/// what it takes, given each frame once, and shown them in turn; a frame it
/// has finished with comes free again; a new size brings new frames. On a
/// compositor without dmabufs the request is an error and the window goes
/// on working, which is what lets a caller fall back.
#[test]
fn frames_go_to_the_compositor_as_dmabufs_or_the_window_is_left_alone() {
    use crate::vk::FrameSink;
    let Some(mut window) = window("fwrender wl gpu", 9 * 20, 17 * 4) else {
        return;
    };
    let palette = Palette::default();
    let mut atlas = Atlas::new(M, 256, 256);
    let mut raster = TestRasterizer { metrics: M };
    let mut screen = Screen::new(4, 20);
    screen.write_str(0, 0, "gpu frames", Style::default());
    let mut frame = Frame::new();
    frame.update(&screen, true, &palette, &mut atlas, &mut raster);
    let (w, h) = window.client_size();
    let mut cpu = CpuRenderer::new(w, h);
    cpu.draw(&frame, &atlas);
    let mut events = Vec::new();

    let frames = (0..3).map(|n| file_frame(&cpu, n)).collect();
    let mut sink = match window.gpu_frames(frames) {
        Ok(sink) => sink,
        Err(e) => {
            println!("no dmabufs on this compositor ({e}); the window is left alone");
            let mut presenter = CpuPresenter::new();
            presenter.present(&window, &cpu, 0..h).expect("present");
            window.wait(Some(Duration::from_millis(30)), &mut events);
            assert!(!events.contains(&Event::CloseRequested), "{events:?}");
            return;
        }
    };

    // Shown in turn; with all three handed over, one comes back as the
    // compositor moves on, so there is always one to draw into again.
    let mut shown = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while shown.len() < 8 {
        match sink.acquire() {
            Some(index) => {
                sink.present(index).expect("present");
                shown.push(index);
            }
            None => assert!(Instant::now() < deadline, "no frame came free: {shown:?}"),
        }
        window.wait(Some(Duration::from_millis(20)), &mut events);
    }
    println!("frames shown: {shown:?}");
    assert!(shown[..3].iter().all(|i| *i < 3));
    assert!(
        !events.contains(&Event::CloseRequested),
        "the compositor kept the window: {events:?}"
    );

    // A new size: new frames replace the old, and are shown.
    let mut cpu = CpuRenderer::new(9 * 30, 17 * 6);
    frame.update(&screen, true, &palette, &mut atlas, &mut raster);
    cpu.draw(&frame, &atlas);
    window.set_client_size(9 * 30, 17 * 6).expect("resize");
    sink.rebuild((0..3).map(|n| file_frame(&cpu, n + 3)).collect())
        .expect("rebuild");
    let index = sink.acquire().expect("a new frame is free");
    sink.present(index).expect("present");
    events.clear();
    window.wait(Some(Duration::from_millis(40)), &mut events);
    assert!(!events.contains(&Event::CloseRequested), "{events:?}");

    // A sink that outlives its window does nothing more.
    drop(window);
    assert_eq!(sink.acquire(), None);
    sink.present(0).expect("a no-op");
}

/// A probe, run by hand against a compositor that can inject input
/// (`cargo test -p nativelite-fwrender probe_events -- --ignored --nocapture`
/// with `wtype` typing meanwhile): prints every event for a few seconds.
#[test]
#[ignore]
fn probe_events() {
    let Some(mut window) = window("fwrender probe", 400, 200) else {
        return;
    };
    let (w, h) = window.client_size();
    let cpu = CpuRenderer::new(w, h);
    let mut presenter = CpuPresenter::new();
    presenter.present(&window, &cpu, 0..h).expect("present");
    let mut events = Vec::new();
    let end = Instant::now() + Duration::from_secs(6);
    while Instant::now() < end {
        window.wait(Some(Duration::from_millis(100)), &mut events);
        for e in events.drain(..) {
            println!("event: {e:?}");
            if let Event::Resized(w, h) = e {
                let cpu = CpuRenderer::new(w, h);
                presenter.present(&window, &cpu, 0..h).expect("present");
            }
        }
    }
}
