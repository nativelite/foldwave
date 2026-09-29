//! foldwave's icon, 01 Reticle (design §11 row 17), drawn in code at any
//! size, so there is no image file to ship: a hexagonal frame with sight
//! ticks, and a lilac wave locking onto a cyan dot. Small sizes drop the
//! ticks and thicken the lines, as small icons need.

/// The icon's colours: Mirai's, whatever the terminal's theme.
const SURFACE: [u8; 3] = [0x0d, 0x16, 0x22];
const ACCENT: [u8; 3] = [0x5e, 0xf2, 0xff];
const HOLO: [u8; 3] = [0xb9, 0xa6, 0xff];

/// The hexagon, in the 64-unit square the icon is drawn in (y down).
const HEX: [(f32, f32); 6] = [
    (32.0, 3.0),
    (57.0, 17.5),
    (57.0, 46.5),
    (32.0, 61.0),
    (7.0, 46.5),
    (7.0, 17.5),
];

/// A shape and the colour it paints.
enum Shape {
    Polygon(Vec<(f32, f32)>),
    /// Line segments `half` units either side (round ends), and the box
    /// they fill, so most samples skip them.
    Stroke(Vec<((f32, f32), (f32, f32))>, f32, [f32; 4]),
    Disc((f32, f32), f32),
}

impl Shape {
    fn covers(&self, x: f32, y: f32) -> bool {
        match self {
            Shape::Polygon(p) => inside(p, x, y),
            Shape::Stroke(segs, half, [x0, y0, x1, y1]) => {
                (*x0..=*x1).contains(&x)
                    && (*y0..=*y1).contains(&y)
                    && segs.iter().any(|&(a, b)| dist(a, b, x, y) <= *half)
            }
            Shape::Disc((cx, cy), r) => (x - cx).powi(2) + (y - cy).powi(2) <= r * r,
        }
    }
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut odd = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let ((xi, yi), (xj, yj)) = (poly[i], poly[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// Distance from `(x, y)` to the segment `a`-`b`.
fn dist(a: (f32, f32), b: (f32, f32), x: f32, y: f32) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len == 0.0 {
        0.0
    } else {
        (((x - a.0) * dx + (y - a.1) * dy) / len).clamp(0.0, 1.0)
    };
    let (px, py) = (a.0 + t * dx, a.1 + t * dy);
    ((x - px).powi(2) + (y - py).powi(2)).sqrt()
}

/// A stroke of `segs`, `half` units either side.
fn stroke(segs: Vec<((f32, f32), (f32, f32))>, half: f32) -> Shape {
    let xs = segs.iter().flat_map(|(a, b)| [a.0, b.0]);
    let ys = segs.iter().flat_map(|(a, b)| [a.1, b.1]);
    let (x0, x1) = xs.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let (y0, y1) = ys.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    Shape::Stroke(segs, half, [x0 - half, y0 - half, x1 + half, y1 + half])
}

/// A cubic Bézier as `n` segments.
fn cubic(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    n: usize,
) -> Vec<((f32, f32), (f32, f32))> {
    let at = |t: f32| {
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        (
            a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
            a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
        )
    };
    (0..n)
        .map(|i| (at(i as f32 / n as f32), at((i + 1) as f32 / n as f32)))
        .collect()
}

/// The layers, back to front, for an icon `size` pixels square.
fn layers(size: u32) -> Vec<(Shape, [u8; 3])> {
    let small = size <= 24;
    let edges: Vec<_> = (0..6).map(|i| (HEX[i], HEX[(i + 1) % 6])).collect();
    let mut wave = cubic((16.0, 34.0), (21.0, 24.0), (26.0, 24.0), (31.0, 34.0), 24);
    wave.extend(cubic(
        (31.0, 34.0),
        (36.0, 44.0),
        (41.0, 44.0),
        (47.0, 32.0),
        24,
    ));
    let mut out = vec![
        (Shape::Polygon(HEX.to_vec()), SURFACE),
        (stroke(edges, if small { 2.2 } else { 0.9 }), ACCENT),
    ];
    if !small {
        let ticks = vec![
            ((32.0, 9.0), (32.0, 13.0)),
            ((32.0, 51.0), (32.0, 55.0)),
            ((11.0, 32.0), (15.0, 32.0)),
            ((49.0, 32.0), (53.0, 32.0)),
        ];
        for t in ticks {
            out.push((stroke(vec![t], 0.9), ACCENT));
        }
    }
    out.push((stroke(wave, if small { 3.4 } else { 1.6 }), HOLO));
    out.push((
        Shape::Disc((47.0, 32.0), if small { 4.2 } else { 2.6 }),
        ACCENT,
    ));
    out
}

/// The icon as straight-alpha RGBA rows, top first, `size` pixels square;
/// each pixel is 16 samples of the layers.
pub fn reticle(size: u32) -> Vec<u8> {
    const GRID: u32 = 4;
    let layers = layers(size);
    let scale = 64.0 / size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut hits) = ([0u32; 3], 0u32);
            for sy in 0..GRID {
                for sx in 0..GRID {
                    let x = (px as f32 + (sx as f32 + 0.5) / GRID as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / GRID as f32) * scale;
                    if let Some((_, c)) = layers.iter().rev().find(|(s, _)| s.covers(x, y)) {
                        for k in 0..3 {
                            sum[k] += c[k] as u32;
                        }
                        hits += 1;
                    }
                }
            }
            let at = ((py * size + px) * 4) as usize;
            if hits > 0 {
                for k in 0..3 {
                    out[at + k] = (sum[k] / hits) as u8;
                }
                out[at + 3] = (hits * 255 / (GRID * GRID)) as u8;
            }
        }
    }
    out
}

