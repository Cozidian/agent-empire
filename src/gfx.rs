//! Pixel framebuffer, math, post-processing and the half-block terminal encoder.

use rayon::prelude::*;
use std::io::{self, Write};
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

// ───────────────────────────── vector math ─────────────────────────────

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[inline]
pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}
#[inline]
pub const fn s3(s: f32) -> V3 {
    V3 { x: s, y: s, z: s }
}

impl Add for V3 {
    type Output = V3;
    #[inline]
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    #[inline]
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for V3 {
    type Output = V3;
    #[inline]
    fn mul(self, s: f32) -> V3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}
impl Mul<V3> for V3 {
    type Output = V3;
    #[inline]
    fn mul(self, o: V3) -> V3 {
        v3(self.x * o.x, self.y * o.y, self.z * o.z)
    }
}
impl Div<f32> for V3 {
    type Output = V3;
    #[inline]
    fn div(self, s: f32) -> V3 {
        self * (1.0 / s)
    }
}
impl Neg for V3 {
    type Output = V3;
    #[inline]
    fn neg(self) -> V3 {
        v3(-self.x, -self.y, -self.z)
    }
}
impl AddAssign for V3 {
    #[inline]
    fn add_assign(&mut self, o: V3) {
        *self = *self + o;
    }
}

impl V3 {
    #[inline]
    pub fn max_elem(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
}

#[inline]
pub fn mix(a: V3, b: V3, t: f32) -> V3 {
    a + (b - a) * t
}
#[inline]
pub fn mixf(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
#[inline]
pub fn fract(x: f32) -> f32 {
    x - x.floor()
}
#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn hsv(h: f32, s: f32, v: f32) -> V3 {
    let f = |n: f32| {
        let k = (n + h * 6.0).rem_euclid(6.0);
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    v3(f(5.0), f(3.0), f(1.0))
}


#[inline]
pub fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}
#[inline]
pub fn rnd(n: u32) -> f32 {
    (hash(n) >> 8) as f32 / (1u32 << 24) as f32
}
#[inline]
pub fn rnd2(x: i32, y: i32, s: u32) -> f32 {
    rnd((x as u32).wrapping_mul(73_856_093)
        ^ (y as u32).wrapping_mul(19_349_663)
        ^ s.wrapping_mul(83_492_791))
}

// ───────────────────────────── framebuffer ─────────────────────────────

/// A crisp glyph drawn over the pixels. Without `bg`, the cell's background is
/// the darkened pixels beneath it. `'\0'` marks the right half of a wide glyph.
#[derive(Clone, Copy, PartialEq)]
pub struct Tc {
    pub ch: char,
    pub fg: V3,
    pub bg: Option<V3>,
}

/// `w × h` square pixels, where every terminal cell holds a 2×4 block (an octant glyph).
/// `txt` is a crisp glyph overlay, one slot per terminal cell.
pub struct Fb {
    pub w: usize,
    pub h: usize,
    pub cols: usize,
    pub rows: usize,
    pub px: Vec<V3>,
    pub txt: Vec<Option<Tc>>,
}

impl Fb {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(20), rows.max(8));
        let (w, h) = (cols * 2, rows * 4);
        Self { w, h, cols, rows, px: vec![V3::default(); w * h], txt: vec![None; cols * rows] }
    }


    #[inline]
    pub fn idx(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h)
            .then(|| y as usize * self.w + x as usize)
    }
    #[inline]
    pub fn add(&mut self, x: i32, y: i32, c: V3) {
        if let Some(i) = self.idx(x, y) {
            self.px[i] += c;
        }
    }

    /// Additive bilinear point — sub-pixel smooth particles.
    pub fn splat(&mut self, x: f32, y: f32, c: V3) {
        splat(&mut self.px, self.w, self.h, x, y, c);
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c0: V3, c1: V3) {
        let n = (x1 - x0).abs().max((y1 - y0).abs()).ceil().clamp(1.0, 400.0) as i32;
        for i in 0..=n {
            let k = i as f32 / n as f32;
            self.splat(mixf(x0, x1, k), mixf(y0, y1, k), mix(c0, c1, k));
        }
    }

    pub fn put(&mut self, col: i32, row: i32, ch: char, c: V3) {
        self.put_bg(col, row, ch, c, None);
    }
    pub fn put_bg(&mut self, col: i32, row: i32, ch: char, fg: V3, bg: Option<V3>) {
        if col >= 0 && row >= 0 && (col as usize) < self.cols && (row as usize) < self.rows {
            self.txt[row as usize * self.cols + col as usize] = Some(Tc { ch, fg, bg });
        }
    }
    /// Fill a rectangle of cells with a solid background.
    pub fn panel(&mut self, col: i32, row: i32, w: i32, h: i32, bg: V3) {
        for r in row..row + h {
            for c in col..col + w {
                self.put_bg(c, r, ' ', bg, Some(bg));
            }
        }
    }
    pub fn text(&mut self, col: i32, row: i32, s: &str, c: V3) {
        self.text_with(col, row, s, |_| c);
    }
    pub fn text_with<F: Fn(usize) -> V3>(&mut self, col: i32, row: i32, s: &str, f: F) {
        for (i, ch) in s.chars().enumerate() {
            self.put(col + i as i32, row, ch, f(i));
        }
    }
    pub fn clear_text(&mut self) {
        self.txt.fill(None);
    }
}

