//! M3: our TrueType rasterizer (`glyphs`, through `fwrender::FontRasterizer`)
//! against Windows GDI, the same font and size.
//!
//! Times one glyph rasterization into a cell, for every printable ASCII
//! character in regular and bold, `reps` times over (each sample is one
//! glyph), and measures how far the two coverages differ: the mean absolute
//! difference per pixel, 0–255, over every cell.

use crate::stats::{ms, Summary};
use fwrender::Rasterizer;
use std::time::{Duration, Instant};

/// One rasterizer's timings.
#[derive(Debug, Clone)]
pub struct Timed {
    /// Name for the report.
    pub name: String,
    /// Cell size it reports.
    pub cell: (u32, u32),
    /// One sample per glyph rasterized.
    pub samples: Vec<Duration>,
}

/// The characters measured: printable ASCII.
fn chars() -> impl Iterator<Item = char> {
    (0x21u8..0x7F).map(char::from)
}

/// Time `r` over printable ASCII, regular and bold, `reps` times.
pub fn time(name: &str, r: &mut dyn Rasterizer, reps: usize) -> Timed {
    let m = r.metrics();
    let mut out = vec![0u8; (m.cell_w * m.cell_h) as usize];
    let mut samples = Vec::with_capacity(reps * 2 * 94);
    for _ in 0..reps {
        for style in [0u8, fwrender::BOLD] {
            for c in chars() {
                out.iter_mut().for_each(|v| *v = 0);
                let t = Instant::now();
                r.rasterize(c, style, 1, &mut out);
                samples.push(t.elapsed());
            }
        }
    }
    Timed {
        name: name.into(),
        cell: (m.cell_w, m.cell_h),
        samples,
    }
}

/// Mean absolute coverage difference per pixel (0–255) between two
/// rasterizers with the same cell size, over printable ASCII (regular).
/// `None` when their cells differ.
pub fn difference(a: &mut dyn Rasterizer, b: &mut dyn Rasterizer) -> Option<f64> {
    let (ma, mb) = (a.metrics(), b.metrics());
    if (ma.cell_w, ma.cell_h) != (mb.cell_w, mb.cell_h) {
        return None;
    }
    let n = (ma.cell_w * ma.cell_h) as usize;
    let (mut x, mut y) = (vec![0u8; n], vec![0u8; n]);
    let (mut sum, mut count) = (0u64, 0u64);
    for c in chars() {
        x.iter_mut().for_each(|v| *v = 0);
        y.iter_mut().for_each(|v| *v = 0);
        a.rasterize(c, 0, 1, &mut x);
        b.rasterize(c, 0, 1, &mut y);
        sum += x
            .iter()
            .zip(&y)
            .map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64)
            .sum::<u64>();
        count += n as u64;
    }
    Some(sum as f64 / count as f64)
}

/// The report.
pub fn render(font: &str, px: f32, runs: &[Timed], diff: Option<f64>) -> String {
    let mut o = format!(
        "glyphs: {font} at {px} px, printable ASCII regular + bold, one sample per glyph\n\n\
         | rasterizer | cell | per glyph p50 / p90 / p99 | max | n |\n| --- | --- | --- | --- | --- |\n"
    );
    for r in runs {
        let s = Summary::of(&r.samples);
        let us = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1e6);
        o += &format!(
            "| {} | {}x{} | {} / {} / {} µs | {} | {} |\n",
            r.name,
            r.cell.0,
            r.cell.1,
            us(s.p50),
            us(s.p90),
            us(s.p99),
            ms(s.max),
            s.count
        );
    }
    o += &match diff {
        Some(d) => format!("\nmean |coverage difference| per pixel, ours vs GDI: {d:.1} of 255\n"),
        None => "\ncells differ in size; coverage not compared\n".into(),
    };
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use fwrender::{Metrics, TestRasterizer};

    #[test]
    fn timing_and_difference_run_on_any_rasterizer() {
        let m = Metrics {
            cell_w: 8,
            cell_h: 16,
            baseline: 12,
        };
        let (mut a, mut b) = (TestRasterizer { metrics: m }, TestRasterizer { metrics: m });
        let t = time("test", &mut a, 2);
        assert_eq!(t.samples.len(), 2 * 2 * 94);
        assert_eq!(
            difference(&mut a, &mut b),
            Some(0.0),
            "identical rasterizers"
        );
        assert!(render("x", 16.0, &[t], Some(0.0)).contains("| test | 8x16 |"));
    }
}