/// Every size Windows asks a window icon for, from 16 px at 100 % to
/// 32 px at 300 %.
pub const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 96];

/// The two a window needs at `dpi`: the title bar's (16 px at 100 %) and
/// the taskbar's (32 px), each the nearest size at or above.
pub fn sizes_for(dpi: u32) -> [u32; 2] {
    let at = |base: u32| {
        let want = base * dpi.max(96) / 96;
        SIZES
            .iter()
            .copied()
            .find(|&s| s >= want)
            .unwrap_or(SIZES[SIZES.len() - 1])
    };
    [at(16), at(32)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn the_reticle_has_its_frame_wave_and_dot_where_they_belong() {
        let s = 64;
        let img = reticle(s);
        assert_eq!(img.len(), 64 * 64 * 4);
        assert_eq!(px(&img, s, 0, 0)[3], 0, "outside the hexagon: clear");
        assert_eq!(
            px(&img, s, 32, 20),
            [SURFACE[0], SURFACE[1], SURFACE[2], 255],
            "inside: surface"
        );
        let dot = px(&img, s, 47, 32);
        assert_eq!(&dot[..3], &ACCENT, "the cyan dot at the wave's end");
        let wave = px(&img, s, 21, 26);
        assert_eq!(&wave[..3], &HOLO, "the wave's crest is lilac");
        let edge = px(&img, s, 7, 32);
        assert!(
            edge[1] > 150 && edge[2] > 150,
            "the frame's left edge is cyan: {edge:?}"
        );
    }

    #[test]
    fn a_window_draws_just_the_two_sizes_its_screen_needs() {
        assert_eq!(sizes_for(96), [16, 32]);
        assert_eq!(sizes_for(144), [24, 48]);
        assert_eq!(sizes_for(288), [48, 96]);
        assert_eq!(sizes_for(0), [16, 32], "unknown: 100 %");
    }

    #[test]
    fn small_icons_keep_the_shape_readable() {
        for s in SIZES {
            let img = reticle(s);
            assert_eq!(img.len(), (s * s * 4) as usize);
            let lilac = img
                .chunks_exact(4)
                .filter(|p| p[3] > 128 && p[0] > 150 && p[2] > 200 && p[1] < 190)
                .count();
            assert!(
                lilac * 60 > (s * s) as usize,
                "{s} px: the wave shows ({lilac} lilac pixels)"
            );
        }
    }
}

#[cfg(test)]
mod timing {
    #[test]
    #[ignore]
    fn how_long_every_size_takes() {
        let t = std::time::Instant::now();
        for s in super::SIZES {
            std::hint::black_box(super::reticle(s));
        }
        eprintln!("all sizes: {:?}", t.elapsed());
        for dpi in [96, 144, 288] {
            let t = std::time::Instant::now();
            for s in super::sizes_for(dpi) {
                std::hint::black_box(super::reticle(s));
            }
            eprintln!("at {dpi} dpi: {:?}", t.elapsed());
        }
    }
}