pub fn splat(buf: &mut [V3], w: usize, h: usize, x: f32, y: f32, c: V3) {
    let (fx, fy) = (x.floor(), y.floor());
    let (ax, ay) = (x - fx, y - fy);
    let (ix, iy) = (fx as i32, fy as i32);
    for (dx, dy, k) in [
        (0, 0, (1.0 - ax) * (1.0 - ay)),
        (1, 0, ax * (1.0 - ay)),
        (0, 1, (1.0 - ax) * ay),
        (1, 1, ax * ay),
    ] {
        let (px, py) = (ix + dx, iy + dy);
        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
            buf[py as usize * w + px as usize] += c * k;
        }
    }
}

// ─────────────────────────── post-processing ───────────────────────────

pub fn bloom(fb: &mut Fb, thr: f32, k: f32) {
    let (w, h) = (fb.w, fb.h);
    let mut a: Vec<V3> = fb
        .px
        .par_iter()
        .map(|c| {
            let l = c.max_elem();
            if l > thr { *c * ((l - thr) / l) } else { V3::default() }
        })
        .collect();
    let mut b = vec![V3::default(); w * h];
    let r = (w.max(h) / 45).max(2) as i32;
    for _ in 0..2 {
        blur_h(&a, &mut b, w, r);
        blur_v(&b, &mut a, w, h, r);
    }
    fb.px.par_iter_mut().zip(a.par_iter()).for_each(|(p, g)| *p += *g * k);
}

fn blur_h(src: &[V3], dst: &mut [V3], w: usize, r: i32) {
    let n = (2 * r + 1) as f32;
    let wi = w as i32;
    dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        let mut acc = V3::default();
        for i in -r..=r {
            acc += s[i.clamp(0, wi - 1) as usize];
        }
        for x in 0..wi {
            row[x as usize] = acc / n;
            acc += s[(x + r + 1).min(wi - 1) as usize] - s[(x - r).max(0) as usize];
        }
    });
}

fn blur_v(src: &[V3], dst: &mut [V3], w: usize, h: usize, r: i32) {
    let n = (2 * r + 1) as f32;
    let hi = h as i32;
    for x in 0..w {
        let at = |y: i32| src[y.clamp(0, hi - 1) as usize * w + x];
        let mut acc = V3::default();
        for i in -r..=r {
            acc += at(i);
        }
        for y in 0..hi {
            dst[y as usize * w + x] = acc / n;
            acc += at(y + r + 1) - at(y - r);
        }
    }
}



// ───────────────────────────── terminal out ────────────────────────────

/// HDR → 8-bit: hot channels bleed into white, then a soft shoulder.
pub fn tone(c: V3) -> [u8; 3] {
    let m = c.max_elem();
    let c = if m > 1.0 { c + s3((m - 1.0) * 0.35) } else { c };
    let f = |v: f32| {
        let v = v.max(0.0);
        let v = if v < 0.85 { v } else { 0.85 + 0.15 * (1.0 - (-(v - 0.85) / 0.15).exp()) };
        ((v * 255.0 / 8.0).round() * 8.0).min(255.0) as u8
    };
    [f(c.x), f(c.y), f(c.z)]
}

/// 4×4 ordered-dither thresholds, tiled across the whole screen so patterns line up.
const BAYER: [[f32; 4]; 4] = [
    [0.5 / 16.0, 8.5 / 16.0, 2.5 / 16.0, 10.5 / 16.0],
    [12.5 / 16.0, 4.5 / 16.0, 14.5 / 16.0, 6.5 / 16.0],
    [3.5 / 16.0, 11.5 / 16.0, 1.5 / 16.0, 9.5 / 16.0],
    [15.5 / 16.0, 7.5 / 16.0, 13.5 / 16.0, 5.5 / 16.0],
];