/// Share of inked pixels (coverage >= 32) that are grey (<= 223) rather than
/// solid, over printable ASCII (regular): lower is crisper.
pub fn softness(r: &mut dyn Rasterizer) -> f64 {
    let m = r.metrics();
    let mut out = vec![0u8; (m.cell_w * m.cell_h) as usize];
    let (mut grey, mut inked) = (0u64, 0u64);
    for c in chars() {
        out.iter_mut().for_each(|v| *v = 0);
        r.rasterize(c, 0, 1, &mut out);
        for &v in &out {
            if v >= 32 {
                inked += 1;
                if v <= 223 {
                    grey += 1;
                }
            }
        }
    }
    grey as f64 / inked.max(1) as f64
}

/// Draw `lines` of text with each rasterizer, one block per rasterizer,
/// light grey on the terminal's dark background, as `0xAARRGGBB` pixels.
/// Returns `(width, height, pixels)`.
pub fn sheet(rasters: &mut [&mut dyn Rasterizer], lines: &[&str]) -> (usize, usize, Vec<u32>) {
    let (fg, bg) = (0xFFCC_CCCCu32, 0xFF0C_0C0Cu32);
    let gap = 6;
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let cw = rasters
        .iter()
        .map(|r| r.metrics().cell_w as usize)
        .max()
        .unwrap_or(1);
    let width = cols * cw + 2 * gap;
    let height: usize = rasters
        .iter()
        .map(|r| r.metrics().cell_h as usize * lines.len() + gap)
        .sum::<usize>()
        + gap;
    let mut px = vec![bg; width * height];
    let mut y0 = gap;
    for r in rasters.iter_mut() {
        let m = r.metrics();
        let (w, h) = (m.cell_w as usize, m.cell_h as usize);
        let mut cell = vec![0u8; w * h];
        for (li, line) in lines.iter().enumerate() {
            for (ci, ch) in line.chars().enumerate() {
                cell.iter_mut().for_each(|v| *v = 0);
                r.rasterize(ch, 0, 1, &mut cell);
                for y in 0..h {
                    for x in 0..w {
                        let at = (y0 + li * h + y) * width + gap + ci * w + x;
                        px[at] = fwrender::blend(fg, bg, cell[y * w + x]);
                    }
                }
            }
        }
        y0 += h * lines.len() + gap;
    }
    (width, height, px)
}

/// `pixels` scaled up `k` times (nearest neighbour), for looking closely.
pub fn zoom(width: usize, height: usize, pixels: &[u32], k: usize) -> (usize, usize, Vec<u32>) {
    let (w, h) = (width * k, height * k);
    let mut out = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = pixels[(y / k) * width + x / k];
        }
    }
    (w, h, out)
}

/// A PNG of `0xAARRGGBB` pixels (RGB, 8-bit), stored uncompressed: std has no
/// deflate, and stored blocks are valid deflate.
pub fn png(width: usize, height: usize, pixels: &[u32]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity(height * (1 + 3 * width));
    for row in pixels.chunks(width) {
        raw.push(0); // filter: none
        for &p in row {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    // zlib: header, stored blocks of at most 65535 bytes, Adler-32.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn png_has_a_valid_signature_and_known_crc() {
        let p = png(2, 1, &[0xFFFF_0000, 0xFF00_FF00]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        // IEND's CRC is fixed: AE 42 60 82.
        assert_eq!(&p[p.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
    }
}
