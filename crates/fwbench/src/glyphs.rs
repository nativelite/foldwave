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