/// Squeeze a 2×4 pixel block into one two-colour cell: the darkest and brightest
/// pixels become bg/fg, and everything in between is ordered-dithered — crisp
/// edges stay crisp, glows and gradients turn into 1-bit-style patterns.
fn octant_cell(fb: &Fb, r: usize, c: usize) -> Cell {
    let w = fb.w;
    let mut p = [V3::default(); 8];
    for (k, v) in p.iter_mut().enumerate() {
        *v = fb.px[(4 * r + k / 2) * w + 2 * c + k % 2];
    }
    let lum = |v: V3| v.x * 0.3 + v.y * 0.59 + v.z * 0.11;
    let (mut lo, mut hi) = (0, 0);
    for k in 1..8 {
        if lum(p[k]) < lum(p[lo]) {
            lo = k;
        }
        if lum(p[k]) > lum(p[hi]) {
            hi = k;
        }
    }
    let (lo, hi) = (p[lo], p[hi]);
    let (tl, th) = (tone(lo), tone(hi));
    if tl == th {
        let a = tone(p.iter().fold(V3::default(), |a, &v| a + v) / 8.0);
        return Cell { ch: ' ', fg: a, bg: a };
    }
    let d = hi - lo;
    let dd = (d.x * d.x + d.y * d.y + d.z * d.z).max(1e-9);
    let mut mask = 0usize;
    for (k, v) in p.iter().enumerate() {
        let e = *v - lo;
        let t = (e.x * d.x + e.y * d.y + e.z * d.z) / dd;
        if t > BAYER[(4 * r + k / 2) % 4][(2 * c + k % 2) % 4] {
            mask |= 1 << k;
        }
    }
    match mask {
        0 => Cell { ch: ' ', fg: tl, bg: tl },
        255 => Cell { ch: ' ', fg: th, bg: th },
        m => Cell { ch: crate::octant::OCTANT[m], fg: th, bg: tl },
    }
}

/// Text colours are authored, not HDR: map them straight through.
fn exact(c: V3) -> [u8; 3] {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [f(c.x), f(c.y), f(c.z)]
}

/// Fit each octant's two colours to its surface, without ordered-dither grain.
fn fitted_cell(fb: &Fb, r: usize, c: usize) -> Cell {
    let p: [V3; 8] = std::array::from_fn(|k| {
        let [r, g, b] = tone(fb.px[(4 * r + k / 2) * fb.w + 2 * c + k % 2]);
        v3(r as f32, g as f32, b as f32) / 255.0
    });
    let dist = |a: V3, b: V3| { let d = a - b; d.x * d.x + d.y * d.y + d.z * d.z };
    // seed with the most distinct colours, then fit two local means in display space
    let (mut lo, mut hi, mut far) = (p[0], p[0], 0.0);
    for i in 0..8 { for j in i + 1..8 {
        let d = dist(p[i], p[j]);
        if d > far { lo = p[i]; hi = p[j]; far = d; }
    } }
    let mut mask = 0;
    for _ in 0..4 {
        let (mut a, mut b, mut n) = (V3::default(), V3::default(), 0);
        mask = 0;
        for (k, &v) in p.iter().enumerate() {
            if dist(v, hi) < dist(v, lo) { b += v; n += 1; mask |= 1 << k; } else { a += v; }
        }
        if n == 0 || n == 8 { return Cell { ch: ' ', fg: exact((a + b) / 8.0), bg: exact((a + b) / 8.0) }; }
        lo = a / (8 - n) as f32; hi = b / n as f32;
    }
    Cell { ch: crate::octant::OCTANT[mask], fg: exact(hi), bg: exact(lo) }
}

#[cfg(test)]
mod graphics_tests {
    use super::*;

    fn error(fb: &Fb, cell: Cell) -> f32 {
        let mask = crate::octant::OCTANT.iter().position(|&c| c == cell.ch).unwrap();
        (0..8).map(|k| {
            let a = tone(fb.px[(k / 2) * fb.w + k % 2]);
            let b = if mask & (1 << k) == 0 { cell.bg } else { cell.fg };
            (0..3).map(|i| (a[i] as f32 - b[i] as f32).powi(2)).sum::<f32>()
        }).sum()
    }

    #[test]
    fn default_encoder_reduces_terminal_reconstruction_error() {
        let mut fb = Fb::new(20, 8);
        for k in 0..8 { fb.px[(k / 2) * fb.w + k % 2] = v3(0.1, 0.3, 0.6) * (0.15 + k as f32 * 0.11); }
        let mut out = Out::new(fb.cols * fb.rows);
        out.present(&fb, &mut io::sink()).unwrap();
        assert!(error(&fb, out.prev[0].unwrap()) < error(&fb, octant_cell(&fb, 0, 0)) * 0.65);
    }

    #[test]
    fn fitted_cells_preserve_silhouettes_and_text() {
        let mut fb = Fb::new(20, 8);
        for mask in 0..256 {
            for k in 0..8 { fb.px[(k / 2) * fb.w + k % 2] = if mask & (1 << k) == 0 { v3(0.0, 0.0, 8.0 / 255.0) } else { v3(160.0, 200.0, 208.0) / 255.0 }; }
            assert_eq!(error(&fb, fitted_cell(&fb, 0, 0)), 0.0, "silhouette {mask}");
        }
        let (fg, bg) = (v3(0.8, 0.6, 0.4), v3(0.04, 0.06, 0.08));
        fb.put_bg(0, 0, 'A', fg, Some(bg));
        let mut out = Out::new(fb.cols * fb.rows);
        out.present(&fb, &mut io::sink()).unwrap();
        let cell = out.prev[0].unwrap();
        assert_eq!((cell.ch, cell.fg, cell.bg), ('A', exact(fg), exact(bg)));
        let mut unchanged = Vec::new();
        out.present(&fb, &mut unchanged).unwrap();
        assert_eq!(unchanged, b"\x1b[?2026h\x1b[?2026l");
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    fg: [u8; 3],
    bg: [u8; 3],
}

/// Encodes the framebuffer as octant cells, only re-sending cells that changed
/// since the previous frame.
pub struct Out {
    prev: Vec<Option<Cell>>,
    buf: Vec<u8>,
    pub bytes: usize,
    fitted: bool,
}

impl Out {
    pub fn new(cells: usize) -> Self {
        Self { prev: vec![None; cells], buf: Vec::with_capacity(1 << 20), bytes: 0, fitted: true }
    }

    /// Keep the classic encoder available for the offline comparison.
    pub fn preview(cells: usize, fitted: bool) -> Self { Self { fitted, ..Self::new(cells) } }

    pub fn present(&mut self, fb: &Fb, w: &mut impl Write) -> io::Result<()> {
        let b = &mut self.buf;
        b.clear();
        b.extend_from_slice(b"\x1b[?2026h");
        let (cols, rows) = (fb.cols, fb.rows);
        let (mut lfg, mut lbg): (Option<[u8; 3]>, Option<[u8; 3]>) = (None, None);
        for r in 0..rows {
            let mut at = false;
            for c in 0..cols {
                let i = r * cols + c;
                let cell = match fb.txt[i] {
                    Some(t) if t.ch == '\0' => {
                        // covered by the wide glyph to the left: draw nothing, but
                        // make sure the next cell repositions the cursor
                        self.prev[i] = None;
                        at = false;
                        continue;
                    }
                    Some(t) => {
                        let bg = t.bg.unwrap_or_else(|| {
                            (0..8).fold(V3::default(), |a, k| a + fb.px[(4 * r + k / 2) * fb.w + 2 * c + k % 2]) / 40.0
                        });
                        Cell { ch: t.ch, fg: exact(t.fg), bg: exact(bg) }
                    }
                    None => if self.fitted { fitted_cell(fb, r, c) } else { octant_cell(fb, r, c) },
                };
                if self.prev[i] == Some(cell) {
                    at = false;
                    continue;
                }
                self.prev[i] = Some(cell);
                if !at {
                    write!(b, "\x1b[{};{}H", r + 1, c + 1)?;
                    at = true;
                }
                let need_fg = cell.ch != ' ' && lfg != Some(cell.fg);
                let need_bg = lbg != Some(cell.bg);
                let ([fr, fg, fb_], [br, bg, bb]) = (cell.fg, cell.bg);
                match (need_fg, need_bg) {
                    (true, true) => write!(b, "\x1b[38;2;{fr};{fg};{fb_};48;2;{br};{bg};{bb}m")?,
                    (true, false) => write!(b, "\x1b[38;2;{fr};{fg};{fb_}m")?,
                    (false, true) => write!(b, "\x1b[48;2;{br};{bg};{bb}m")?,
                    _ => {}
                }
                if need_fg {
                    lfg = Some(cell.fg);
                }
                if need_bg {
                    lbg = Some(cell.bg);
                }
                let mut tmp = [0u8; 4];
                b.extend_from_slice(cell.ch.encode_utf8(&mut tmp).as_bytes());
            }
        }
        b.extend_from_slice(b"\x1b[?2026l");
        self.bytes = b.len();
        w.write_all(b)?;
        w.flush()
    }
}
