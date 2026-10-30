//! Rendering: the galaxy (pixels), and the HUD, outliner, dialogs and comms
//! terminal (text cells).

use crate::game::*;
use crate::gfx::*;
use crate::sound::Sound;
use crate::survey;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::f32::consts::{PI, TAU};
use std::sync::atomic::Ordering::Relaxed;

const BAR: V3 = v3(0.035, 0.05, 0.085);
const PANEL: V3 = v3(0.022, 0.03, 0.052);
const BOX: V3 = v3(0.035, 0.05, 0.085);
const SELBG: V3 = v3(0.09, 0.16, 0.27);
const DIM: V3 = v3(0.45, 0.52, 0.62);
const TEXT: V3 = v3(0.85, 0.88, 0.94);
const TERM_FG: V3 = v3(0.847, 0.871, 0.914);
const TERM_BG: V3 = v3(0.043, 0.059, 0.102);

pub fn frame(fb: &mut Fb, app: &mut App, w: &World, l: &Lay, t: f32, sound: Option<&Sound>) {
    fb.clear_text();
    if let Mode::Comms(id) = app.mode {
        top_bar(fb, app, w, l, t);
        comms(fb, app, l, t, id);
        if let Some((m, t0)) = &app.toast
            && t - t0 < 2.5
        {
            let m = format!(" {m} ");
            seg(fb, (l.cols - m.chars().count() as i32) / 2, 1, &m, l.cols, v3(0.6, 0.95, 0.7), Some(BOX));
        }
        return;
    }
    build_bg(app, fb, l);
    fb.px.copy_from_slice(&app.bg);
    galaxy(fb, app, w, l, t);
    bloom(fb, 0.5, 0.7);
    labels(fb, app, w, l, t);
    top_bar(fb, app, w, l, t);
    if l.pw > 0 {
        outliner(fb, app, w, l, t);
    }
    if matches!(app.mode, Mode::Galaxy) {
        card(fb, app, w, l, t);
        transmission(fb, app, l, t);
    }
    footer(fb, app, l, sound);
    if let Some((m, t0)) = &app.toast {
        let a = 1.0 - smoothstep(2.6, 3.2, t - t0);
        if a > 0.0 {
            let cw = l.cols - l.pw;
            let s = format!(" {m} ");
            fb.text((cw - s.chars().count() as i32).max(0) / 2, l.rows - 3, &s, TEXT * a);
        }
    }
    match &app.mode {
        Mode::Colonize(c) => colonize(fb, app, c, l, t),
        Mode::Deploy { planet, kind } => deploy(fb, app, planet, *kind, l),
        Mode::Confirm { id } => confirm(fb, app, *id, l),
        Mode::Help => help(fb, l),
        Mode::Brief { planet, text, kind, launch, attachments, sending } => brief(fb, app, planet, text, *kind, *launch, attachments, *sending, l, t),
        Mode::Fleet { target, text } => fleet_order(fb, app, w, target, text, l, t),
        Mode::Moon { planet, branch, kind } => moon(fb, app, planet, branch, *kind, l, t),
        Mode::Actions { planet, sel } => actions(fb, app, planet, *sel, l),
        Mode::Log { sel, open } => mission_log(fb, app, *sel, open, l, t),
        Mode::Dock { moon } => dock(fb, app, moon, l),
        _ => {}
    }
    if !app.online {
        let m = format!(" ○ daemon offline · {} · reconnecting… ", app.err);
        fb.text(2, 2, &m, v3(1.0, 0.4, 0.3));
    } else if app.loaded && app.agents.is_empty() && matches!(app.mode, Mode::Galaxy) {
        let cw = l.cols - l.pw;
        let lines = ["THE GALAXY IS UNCHARTED", "", "press  n  to colonize your first world"];
        for (i, s) in lines.iter().enumerate() {
            let c = if i == 0 { v3(0.6, 0.85, 1.0) } else { DIM };
            fb.text((cw - s.chars().count() as i32) / 2, l.rows / 2 - 1 + i as i32, s, c);
        }
    }
}

// ─────────────────────────────── galaxy ────────────────────────────────

fn build_bg(app: &mut App, fb: &Fb, l: &Lay) {
    let key = (fb.w, fb.h, l.pw);
    if app.bg_key == key {
        return;
    }
    app.bg_key = key;
    let (w, vw) = (fb.w, l.vw as usize);
    app.bg = (0..fb.w * fb.h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if x >= vw {
                return PANEL;
            }
            let mut c = v3(0.006, 0.008, 0.02);
            let st = rnd2(x as i32, y as i32, 11);
            if st > 0.9965 {
                let k = (st - 0.9965) / 0.0035;
                c += hsv(rnd2(x as i32, y as i32, 5) * 0.3 + 0.5, 0.3, 0.15 + k * 0.5);
            }
            c
        })
        .collect();
}

fn orb(fb: &mut Fb, x: f32, y: f32, r: f32, col: V3) {
    let e = (r * 3.0).ceil() as i32;
    let (ix, iy) = (x.round() as i32, y.round() as i32);
    for py in iy - e..=iy + e {
        for px in ix - e..=ix + e {
            let d = ((px as f32 - x).powi(2) + (py as f32 - y).powi(2)).sqrt();
            let core = smoothstep(r + 0.7, r - 0.7, d);
            let halo = (-(d / (r * 1.7)).powi(2)).exp() * 0.45;
            fb.add(px, py, col * (core + halo));
        }
    }
}

fn ellipse(fb: &mut Fb, x: f32, y: f32, rx: f32, ry: f32, col: V3) {
    let n = (TAU * rx.max(ry) * 1.2).clamp(12.0, 900.0) as u32;
    for i in 0..n {
        let a = TAU * i as f32 / n as f32;
        fb.splat(x + a.cos() * rx, y + a.sin() * ry, col);
    }
}

fn circle(fb: &mut Fb, x: f32, y: f32, r: f32, col: V3) {
    ellipse(fb, x, y, r, r, col);
}

fn arc(fb: &mut Fb, x: f32, y: f32, r: f32, a0: f32, a1: f32, col: V3) {
    let n = ((a1 - a0).abs() * r * 1.5).max(3.0) as u32;
    for i in 0..=n {
        let a = mixf(a0, a1, i as f32 / n as f32);
        fb.splat(x + a.cos() * r, y + a.sin() * r, col);
    }
}

fn vnoise(x: f32, y: f32, s: u32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (ix, iy) = (ix as i32, iy as i32);
    let a = mixf(rnd2(ix, iy, s), rnd2(ix + 1, iy, s), ux);
    let b = mixf(rnd2(ix, iy + 1, s), rnd2(ix + 1, iy + 1, s), ux);
    mixf(a, b, uy)
}

fn fbm(x: f32, y: f32, s: u32) -> f32 {
    vnoise(x, y, s) * 0.5 + vnoise(x * 2.1, y * 2.1, s + 1) * 0.3 + vnoise(x * 4.3, y * 4.3, s + 2) * 0.2
}

/// Surface of a planet type: (albedo, emission, atmosphere).
fn terrain(kind: u8, lon: f32, lat: f32, seed: f32) -> (V3, V3, V3) {
    let s = (seed * 1_000_000.0) as u32;
    let scale = 1.2 + rnd(s ^ 71) * 1.8;
    let (x, y) = (lon.cos() * scale + 10.0, lon.sin() * scale + lat * 2.6);
    let n = fbm(x, y, s);
    let (albedo, emission, atmosphere) = match kind {
        0 => {
            // terran
            let coast = 0.38 + rnd(s ^ 19) * 0.22;
            let land = smoothstep(coast, coast + 0.06, n);
            let ground = mix(v3(0.22, 0.5, 0.22), v3(0.55, 0.45, 0.28), fbm(x * 2.0, y * 2.0, s + 9));
            let mut c = mix(v3(0.08, 0.22, 0.55) + s3(n * 0.08), ground, land);
            if lat.abs() > 0.68 + rnd(s ^ 23) * 0.24 {
                c = v3(0.9, 0.95, 1.0);
            }
            (c, V3::default(), v3(0.4, 0.7, 1.0))
        }
        1 => (mix(v3(0.04, 0.18, 0.48), v3(0.25, 0.55, 0.85), smoothstep(0.55, 0.75, n)), V3::default(), v3(0.3, 0.6, 1.0)),
        2 => {
            let b = (lat * (5.0 + rnd(s ^ 31) * 8.0) + n * 4.0).sin() * 0.5 + 0.5;
            (mix(v3(0.88, 0.62, 0.35), v3(0.58, 0.33, 0.18), b), V3::default(), v3(1.0, 0.7, 0.4))
        }
        3 => {
            let crack = (-((n - 0.5) * 30.0).powi(2)).exp();
            (mix(v3(0.86, 0.93, 1.0), v3(0.5, 0.7, 0.86), n) - s3(crack * 0.3), V3::default(), v3(0.7, 0.9, 1.0))
        }
        4 => {
            let b = (lat * (8.0 + rnd(s ^ 31) * 13.0) + n * 2.5).sin() * 0.5 + 0.5;
            let storm_lon = rnd(s ^ 37) * TAU;
            let storm_lat = rnd(s ^ 41) * 1.1 - 0.55;
            let storm = (-((1.0 - (lon - storm_lon).cos()) * 12.0 + (lat - storm_lat).powi(2) * 40.0)).exp();
            let c = mix(v3(0.88, 0.72, 0.5), v3(0.55, 0.35, 0.62), b);
            (mix(c, v3(0.95, 0.45, 0.3), storm), V3::default(), v3(0.9, 0.7, 1.0))
        }
        5 => {
            let crack = (-((n - 0.5) * 22.0).powi(2)).exp();
            (v3(0.14, 0.09, 0.09), v3(1.0, 0.35, 0.08) * crack * 1.6, v3(1.0, 0.4, 0.2))
        }
        _ => (mix(v3(0.62, 0.76, 0.2), v3(0.3, 0.45, 0.15), n), V3::default(), v3(0.7, 1.0, 0.3)),
    };
    // Keep each world's class legible while giving it its own mineral palette.
    let tint = hsv(rnd(s ^ 53), 0.25 + rnd(s ^ 59) * 0.4, 1.0);
    let palette = mix(s3(1.0), tint, 0.65);
    (albedo * palette, emission * palette, atmosphere * palette)
}

/// A lit, rotating planet with an atmosphere rim, lit from `light` (toward the star).
fn sphere(fb: &mut Fb, cx: f32, cy: f32, r: f32, p: &Planet, light: (f32, f32), t: f32) {
    let l = {
        let v = v3(light.0, light.1, 0.55);
        v / (v.x * v.x + v.y * v.y + v.z * v.z).sqrt()
    };
    let e = r + 4.0;
    let tilt = (rnd(p.seed.to_bits() ^ 83) - 0.5) * 1.4;
    let (st, ct) = tilt.sin_cos();
    let (_, _, atm) = terrain(p.terrain, 0.0, 0.0, p.seed);
    for py in (cy - e).floor() as i32..=(cy + e).ceil() as i32 {
        for px in (cx - e).floor() as i32..=(cx + e).ceil() as i32 {
            let (dx, dy) = ((px as f32 + 0.5 - cx) / r, (py as f32 + 0.5 - cy) / r);
            let d2 = dx * dx + dy * dy;
            let Some(i) = fb.idx(px, py) else { continue };
            if d2 > 1.0 {
                let d = d2.sqrt();
                let lit = ((dx * l.x + dy * l.y) / d).max(0.0) * 0.7 + 0.3;
                fb.px[i] += atm * (-(d - 1.0) * r * 0.9).exp() * 0.4 * lit;
                continue;
            }
            let nz = (1.0 - d2).sqrt();
            let latitude = dx * st + dy * ct;
            let lon = (dx * ct - dy * st).atan2(nz) + p.seed * TAU + t * 0.04 * (0.5 + p.seed);
            let (alb, emit, atm) = terrain(p.terrain, lon, latitude, p.seed);
            let ndl = dx * l.x + dy * l.y + nz * l.z;
            let day = smoothstep(-0.12, 0.5, ndl);
            let mut c = alb * (0.035 + 1.05 * day) + emit * (1.0 - 0.6 * day) + atm * (1.0 - nz).powi(3) * 0.9 * day;
            // a city world glitters on its night side
            if p.level >= 2 && day < 0.35 {
                let cell = ((lon * 16.0).floor() as i32, (dy * 12.0).floor() as i32);
                let lit = rnd((cell.0 as u32).wrapping_mul(73_856_093) ^ (cell.1 as u32).wrapping_mul(19_349_663) ^ p.seed.to_bits());
                let density = if p.level >= 3 { 0.45 } else { 0.72 };
                if lit > density && fract(lon * 16.0) < 0.5 && fract(dy * 12.0) < 0.5 {
                    c += v3(1.0, 0.75, 0.35) * (0.35 - day) * 2.2;
                }
            }
            // a neglected world goes grey and dim under the fog
            let grey = s3((c.x + c.y + c.z) / 3.0);
            fb.px[i] = mix(c, mix(grey, v3(0.35, 0.3, 0.5) * grey.x * 2.0, 0.5), p.fog) * (1.0 - 0.7 * p.fog);
        }
    }
    // and the nebula closes in around it
    if p.fog > 0.3 {
        let e = r * 2.6 + 6.0;
        for py in (cy - e).floor() as i32..=(cy + e).ceil() as i32 {
            for px in (cx - e).floor() as i32..=(cx + e).ceil() as i32 {
                let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt() / e;
                if d >= 1.0 {
                    continue;
                }
                let Some(i) = fb.idx(px, py) else { continue };
                let n = fbm(px as f32 * 0.045 + t * 0.03, py as f32 * 0.06, p.seed.to_bits());
                let haze = smoothstep(0.35, 0.8, n) * (1.0 - d * d) * (p.fog - 0.3) * 0.5;
                fb.px[i] = mix(fb.px[i], v3(0.22, 0.14, 0.34), haze);
            }
        }
    }
}

/// Seeded silhouettes, kept inside the existing ship selection radius.
fn hull(fb: &mut Fb, (x, y): (f32, f32), s: f32, seed: u32, col: V3) {
    let angle = rnd(seed ^ 101) * TAU;
    let (sn, cs) = angle.sin_cos();
    let width = 0.8 + rnd(seed ^ 103) * 0.35;
    let point = |(u, v): (f32, f32)| {
        (x + (u * cs - v * width * sn) * s, y + (u * sn + v * width * cs) * s)
    };
    let vertices: &[(f32, f32)] = match crate::gfx::hash(seed ^ 107) % 4 {
        0 => &[(3.8, 0.0), (-2.5, -2.6), (-1.0, 0.0), (-2.5, 2.6)], // interceptor
        1 => &[(3.5, 0.0), (0.0, -2.5), (-3.5, 0.0), (0.0, 2.5)], // scout
        2 => &[(3.0, -1.2), (3.0, 1.2), (-2.8, 2.2), (-2.8, -2.2)], // frigate
        _ => &[(3.7, 0.0), (-1.8, -3.0), (-0.5, -0.7), (-3.0, 0.0), (-0.5, 0.7), (-1.8, 3.0)],
    };
    for i in 0..vertices.len() {
        let (ax, ay) = point(vertices[i]);
        let (bx, by) = point(vertices[(i + 1) % vertices.len()]);
        fb.line(ax, ay, bx, by, col * 0.65, col * 0.65);
    }
    let (ex, ey) = point((-2.8, 0.0));
    let (tx, ty) = point((-4.3 - rnd(seed ^ 109), 0.0));
    fb.line(ex, ey, tx, ty, col * 0.7, col * 0.05);
}

fn ship(fb: &mut Fb, a: &Agent, (x, y): (f32, f32), z: f32, t: f32) {
    let grow = smoothstep(a.born, a.born + 0.8, t) * a.dying.map_or(1.0, |d| 1.0 - smoothstep(d, d + 0.8, t));
    if grow <= 0.0 {
        return;
    }
    let s = z.clamp(0.7, 2.2) * grow;
    let col = a.status.color();
    match a.status {
        Status::Working => {
            orb(fb, x, y, 0.9 * s, col * (0.9 + 0.3 * (t * 6.0 + a.seed * 9.0).sin()));
            let ang = t * 2.5 + a.seed * TAU;
            fb.splat(x + ang.cos() * 4.5 * s, y + ang.sin() * 4.5 * s, col);
        }
        Status::Blocked => {
            let on = fract(t * 2.0 + a.seed) < 0.5;
            orb(fb, x, y, 1.1 * s, col * if on { 1.8 } else { 0.5 });
            for k in 0..2 {
                let p = fract(t / 1.3 + k as f32 * 0.5);
                circle(fb, x, y, (2.5 + p * 14.0) * s, col * (1.0 - p) * 0.6);
            }
        }
        Status::Done => {
            orb(fb, x, y, 1.0 * s, col);
            let tw = rnd((t * 8.0) as u32 ^ (a.seed * 1e6) as u32);
            if tw > 0.7 {
                let ang = tw * 40.0;
                fb.splat(x + ang.cos() * 4.0 * s, y + ang.sin() * 4.0 * s, s3(0.9));
            }
        }
        Status::Booting => orb(fb, x, y, 0.9 * s, col * if fract(t * 3.0) < 0.5 { 1.2 } else { 0.4 }),
        Status::Idle => orb(fb, x, y, 0.8 * s, col * (0.55 + 0.12 * (t * 1.2 + a.seed * 6.0).sin())),
        Status::Exited => orb(fb, x, y, 0.7 * s, col * 0.4),
    }
    hull(fb, (x, y), s, a.seed.to_bits(), mix(col, kind_color(&a.kind), 0.25) * grow);
    // The agent provider remains a small, consistent navigation light.
    fb.splat(x, y - 2.0 * s, kind_color(&a.kind) * grow);
}

fn star(fb: &mut Fb, (x, y): (f32, f32), r: f32, seed: u32, col: V3, t: f32) {
    let core = r * (0.72 + rnd(seed ^ 113) * 0.28);
    orb(fb, x, y, core, col * 1.5);
    let rays = 5 + crate::gfx::hash(seed ^ 127) % 5;
    let phase = rnd(seed ^ 131) * TAU;
    for k in 0..rays {
        let a = phase + k as f32 * TAU / rays as f32;
        let reach = core * (1.5 + rnd(seed ^ k ^ 137) * 0.9);
        let shimmer = 0.85 + 0.15 * (t * 0.6 + k as f32).sin();
        fb.line(x + a.cos() * core, y + a.sin() * core,
            x + a.cos() * reach, y + a.sin() * reach, col * 0.3 * shimmer, col * 0.015);
    }
    circle(fb, x, y, core * (1.5 + rnd(seed ^ 139) * 0.5), col * 0.1);
}

fn galaxy(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let cam = &app.cam;
    let z = cam.z;
    let scr = |p: (f32, f32)| l.to_screen(cam, p);

    for &(i, j) in &w.lanes {
        let (a, b) = (scr(w.systems[i].wpos), scr(w.systems[j].wpos));
        fb.line(a.0, a.1, b.0, b.1, v3(0.03, 0.08, 0.11), v3(0.03, 0.08, 0.11));
        let s = fract(t * 0.12 + (i * 7 + j * 3) as f32 * 0.13);
        fb.splat(mixf(a.0, b.0, s), mixf(a.1, b.1, s), v3(0.3, 0.8, 1.0) * 0.6);
    }

    if let Some(st) = w.station {
        trade(fb, app, w, scr(st), &scr, z, t);
    }

    for s in &w.systems {
        let (sx, sy) = scr(s.wpos);
        let seed = s.key.bytes().fold(7u32, |h, b| h.wrapping_mul(31) ^ b as u32);
        let orbit_col = mix(v3(0.03, 0.065, 0.085), s.star * 0.07, 0.45);
        for &r in &s.orbits {
            ellipse(fb, sx, sy, r * z, r * 0.55 * z, orbit_col);
        }
        // Some systems have a quiet inner asteroid belt, clear of planet orbits.
        if rnd(seed ^ 149) > 0.45 {
            for k in 0..64 {
                let a = rnd(seed ^ (k * 3 + 151)) * TAU;
                let r = (38.0 + rnd(seed ^ (k * 3 + 152)) * 9.0) * z;
                fb.splat(sx + a.cos() * r, sy + a.sin() * r * 0.55,
                    mix(s3(0.12), s.star * 0.22, rnd(seed ^ (k * 3 + 153))));
            }
        }
        let rs = (8.0 * z).max(2.2);
        if s.planets.iter().any(|&i| w.planets[i].level >= 3) {
            for k in 0..36 {
                let a = k as f32 / 36.0 * TAU + t * 0.4 * if k % 2 == 0 { 1.0 } else { -0.7 };
                let tilt = (k % 3) as f32 * 0.35;
                let rr = rs * (2.3 + 0.25 * (k % 3) as f32);
                fb.splat(sx + a.cos() * rr, sy + a.sin() * rr * (0.35 + tilt), v3(1.0, 0.8, 0.35) * 0.8);
            }
        }
        star(fb, (sx, sy), rs, seed, s.star, t);
    }

    for (pi, p) in w.planets.iter().enumerate() {
        let (px, py) = scr(p.wpos);
        let star = w.system_of(pi).map_or(p.wpos, |s| s.wpos);
        let (lx, ly) = (star.0 - p.wpos.0, star.1 - p.wpos.1);
        let ll = (lx * lx + ly * ly).sqrt().max(1e-3);
        let r = (p.r * z).max(2.5);
        let ships = |front: bool, fb: &mut Fb| {
            for &ai in &p.agents {
                let a = &app.agents[ai];
                if a.front == front {
                    ship(fb, a, scr(a.wpos), z, t);
                }
            }
        };
        let rings = |front: bool, fb: &mut Fb| {
            if p.level == 0 || p.dormant && p.fog > 0.6 {
                return;
            }
            let bands = if p.level >= 3 { 3 } else { p.level as usize };
            let seed = p.seed.to_bits();
            let tilt = (rnd(seed ^ 157) - 0.5) * 1.2;
            let (st, ct) = tilt.sin_cos();
            let flatten = 0.2 + rnd(seed ^ 163) * 0.22;
            let tint = hsv(rnd(seed ^ 167), 0.3, 1.0);
            for b in 0..bands {
                let rr = r * (1.55 + 0.22 * b as f32);
                let n = (rr * 7.0) as usize + 24;
                for k in 0..n {
                    let a = k as f32 / n as f32 * TAU;
                    if (a.sin() > 0.0) != front {
                        continue;
                    }
                    let (u, v) = (a.cos() * rr, a.sin() * rr * flatten);
                    let (x, y) = (px + u * ct - v * st, py + u * st + v * ct);
                    let col = [v3(0.85, 0.75, 0.55), v3(0.7, 0.8, 0.95), v3(1.0, 0.85, 0.4)][b] * tint * (0.35 + 0.15 * (k % 3) as f32);
                    fb.splat(x, y, col);
                }
            }
        };
        ships(false, fb);
        rings(false, fb);
        sphere(fb, px, py, r, p, (lx / ll, ly / ll), t);
        rings(true, fb);
        let live = || p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none());
        if live().any(|a| a.status == Status::Blocked) {
            circle(fb, px, py, r + 3.5, Status::Blocked.color() * (0.4 + 0.4 * (t * 4.0).sin().abs()));
        } else if live().any(|a| a.status == Status::Done) {
            circle(fb, px, py, r + 3.5, Status::Done.color() * 0.35);
        }
        ships(true, fb);
    }

    // border conflicts: rivals trade laser fire, their world flashes
    for a in app.agents.iter().filter(|a| a.dying.is_none()) {
        for war in a.war.iter().filter(|w| w.rival > a.id) {
            let Some(b) = app.agents.iter().find(|b| b.id == war.rival && b.dying.is_none()) else { continue };
            let (p, q) = (scr(a.wpos), scr(b.wpos));
            let phase = fract(t * 1.7 + a.id as f32 * 0.3);
            let (from, to) = if (t * 0.85) as i32 % 2 == 0 { (p, q) } else { (q, p) };
            let hot = v3(1.0, 0.3, 0.15) * (0.4 + 0.6 * (1.0 - phase));
            fb.line(from.0, from.1, to.0, to.1, hot * 0.5, hot);
            fb.splat(mixf(from.0, to.0, phase), mixf(from.1, to.1, phase), v3(1.0, 0.85, 0.5));
            if let Some(pl) = w.planet(&a.cwd) {
                let (px, py) = scr(pl.wpos);
                let flash = if (t * 4.0) as i32 % 2 == 0 { v3(1.0, 0.3, 0.15) } else { v3(1.0, 0.65, 0.2) };
                circle(fb, px, py, (pl.r * z).max(2.5) + 5.0, flash * 0.6);
            }
        }
    }

    for f in &app.fx {
        match *f {
            Fx::Ring { x, y, t0, col, speed, dur } => {
                let (sx, sy) = scr((x, y));
                let a = (t - t0) / dur;
                circle(fb, sx, sy, (2.0 + (t - t0) * speed) * z.clamp(0.7, 2.0), col * (1.0 - a).powi(2) * 0.9);
            }
            Fx::Spark { x, y, vx, vy, t0, life, col } => {
                let a = t - t0;
                let k = (1.0 - (-a * 3.0).exp()) / 3.0;
                let (sx, sy) = scr((x + vx * k, y + vy * k + a * a * 4.0));
                fb.splat(sx, sy, col * (1.0 - a / life) * 1.3);
            }
        }
    }

    // target reticle
    let target = match &app.sel {
        Some(Sel::Agent(id)) => app.agent(*id).map(|a| (a.wpos, 6.0 * z.clamp(0.7, 2.2) + 2.0)),
        Some(Sel::Planet(k)) => w.planet(k).map(|p| (p.wpos, (p.r * z).max(2.5) + 6.0)),
        Some(Sel::Sys(k)) => w.systems.iter().find(|s| &s.key == k).map(|s| (s.wpos, (8.0 * z).max(2.2) + 8.0)),
        None => None,
    };
    if let Some((p, rr)) = target {
        let (x, y) = scr(p);
        let rr = rr + 0.8 * (t * 4.0).sin();
        for k in 0..4 {
            let a = t * 1.2 + k as f32 * PI * 0.5;
            arc(fb, x, y, rr, a, a + 0.6, s3(0.95));
        }
    }
}

// ──────────────────────────────── text ─────────────────────────────────

fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn pad(s: &str, w: usize) -> String {
    format!("{:<w$}", trunc(s, w))
}

fn ago(secs: f32) -> String {
    let s = secs.max(0.0) as u32;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..172_800 => format!("{}h{:02}", s / 3600, (s / 60) % 60),
        _ => format!("{}d", s / 86_400),
    }
}

/// Write `s` clipped to `max` cells, optionally over a solid background. Returns the next column.
fn seg(fb: &mut Fb, col: i32, row: i32, s: &str, max: i32, fg: V3, bg: Option<V3>) -> i32 {
    let mut n = 0;
    for ch in s.chars() {
        if n >= max {
            break;
        }
        fb.put_bg(col + n, row, ch, fg, bg);
        n += 1;
    }
    col + n
}

/// Status tallies for a set of agents, e.g. "▲1 ◐2".
fn tally<'a>(agents: impl Iterator<Item = &'a Agent>, t: f32) -> Vec<(String, V3)> {
    let mut v = Vec::new();
    let all: Vec<&Agent> = agents.collect();
    for s in [Status::Blocked, Status::Done, Status::Working, Status::Booting, Status::Idle] {
        let n = all.iter().filter(|a| a.status == s).count();
        if n > 0 {
            v.push((format!("{}{}", s.glyph(t), n), s.color()));
        }
    }
    v
}

fn ci_color(ci: &str) -> V3 {
    match ci {
        "fail" => v3(1.0, 0.3, 0.25),
        "pending" => v3(1.0, 0.75, 0.3),
        "pass" => v3(0.35, 1.0, 0.55),
        _ => v3(0.55, 0.65, 0.8),
    }
}

/// Every PR across the empire, with the world it's from.
fn all_prs(app: &App) -> Vec<(&String, &survey::Pr)> {
    let mut v: Vec<(&String, &survey::Pr)> = app.survey.worlds.iter().flat_map(|(k, w)| w.prs.iter().map(move |p| (k, p))).collect();
    v.sort_by_key(|(k, p)| ((*k).clone(), p.number));
    v
}

/// Trade routes to the GitHub station: freighters with unpushed commits on the way,
/// PRs docked around it in their CI colours.
fn trade(fb: &mut Fb, app: &App, w: &World, (sx, sy): (f32, f32), scr: &dyn Fn((f32, f32)) -> (f32, f32), z: f32, t: f32) {
    for p in w.planets.iter().filter(|p| p.host.is_none()) {
        let Some(s) = app.survey.worlds.get(&p.key).filter(|s| s.github) else { continue };
        let (px, py) = scr(p.wpos);
        // a dotted route
        let n = (((sx - px).powi(2) + (sy - py).powi(2)).sqrt() / 6.0) as usize;
        for i in 0..n {
            let f = i as f32 / n.max(1) as f32;
            if (i + (t * 3.0) as usize) % 3 == 0 {
                fb.splat(mixf(px, sx, f), mixf(py, sy, f), v3(0.15, 0.3, 0.4) * 0.5);
            }
        }
        // freighters: one per unpushed commit (up to 5), drifting toward the station
        let k = s.ahead.min(5);
        for i in 0..k {
            let f = fract(t * 0.06 + i as f32 / k as f32 + rnd(p.seed.to_bits()) * 0.3);
            let (x, y) = (mixf(px, sx, f), mixf(py, sy, f));
            orb(fb, x, y, 1.6 * z.clamp(0.8, 1.5), v3(1.0, 0.8, 0.35));
        }
    }
    let prs = all_prs(app);
    let worst = if prs.iter().any(|(_, p)| p.ci == "fail") {
        "fail"
    } else if prs.iter().any(|(_, p)| p.ci == "pending") {
        "pending"
    } else if prs.iter().any(|(_, p)| p.ci == "pass") {
        "pass"
    } else {
        "none"
    };
    let glow = ci_color(worst);
    let r = (13.0 * z).max(5.0);
    // a ring station: a hub, a slowly turning ring with lit windows
    circle(fb, sx, sy, r * 2.2, glow * 0.08);
    orb(fb, sx, sy, r * 0.45, v3(0.7, 0.8, 0.9));
    for i in 0..24 {
        let a = i as f32 / 24.0 * TAU + t * 0.25;
        let lit = (i + (t * 2.0) as usize) % 4 != 0;
        fb.splat(sx + a.cos() * r, sy + a.sin() * r * 0.55, if lit { glow * 0.9 } else { v3(0.25, 0.3, 0.35) });
    }
    for i in 0..4 {
        let a = i as f32 * PI * 0.5 + t * 0.25;
        fb.line(sx, sy, sx + a.cos() * r, sy + a.sin() * r * 0.55, v3(0.3, 0.35, 0.4), v3(0.3, 0.35, 0.4));
    }
    // docked PRs
    let n = prs.len().max(1) as f32;
    for (i, (_, pr)) in prs.iter().enumerate() {
        let a = i as f32 / n * TAU - t * 0.12;
        let (x, y) = (sx + a.cos() * r * 1.7, sy + a.sin() * r * 1.7 * 0.55);
        let blink = if pr.ci == "fail" { 0.5 + 0.5 * (t * 5.0).sin().abs() } else { 1.0 };
        orb(fb, x, y, 1.8 * z.clamp(0.8, 1.6), ci_color(pr.ci) * blink);
    }
}

fn labels(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let cam = &app.cam;
    let z = cam.z;
    let cw = l.cols - l.pw;
    let in_view = |c: i32, r: i32| c >= 0 && c < cw && r >= 1 && r < l.rows - 1;
    for s in &w.systems {
        let (sx, sy) = l.to_screen(cam, s.wpos);
        let name = s.name.to_uppercase();
        let len = name.chars().count() as i32;
        let (c, r) = ((sx / 2.0) as i32 - len / 2, ((sy - (8.0 * z).max(2.2) - 6.0) / 4.0) as i32);
        if in_view(c, r) && in_view(c + len - 1, r) {
            fb.text(c, r, &name, s.star * 0.75);
        }
    }
    if let Some(st) = w.station {
        let (sx, sy) = l.to_screen(cam, st);
        let prs = all_prs(app);
        let fails = prs.iter().filter(|(_, p)| p.ci == "fail").count();
        let mut s = format!("◇ GITHUB · {} PR{}", prs.len(), if prs.len() == 1 { "" } else { "s" });
        if fails > 0 {
            s.push_str(&format!(" · ✖{fails}"));
        }
        let len = s.chars().count() as i32;
        let (c, r) = ((sx / 2.0) as i32 - len / 2, ((sy + (13.0 * z).max(5.0) * 1.3 + 8.0) / 4.0) as i32);
        // slide it back into view rather than lose it at the edge of the map
        let c = c.min(cw - len - 1);
        if in_view(c, r) && (sx / 2.0) < cw as f32 {
            fb.text(c, r, &s, if fails > 0 { v3(1.0, 0.45, 0.4) } else { v3(0.6, 0.75, 0.9) });
        }
    }
    for p in &w.planets {
        let selected = app.sel.as_ref() == Some(&Sel::Planet(p.key.clone()));
        if p.r * z < 3.0 && !selected {
            continue;
        }
        let (px, py) = l.to_screen(cam, p.wpos);
        let parts = tally(p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none()), t);
        let len = p.name.chars().count() as i32 + parts.iter().map(|(s, _)| s.chars().count() as i32 + 1).sum::<i32>();
        let (mut c, r) = ((px / 2.0) as i32 - len / 2, ((py + (p.r * z).max(2.5) + 7.0) / 4.0) as i32);
        if !in_view(c, r) || !in_view(c + len, r) {
            continue;
        }
        let name = if p.host.is_some() { format!("☾ {}", p.name) } else { p.name.clone() };
        c = seg(fb, c, r, &name, len + 2, if selected { s3(1.0) } else { TEXT * (1.0 - 0.65 * p.fog) }, None);
        for (s, col) in parts {
            c = seg(fb, c + 1, r, &s, 8, col, None);
        }
        // what it needs, and its missions, on the line below
        let mut chips: Vec<(String, V3)> = app.needs(&p.key).into_iter().filter(|n| n.badge != "▲" && n.badge != "✔").map(|n| (n.badge, n.col)).collect();
        let ms = |st: &[&str]| app.missions.iter().filter(|m| m.cwd == p.key && st.contains(&m.state.as_str())).count();
        let (busy, queued) = (ms(&["sent", "active"]), ms(&["queued"]));
        if busy > 0 {
            chips.push((format!("★{busy}"), v3(0.4, 0.85, 1.0)));
        }
        if queued > 0 {
            chips.push((format!("☆{queued}"), v3(1.0, 0.85, 0.4)));
        }
        if !chips.is_empty() && p.fog < 0.6 {
            let w: i32 = chips.iter().map(|(s, _)| s.chars().count() as i32 + 1).sum::<i32>() - 1;
            let mut c = ((px / 2.0) as i32 - w / 2).max(0);
            if in_view(c, r + 1) && in_view(c + w, r + 1) {
                for (s, col) in chips {
                    c = seg(fb, c, r + 1, &s, 10, col, None) + 1;
                }
            }
        }
    }
    for a in app.agents.iter().filter(|a| a.dying.is_none()) {
        let selected = app.sel.as_ref() == Some(&Sel::Agent(a.id));
        if !(selected || a.status.needs_you()) || z < 0.35 {
            continue;
        }
        let (x, y) = l.to_screen(cam, a.wpos);
        let label = format!("{} {}", a.status.glyph(t), a.kind);
        let (c, r) = ((x / 2.0) as i32 + 3, (y / 4.0) as i32);
        let col = if a.status == Status::Blocked && fract(t * 1.5) < 0.5 { s3(1.0) } else { kind_color(&a.kind) };
        if in_view(c, r) {
            seg(fb, c, r, &label, cw - c, col, None);
        }
    }
}

fn top_bar(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    fb.panel(0, 0, l.cols, 1, BAR);
    let title = " ◆ AGENT EMPIRE ";
    for (k, ch) in title.chars().enumerate() {
        fb.put_bg(k as i32, 0, ch, hsv(0.52 + k as f32 * 0.015 - t * 0.04, 0.45, 1.0), Some(BAR));
    }
    let live: Vec<&Agent> = app.agents.iter().filter(|a| a.dying.is_none()).collect();
    let moons = w.planets.iter().filter(|p| p.host.is_some()).count();
    let moons = if moons > 0 { format!(" · {moons} moon{}", if moons == 1 { "" } else { "s" }) } else { String::new() };
    let stats = format!("  {} systems · {} worlds{moons} · {} agents  ", w.systems.len(), w.planets.len() - w.planets.iter().filter(|p| p.host.is_some()).count(), live.len());
    let mut x = seg(fb, title.chars().count() as i32, 0, &stats, l.cols, DIM, Some(BAR));
    for (s, c) in tally(live.iter().copied(), t) {
        x = seg(fb, x, 0, &format!("{s}  "), l.cols - x, c, Some(BAR));
    }
    let right = format!("{} daemon  {} ", if app.online { "●" } else { "○" }, clock());
    let rc = l.cols - right.chars().count() as i32;
    // usage left, Stellaris resource-bar style; drop the bars if space is tight
    for bars in [true, false] {
        let parts = resources(&app.survey.limits, bars);
        let w: i32 = parts.iter().map(|(s, _)| s.chars().count() as i32).sum();
        if rc - w - 2 > x {
            let mut rx = rc - w - 2;
            for (s, c) in parts {
                rx = seg(fb, rx, 0, &s, l.cols, c, Some(BAR));
            }
            break;
        }
    }
    if rc > x {
        let c = if app.online { v3(0.35, 1.0, 0.55) } else { v3(1.0, 0.35, 0.3) };
        seg(fb, rc, 0, &right, l.cols, c, Some(BAR));
    }
}

/// Colour for how much of an allowance is used up.
fn strain(used: f32) -> V3 {
    if used >= 85.0 {
        v3(1.0, 0.35, 0.3)
    } else if used >= 60.0 {
        v3(1.0, 0.75, 0.3)
    } else {
        v3(0.4, 0.95, 0.6)
    }
}

fn meter(frac: f32, n: usize) -> String {
    let k = ((frac.clamp(0.0, 1.0) * n as f32).round() as usize).min(n);
    format!("{}{}", "▰".repeat(k), "▱".repeat(n - k))
}

fn until(secs: i64) -> String {
    match secs.max(0) {
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h{:02}", s / 3600, s % 3600 / 60),
        s => format!("{}d{}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// `CLAUDE 5h ▰▰▰▱▱ 62% · wk 31%  ↻1h12   CODEX wk 28%`
fn resources(limits: &[survey::Limit], bars: bool) -> Vec<(String, V3)> {
    let mut out = Vec::new();
    let mut who: Vec<&str> = limits.iter().map(|l| l.who).collect();
    who.dedup();
    for name in who {
        let ls: Vec<&survey::Limit> = limits.iter().filter(|l| l.who == name).collect();
        out.push((format!("{} ", name.to_uppercase()), kind_color(name)));
        for (i, lim) in ls.iter().enumerate() {
            if i > 0 {
                out.push((" · ".into(), DIM * 0.7));
            }
            out.push((format!("{} ", lim.window), DIM));
            if bars {
                out.push((format!("{} ", meter(lim.used / 100.0, 5)), strain(lim.used)));
            }
            out.push((format!("{:.0}%", lim.used), strain(lim.used)));
        }
        // when the tightest window will refill, once it starts to matter
        if let Some(l) = ls.iter().max_by(|a, b| a.used.total_cmp(&b.used))
            && l.used >= 50.0
            && let Some(r) = l.resets
        {
            out.push((format!(" ↻{}", until(r - survey::now())), DIM));
        }
        out.push(("   ".into(), DIM));
    }
    out
}

// ─────────────────────────────── survey cards ──────────────────────────

const CLASSES: [&str; 7] = ["continental", "ocean", "desert", "arctic", "gas giant", "molten", "tropical"];

/// Rows of a card: (label, [(text, colour)]).
type Rows = Vec<(&'static str, Vec<(String, V3)>)>;

fn num(n: u32) -> String {
    if n >= 10_000 { format!("{:.0}k", n as f32 / 1000.0) } else if n >= 1000 { format!("{:.1}k", n as f32 / 1000.0) } else { n.to_string() }
}

/// What the agents on these worlds have been up to.
fn economy(rows: &mut Rows, agents: &[&Agent], t: f32) {
    let mut pop = vec![(format!("{} agent{}  ", agents.len(), if agents.len() == 1 { "" } else { "s" }), TEXT)];
    pop.extend(tally(agents.iter().copied(), t).into_iter().map(|(s, c)| (format!("{s} "), c)));
    rows.push(("population", pop));
    let turns: u32 = agents.iter().map(|a| a.turns).sum();
    let (add, rem): (u32, u32) = agents.iter().fold((0, 0), |(x, y), a| (x + a.report.added.unwrap_or(0), y + a.report.removed.unwrap_or(0)));
    let mut out = vec![(format!("{turns} turn{} ", if turns == 1 { "" } else { "s" }), TEXT)];
    if add + rem > 0 {
        out.push((format!("+{} ", num(add)), v3(0.4, 0.95, 0.6)));
        out.push((format!("−{}", num(rem)), v3(1.0, 0.45, 0.4)));
    }
    rows.push(("output", out));
    let cost: f32 = agents.iter().filter_map(|a| a.report.cost).sum();
    if cost > 0.0 {
        rows.push(("upkeep", vec![(format!("${cost:.2}"), v3(1.0, 0.85, 0.4)), (" this session".into(), DIM)]));
    }
}

fn planet_rows(app: &App, w: &World, p: &Planet, t: f32) -> Rows {
    let mut rows: Rows = Vec::new();
    let agents: Vec<&Agent> = p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none()).collect();
    let s = app.survey.worlds.get(&p.key);
    match s {
        Some(g) if g.git => {
            let mut b = vec![(format!("⎇ {} ", g.branch), v3(0.6, 0.85, 1.0))];
            if g.ahead > 0 {
                b.push((format!("↑{} ", g.ahead), v3(0.4, 0.95, 0.6)));
            }
            if g.behind > 0 {
                b.push((format!("↓{} ", g.behind), v3(1.0, 0.75, 0.3)));
            }
            if let Some(c) = g.last_commit {
                b.push((format!(" last commit {} ago", ago((survey::now() - c) as f32)), DIM));
            }
            rows.push(("dock", b));
            let stab = 100.0 - (g.dirty as f32 * 4.0).min(100.0);
            let col = strain(100.0 - stab);
            let note = if g.dirty == 0 { "  all committed".to_string() } else { format!("  {} uncommitted", g.dirty) };
            rows.push(("stability", vec![(meter(stab / 100.0, 8), col), (note, if g.dirty == 0 { DIM } else { v3(1.0, 0.75, 0.3) })]));
            if g.github || g.ahead > 0 {
                let mut tr = Vec::new();
                if g.ahead > 0 {
                    tr.push((format!("{} unpushed  ", g.ahead), v3(1.0, 0.8, 0.35)));
                }
                let prs = p.host.map(|h| &w.planets[h].key).and_then(|k| app.survey.worlds.get(k)).map_or(&g.prs, |h| &h.prs);
                for pr in prs.iter().take(4) {
                    let mark = match pr.ci {
                        "fail" => "✖",
                        "pending" => "◌",
                        "pass" => "✔",
                        _ => "·",
                    };
                    tr.push((format!("#{}{mark} ", pr.number), ci_color(pr.ci)));
                }
                if tr.is_empty() {
                    tr.push(("no open PRs".into(), DIM));
                }
                rows.push(("trade", tr));
            }
            if g.files > 0 {
                let mut d = vec![(format!("{} commits · {} files", num(g.commits), num(g.files)), TEXT)];
                if !g.lang.is_empty() {
                    d.push((format!(" · {}", g.lang), v3(0.8, 0.7, 1.0)));
                }
                rows.push(("size", d));
            }
        }
        Some(_) => rows.push(("dock", vec![("no git — uncharted territory".into(), DIM)])),
        None => rows.push(("dock", vec![("surveying…".into(), DIM)])),
    }
    if p.host.is_none() {
        let pts = app.prestige.worlds.get(&p.key).copied().unwrap_or(0);
        let lv = level_of(pts) as usize;
        let mut row = vec![(format!("★ {} ", LEVELS[lv].1), v3(1.0, 0.85, 0.4)), (format!("{pts} prestige"), TEXT)];
        if let Some((need, next)) = LEVELS.get(lv + 1) {
            let lo = LEVELS[lv].0;
            row.push((format!("  {} ", meter((pts - lo) as f32 / (need - lo) as f32, 6)), v3(1.0, 0.85, 0.4)));
            row.push((format!("{next} at {need}"), DIM));
        }
        rows.push(("development", row));
    }
    if p.dormant && p.host.is_none() {
        let age = ago((survey::now() as u64).saturating_sub(p.last_seen) as f32);
        let txt = if p.fog > 0.8 { "lost in the nebula" } else if p.fog > 0.5 { "fading" } else { "dormant" };
        rows.push(("status", vec![(format!("{txt} · quiet for {age}"), v3(0.6, 0.5, 0.85)), ("  e deploy · X forget".into(), DIM)]));
    }
    economy(&mut rows, &agents, t);
    let ctx: Vec<(String, V3)> = agents
        .iter()
        .filter_map(|a| a.report.ctx.map(|c| (a, c)))
        .flat_map(|(a, c)| [(format!("{} ", a.kind), kind_color(&a.kind)), (format!("{} {:.0}%  ", meter(c / 100.0, 5), c), strain(c))])
        .collect();
    if !ctx.is_empty() {
        rows.push(("context", ctx));
    }
    fleet_row(&mut rows, app, &agents);
    let needs = app.needs(&p.key);
    for (i, n) in needs.iter().take(4).enumerate() {
        let key = if n.act.is_some() { format!("{} ", needs.iter().take(i + 1).filter(|m| m.act.is_some()).count()) } else { "  ".into() };
        rows.push((if i == 0 { "needs  r" } else { "" }, vec![(key, DIM), (clip(&n.label, 44), n.col)]));
    }
    if needs.len() > 4 {
        rows.push(("", vec![(format!("  +{} more · r for all", needs.len() - 4), DIM)]));
    }
    let mut files: Vec<String> = agents.iter().flat_map(|a| a.war.iter().flat_map(|w| w.files.clone())).collect();
    files.sort();
    files.dedup();
    if !files.is_empty() {
        let who: Vec<String> = agents.iter().filter(|a| !a.war.is_empty()).map(|a| a.kind.clone()).collect();
        rows.push(("conflict", vec![(format!("⚔ {} ", who.join(" vs ")), v3(1.0, 0.45, 0.3)), (files.join(", "), TEXT)]));
    }
    // the quest log: what's under way first, then what's waiting, then what's done
    let mut ms: Vec<&crate::proto::Mission> = app.missions.iter().filter(|m| m.cwd == p.key).collect();
    let rank = |m: &crate::proto::Mission| match m.state.as_str() {
        "active" | "sent" => 0,
        "queued" => 1,
        _ => 2,
    };
    ms.sort_by_key(|m| (rank(m), std::cmp::Reverse(m.done_at.unwrap_or(m.at))));
    for (i, m) in ms.iter().take(4).enumerate() {
        let (g, c) = match m.state.as_str() {
            "active" | "sent" => ("◆", v3(0.4, 0.85, 1.0)),
            "queued" => ("○", DIM),
            _ => ("★", v3(1.0, 0.85, 0.4)),
        };
        let row = vec![(format!("{g} "), c), (clip(&m.text, 38), if m.state == "done" { DIM } else { TEXT })];
        rows.push((if i == 0 { "missions" } else { "" }, row));
    }
    if ms.len() > 4 {
        rows.push(("", vec![(format!("+{} more", ms.len() - 4), DIM)]));
    }
    rows
}

fn system_rows(app: &App, w: &World, s: &System, t: f32) -> Rows {
    let mut rows: Rows = Vec::new();
    let planets: Vec<&Planet> = s.planets.iter().map(|&i| &w.planets[i]).collect();
    let agents: Vec<&Agent> = planets.iter().flat_map(|p| p.agents.iter().map(|&i| &app.agents[i])).filter(|a| a.dying.is_none()).collect();
    rows.push(("worlds", vec![(format!("{} colonized", planets.len()), TEXT)]));
    if let Some(cap) = planets.iter().max_by_key(|p| p.agents.len()) {
        rows.push(("capital", vec![(cap.name.clone(), planet_color(cap))]));
    }
    economy(&mut rows, &agents, t);
    fleet_row(&mut rows, app, &agents);
    let surveyed: Vec<(&Planet, &survey::World)> = planets.iter().filter_map(|p| app.survey.worlds.get(&p.key).filter(|g| g.git).map(|g| (*p, g))).collect();
    let dirty: u32 = surveyed.iter().map(|(_, g)| g.dirty).sum();
    let restless = surveyed.iter().filter(|(_, g)| g.dirty > 0).count();
    if !surveyed.is_empty() {
        let txt = if dirty == 0 { "every world committed".to_string() } else { format!("{dirty} uncommitted on {restless} world{}", if restless == 1 { "" } else { "s" }) };
        rows.push(("unrest", vec![(txt, if dirty == 0 { DIM } else { v3(1.0, 0.75, 0.3) })]));
        let unpushed: u32 = surveyed.iter().map(|(_, g)| g.ahead).sum();
        if unpushed > 0 {
            rows.push(("trade", vec![(format!("{unpushed} commits waiting to be pushed"), v3(1.0, 0.75, 0.3))]));
        }
    }
    rows
}

/// A Stellaris-style info card for the selected world or system, bottom left of the map.
fn card(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let planet_key = match &app.sel {
        Some(Sel::Planet(k)) => Some(k.clone()),
        Some(Sel::Agent(id)) => app.agent(*id).map(|a| a.cwd.clone()),
        _ => None,
    };
    let (title, sub, accent, rows) = if let Some(p) = planet_key.as_deref().and_then(|k| w.planet(k)) {
        if let Some(h) = p.host {
            let sub = format!("moon of {} · worktree at {}", w.planets[h].name, tilde(&p.key));
            (format!("☾ {}", p.name.to_uppercase()), sub, planet_color(p), planet_rows(app, w, p, t))
        } else {
            let class = CLASSES[p.terrain as usize % CLASSES.len()];
            (format!("◉ {}", p.name.to_uppercase()), format!("{class} world · {}", tilde(&p.key)), planet_color(p), planet_rows(app, w, p, t))
        }
    } else if let Some(Sel::Sys(k)) = &app.sel
        && let Some(s) = w.systems.iter().find(|s| &s.key == k)
    {
        (format!("✶ {} SYSTEM", s.name.to_uppercase()), tilde(&s.key), s.star, system_rows(app, w, s, t))
    } else {
        return;
    };
    let accent = mix(accent * (0.9 / accent.x.max(accent.y).max(accent.z).max(0.05)), s3(1.0), 0.35);
    let cw = l.cols - l.pw;
    let width = 58.min(cw - 4);
    let height = rows.len() as i32 + 4;
    if width < 30 || height > l.rows - 6 {
        return;
    }
    let (x, y) = (2, l.rows - 2 - height);
    frame_box(fb, x, y, width, height, &title, accent);
    seg(fb, x + 2, y + 1, &trunc(&sub, (width - 4) as usize), width - 4, DIM, Some(BOX));
    for (i, (label, parts)) in rows.iter().enumerate() {
        let r = y + 3 + i as i32;
        seg(fb, x + 2, r, &pad(label, 12), 12, DIM * 0.9, Some(BOX));
        let mut cx = x + 15;
        for (s, c) in parts {
            if cx >= x + width - 2 {
                break;
            }
            cx = seg(fb, cx, r, s, x + width - 2 - cx, *c, Some(BOX));
        }
    }
}

/// A blocked agent's question, answerable without opening comms.
fn transmission(fb: &mut Fb, app: &App, l: &Lay, t: f32) {
    let Some(a) = app.transmission() else { return };
    let Some(q) = &a.ask else { return };
    let cw = l.cols - l.pw;
    let width = 76.min(cw - 4);
    if width < 36 {
        return;
    }
    let inner = (width - 4) as usize;
    let waiting = app.agents.iter().filter(|a| a.dying.is_none() && a.status == Status::Blocked).count();
    let mut body: Vec<(String, V3)> = Vec::new();
    for c in &q.context {
        body.push((trunc(c, inner), DIM));
    }
    body.push((trunc(&q.question, inner), TEXT));
    body.push((String::new(), DIM));
    let height = body.len() as i32 + q.options.len().min(9) as i32 + 3;
    let x = (cw - width) / 2;
    let y = 2;
    let red = Status::Blocked.color();
    let pulse = 0.75 + 0.25 * (t * 5.0).sin();
    let title = format!("▲ INCOMING TRANSMISSION · {} @ {}", a.kind, base_name(&a.cwd));
    frame_box(fb, x, y, width, height, &title, red * pulse);
    for (i, (s, c)) in body.iter().enumerate() {
        seg(fb, x + 2, y + 1 + i as i32, s, width - 4, *c, Some(BOX));
    }
    let oy = y + 1 + body.len() as i32;
    for (i, o) in q.options.iter().take(9).enumerate() {
        let r = oy + i as i32;
        let cx = seg(fb, x + 2, r, &format!(" {} ", i + 1), 3, BOX, Some(red * 0.85));
        seg(fb, cx + 1, r, &trunc(&o.label, inner - 4), width - 4 - (cx + 1 - x - 2), TEXT, Some(BOX));
    }
    let more = if waiting > 1 { format!(" · ⇥ next ({} waiting)", waiting - 1) } else { String::new() };
    let hint = format!(" 1-{} answer · ⏎ open comms{more} ", q.options.len().min(9));
    seg(fb, x + width - 2 - hint.chars().count() as i32, y + height - 1, &hint, width, DIM, Some(BOX));
}

fn footer(fb: &mut Fb, app: &App, l: &Lay, sound: Option<&Sound>) {
    fb.panel(0, l.rows - 1, l.cols, 1, BAR);
    let mute = match sound {
        None => "",
        Some(s) if s.muted.load(Relaxed) => " · m unmute",
        Some(_) => " · m mute",
    };
    let ctx = match &app.sel {
        Some(Sel::Agent(id)) if app.agent(*id).is_some_and(|a| !a.war.is_empty()) => "⏎ comms · P give priority · x decommission · ",
        Some(Sel::Agent(_)) => "⏎ comms · t mission · e deploy here · x decommission · ",
        Some(Sel::Planet(k)) if app.moon_host(k).is_some() => "⏎ comms · t mission · D dock · e deploy here · ",
        Some(Sel::Planet(_)) => "⏎ comms · r needs · t mission · b fleet · u moon · e deploy · ",
        Some(Sel::Sys(_)) => "⏎ zoom in · b fleet order · ",
        None => "",
    };
    let s = format!(" {ctx}n colonize · L missions · ⇥ next alert · ↑↓ select · wasd/drag pan · ± zoom · z fit{mute} · ? help · q quit");
    seg(fb, 0, l.rows - 1, &s, l.cols, DIM, Some(BAR));
}

/// Outliner rows on screen: (rows, first screen row, visible count, scroll offset).
fn outliner_view(app: &App, w: &World, l: &Lay) -> (Vec<Sel>, i32, i32, usize) {
    let rows = app.rows(w);
    let log_h = ((l.rows - 6) / 3).clamp(3, 10);
    let first = 3;
    let vis = (l.rows - 2 - log_h - 2 - first).max(1);
    let si = app.sel.as_ref().and_then(|s| rows.iter().position(|r| r == s)).unwrap_or(0);
    let off = si.saturating_sub(vis as usize - 1);
    (rows, first, vis, off)
}

pub fn panel_hit(app: &App, w: &World, l: &Lay, row: i32) -> Option<Sel> {
    let (rows, first, vis, off) = outliner_view(app, w, l);
    let k = row - first;
    (k >= 0 && k < vis).then(|| rows.get(off + k as usize).cloned()).flatten()
}

fn outliner(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let x0 = l.cols - l.pw;
    let pw = l.pw;
    fb.panel(x0, 1, pw, l.rows - 2, PANEL);
    for r in 1..l.rows - 1 {
        fb.put_bg(x0, r, '│', v3(0.1, 0.18, 0.24), Some(PANEL));
    }
    let (rows, first, vis, off) = outliner_view(app, w, l);
    seg(fb, x0 + 2, 1, "OUTLINER", pw, v3(0.6, 0.85, 1.0), Some(PANEL));
    seg(fb, x0 + 2, 2, &"─".repeat(pw as usize - 3), pw, v3(0.1, 0.2, 0.26), Some(PANEL));
    let inner = (pw - 3) as usize;
    for (k, sel) in rows.iter().enumerate().skip(off).take(vis as usize) {
        let row = first + (k - off) as i32;
        let me = app.sel.as_ref() == Some(sel);
        let bg = if me { SELBG } else { PANEL };
        if me {
            fb.panel(x0 + 1, row, pw - 1, 1, bg);
        }
        let c0 = x0 + 2;
        match sel {
            Sel::Sys(key) => {
                let s = w.systems.iter().find(|s| &s.key == key);
                let col = s.map_or(DIM, |s| s.star * 0.8);
                let name = base_name(key).to_uppercase();
                let x = seg(fb, c0, row, &format!("✶ {name} "), pw - 2, col, Some(bg));
                // the full path, trimmed from the left so the useful end stays visible
                let path = tilde(key);
                let room = (x0 + pw - 1 - x).max(0) as usize;
                let n = path.chars().count();
                let shown = if n > room { format!("…{}", path.chars().skip(n + 1 - room).collect::<String>()) } else { path };
                seg(fb, x, row, &shown, room as i32, DIM * 0.7, Some(bg));
            }
            Sel::Planet(key) => {
                let Some(p) = w.planet(key) else { continue };
                let parts = tally(p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none()), t);
                let tw: usize = parts.iter().map(|(s, _)| s.chars().count() + 1).sum();
                let (glyph, indent) = if p.host.is_some() { ("☾ ", 3) } else if p.dormant { ("◌ ", 1) } else { ("● ", 1) };
                let x = seg(fb, c0 + indent, row, glyph, 2, planet_color(p) * (1.0 - 0.6 * p.fog), Some(bg));
                if p.dormant && p.host.is_none() {
                    let age = ago((survey::now() as u64).saturating_sub(p.last_seen) as f32);
                    seg(fb, x, row, &pad(&p.name, inner.saturating_sub(4 + age.len())), pw, DIM * (1.1 - 0.5 * p.fog), Some(bg));
                    seg(fb, x0 + pw - 1 - age.len() as i32, row, &age, 8, DIM * 0.7, Some(bg));
                    continue;
                }
                seg(fb, x, row, &pad(&p.name, inner.saturating_sub(2 + indent as usize + tw)), pw, TEXT, Some(bg));
                let mut x = x0 + pw - 1 - tw as i32;
                for (s, c) in parts {
                    x = seg(fb, x, row, &s, 8, c, Some(bg)) + 1;
                }
            }
            Sel::Agent(id) => {
                let Some(a) = app.agent(*id) else { continue };
                let el = ago(t - a.since);
                let indent = if w.planet(&a.cwd).is_some_and(|p| p.host.is_some()) { 5 } else { 3 };
                let mut x = seg(fb, c0 + indent, row, &format!("{} ", a.status.glyph(t)), 2, a.status.color(), Some(bg));
                x = seg(fb, x, row, &pad(&a.kind, 7), 7, kind_color(&a.kind), Some(bg)) + 1;
                let room = (x0 + pw - 2 - el.len() as i32 - x).max(0);
                seg(fb, x, row, &pad(&a.title, room as usize), room, if me { TEXT } else { DIM }, Some(bg));
                seg(fb, x0 + pw - 1 - el.len() as i32, row, &el, 8, DIM, Some(bg));
            }
        }
    }
    if rows.is_empty() {
        seg(fb, x0 + 3, first, "no worlds yet — press n", pw - 4, DIM, Some(PANEL));
    }
    let ly = first + vis + 1;
    seg(fb, x0 + 2, ly, "SITUATION LOG", pw, v3(0.6, 0.85, 1.0), Some(PANEL));
    seg(fb, x0 + 2, ly + 1, &"─".repeat(pw as usize - 3), pw, v3(0.1, 0.2, 0.26), Some(PANEL));
    for (k, (when, msg, col)) in app.log.iter().enumerate().take((l.rows - 2 - ly - 2).max(0) as usize) {
        let r = ly + 2 + k as i32;
        let x = seg(fb, x0 + 2, r, when, 5, DIM * 0.8, Some(PANEL)) + 1;
        seg(fb, x, r, &trunc(msg, (x0 + pw - 1 - x) as usize), x0 + pw - 1 - x, *col, Some(PANEL));
    }
}

fn planet_color(p: &Planet) -> V3 {
    let (alb, emit, _) = terrain(p.terrain, 0.7, 0.1, p.seed);
    (alb + emit) * 1.1
}

// ─────────────────────────────── dialogs ───────────────────────────────

fn frame_box(fb: &mut Fb, x: i32, y: i32, w: i32, h: i32, title: &str, accent: V3) {
    fb.panel(x, y, w, h, BOX);
    for c in x + 1..x + w - 1 {
        fb.put_bg(c, y, '─', accent * 0.7, Some(BOX));
        fb.put_bg(c, y + h - 1, '─', accent * 0.7, Some(BOX));
    }
    for r in y + 1..y + h - 1 {
        fb.put_bg(x, r, '│', accent * 0.7, Some(BOX));
        fb.put_bg(x + w - 1, r, '│', accent * 0.7, Some(BOX));
    }
    for (c, r, ch) in [(x, y, '╭'), (x + w - 1, y, '╮'), (x, y + h - 1, '╰'), (x + w - 1, y + h - 1, '╯')] {
        fb.put_bg(c, r, ch, accent * 0.7, Some(BOX));
    }
    seg(fb, x + 2, y, &format!(" {title} "), w - 4, accent, Some(BOX));
}

fn centered(l: &Lay, w: i32, h: i32) -> (i32, i32, i32, i32) {
    let w = w.min(l.cols - 2);
    let h = h.min(l.rows - 2);
    ((l.cols - w) / 2, (l.rows - h) / 2, w, h)
}

fn species(fb: &mut Fb, app: &App, x: i32, y: i32, kind: usize) {
    let k = app.kinds.get(kind).map_or("?", String::as_str);
    let mut c = seg(fb, x, y, "species   ", 10, DIM, Some(BOX));
    c = seg(fb, c, y, "◀ ", 2, DIM, Some(BOX));
    c = seg(fb, c, y, k, 16, kind_color(k), Some(BOX));
    seg(fb, c, y, " ▶", 2, DIM, Some(BOX));
}

fn colonize(fb: &mut Fb, app: &App, c: &Colonize, l: &Lay, t: f32) {
    let (x, y, w, h) = centered(l, 72, 17);
    let accent = v3(0.45, 0.85, 1.0);
    frame_box(fb, x, y, w, h, "◆ COLONIZE A NEW WORLD", accent);
    seg(fb, x + 3, y + 2, "Pick a folder for the new colony, or type a new path to found one.", w - 6, DIM, Some(BOX));
    let row = y + 4;
    let x1 = seg(fb, x + 3, row, "› ", 2, accent, Some(BOX));
    let room = w - 8;
    let chars: Vec<char> = c.path.s.chars().collect();
    let start = c.path.cur.saturating_sub(room as usize - 1);
    for (k, ch) in chars.iter().enumerate().skip(start).take(room as usize) {
        fb.put_bg(x1 + (k - start) as i32, row, *ch, TEXT, Some(BOX));
    }
    if fract(t * 1.6) < 0.6 {
        let cc = x1 + (c.path.cur - start) as i32;
        let ch = chars.get(c.path.cur).copied().unwrap_or(' ');
        fb.put_bg(cc, row, ch, BOX, Some(accent));
    }
    let exists = std::path::Path::new(&expand_path(&c.path.s)).is_dir();
    if c.comps.is_empty() {
        let (m, col) = if exists { ("an established world — agents will join its orbit", v3(0.4, 0.9, 0.6)) } else { ("✦ uncharted space — this folder will be created", v3(1.0, 0.8, 0.4)) };
        seg(fb, x + 5, row + 2, m, w - 8, col, Some(BOX));
    }
    for (k, comp) in c.comps.iter().enumerate().take(7) {
        let hot = c.comp_i > 0 && (c.comp_i - 1) % c.comps.len() == k;
        seg(fb, x + 5, row + 2 + k as i32, &trunc(comp, (w - 8) as usize), w - 8, if hot { TEXT } else { DIM }, Some(if hot { SELBG } else { BOX }));
    }
    species(fb, app, x + 3, y + h - 5, c.kind);
    if c.mkdir {
        let m = format!("found new world at {}?  press ⏎ again", c.path.s.trim());
        seg(fb, x + 3, y + h - 3, &trunc(&m, (w - 6) as usize), w - 6, v3(1.0, 0.8, 0.4), Some(BOX));
    }
    seg(fb, x + 3, y + h - 2, "⏎ launch · ⇥ complete · ↑↓ species · esc cancel", w - 6, DIM, Some(BOX));
}

fn expand_path(p: &str) -> String {
    match p.trim().strip_prefix('~') {
        Some(rest) => format!("{}/{}", home().to_string_lossy(), rest.trim_start_matches('/')),
        None => p.trim().to_string(),
    }
}

fn deploy(fb: &mut Fb, app: &App, planet: &str, kind: usize, l: &Lay) {
    let (x, y, w, h) = centered(l, 60, 9);
    let accent = v3(0.45, 0.85, 1.0);
    frame_box(fb, x, y, w, h, &format!("◆ DEPLOY TO {}", base_name(planet).to_uppercase()), accent);
    seg(fb, x + 3, y + 2, &trunc(&tilde(planet), (w - 6) as usize), w - 6, DIM, Some(BOX));
    species(fb, app, x + 3, y + 4, kind);
    seg(fb, x + 3, y + h - 2, "⏎ deploy · ←→ species · esc cancel", w - 6, DIM, Some(BOX));
}

/// A text field with a blinking block cursor.
fn field(fb: &mut Fb, x: i32, row: i32, width: i32, text: &Line, accent: V3, t: f32) {
    let x1 = seg(fb, x, row, "› ", 2, accent, Some(BOX));
    let room = (width - 2).max(1) as usize;
    let chars: Vec<char> = text.s.chars().collect();
    let start = text.cur.saturating_sub(room - 1);
    for (k, ch) in chars.iter().enumerate().skip(start).take(room) {
        fb.put_bg(x1 + (k - start) as i32, row, *ch, TEXT, Some(BOX));
    }
    if fract(t * 1.6) < 0.6 {
        let ch = chars.get(text.cur).copied().unwrap_or(' ');
        fb.put_bg(x1 + (text.cur - start) as i32, row, ch, BOX, Some(accent));
    }
}

fn fleet_order(fb: &mut Fb, app: &App, w: &World, target: &Sel, text: &Line, l: &Lay, t: f32) {
    let (x, y, bw, h) = centered(l, 72, 11);
    let accent = v3(0.45, 0.85, 1.0);
    let name = match target {
        Sel::Sys(k) => format!("{} SYSTEM", base_name(k).to_uppercase()),
        Sel::Planet(k) => base_name(k).to_uppercase(),
        Sel::Agent(_) => String::new(),
    };
    frame_box(fb, x, y, bw, h, &format!("◆ FLEET ORDER · {name}"), accent);
    let ships = app.fleet(w, target);
    let free = ships.iter().filter(|a| a.status != Status::Blocked).count();
    let mut note = format!("Every ship here gets this order: {free} of {}", ships.len());
    if free < ships.len() {
        note.push_str(" (the rest are waiting on your answer)");
    }
    seg(fb, x + 3, y + 2, &note, bw - 6, DIM, Some(BOX));
    field(fb, x + 3, y + 4, bw - 6, text, accent, t);
    let mut cx = x + 3;
    for a in ships.iter().take(8) {
        cx = seg(fb, cx, y + 6, &format!("{} {} ", a.status.glyph(t), a.kind), bw - 6 - (cx - x - 3), kind_color(&a.kind), Some(BOX)) + 1;
    }
    seg(fb, x + 3, y + h - 2, "⏎ send to the fleet · esc cancel", bw - 6, DIM, Some(BOX));
}

/// Quick actions: what the world needs, one key each.
fn actions(fb: &mut Fb, app: &App, planet: &str, sel: usize, l: &Lay) {
    let needs = app.needs(planet);
    let acts: Vec<&Need> = needs.iter().filter(|n| n.act.is_some()).collect();
    let info: Vec<&Need> = needs.iter().filter(|n| n.act.is_none()).collect();
    let (x, y, w, h) = centered(l, 76, (acts.len() + info.len()) as i32 + 7);
    let accent = v3(1.0, 0.75, 0.3);
    frame_box(fb, x, y, w, h, &format!("◆ {} NEEDS", base_name(planet).to_uppercase()), accent);
    seg(fb, x + 3, y + 2, "Each order goes to a free agent here, or a new ship if there's no one.", w - 6, DIM, Some(BOX));
    for (i, n) in acts.iter().enumerate() {
        let r = y + 4 + i as i32;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 2, r, w - 4, 1, bg);
        let what = match &n.act {
            Some(Act::Comms(_)) => "open comms",
            Some(Act::Priority(_)) => "give priority",
            _ => "send orders",
        };
        let c = seg(fb, x + 3, r, &format!(" {} ", i + 1), 3, BOX, Some(n.col * 0.9));
        let c = seg(fb, c + 1, r, &clip(&n.label, (w - 26) as usize), w - 26, TEXT, Some(bg));
        let _ = c;
        seg(fb, x + w - 3 - what.len() as i32, r, what, 16, DIM, Some(bg));
    }
    for (i, n) in info.iter().enumerate() {
        seg(fb, x + 6, y + 4 + (acts.len() + i) as i32, &clip(&n.label, (w - 10) as usize), w - 10, n.col * 0.8, Some(BOX));
    }
    seg(fb, x + 3, y + h - 2, "1-9 / ⏎ act · ↑↓ choose · esc close", w - 6, DIM, Some(BOX));
}

/// The mission log, grouped by status, world, system or fleet order; open missions show in full.
fn mission_log(fb: &mut Fb, app: &App, sel: usize, open: &HashSet<u32>, l: &Lay, t: f32) {
    let log = app.mission_log();
    let accent = v3(1.0, 0.85, 0.4);
    let w = 96.min(l.cols - 4);
    // lay the log out as lines first, so expanded missions scroll like the rest
    enum Ln {
        Head(String),
        Item(usize),
        More(usize, String, V3),
    }
    let now = survey::now() as u64;
    let mut lines = Vec::new();
    let mut last = "";
    for (i, (head, m)) in log.iter().enumerate() {
        if head != last {
            lines.push(Ln::Head(head.clone()));
            last = head;
        }
        lines.push(Ln::Item(i));
        if open.contains(&m.id) {
            for s in wrap(&m.text, (w - 12) as usize) {
                lines.push(Ln::More(i, s, TEXT * 0.85));
            }
            for image in &m.attachments {
                lines.push(Ln::More(i, format!("▧ {}", clip(&image.name, (w - 14).max(1) as usize)), DIM));
            }
            let mut info = vec![tilde(&m.cwd)];
            if let Some(f) = m.fleet {
                info.push(format!("fleet order #{f}"));
            }
            match m.agent.and_then(|id| app.agent(id)) {
                Some(a) => info.push(format!("{} #{}", a.kind, a.id)),
                None if m.agent.is_some() => info.push("agent recalled".into()),
                None => {}
            }
            info.push(format!("written {} ago", ago(now.saturating_sub(m.at) as f32)));
            if let Some(d) = m.done_at {
                info.push(format!("done {} ago", ago(now.saturating_sub(d) as f32)));
            }
            lines.push(Ln::More(i, info.join(" · "), DIM * 0.8));
        }
    }
    let (x, y, w, h) = centered(l, w, (l.rows - 6).min(lines.len() as i32 + 6).max(12));
    let under = log.iter().filter(|(_, m)| matches!(m.state.as_str(), "sent" | "active")).count();
    let queued = log.iter().filter(|(_, m)| m.state == "queued").count();
    let title = format!("★ MISSION LOG · {under} under way · {queued} queued · {} done · by {}", log.len() - under - queued, app.log_sort.label());
    frame_box(fb, x, y, w, h, &title, accent);
    if log.is_empty() {
        seg(fb, x + 3, y + 2, "No missions yet. Select a world and press t to give it one, or r for what it needs.", w - 6, DIM, Some(BOX));
    }
    // keep the whole selected mission in view, and its heading when there's room
    let room = (h - 5).max(1) as usize;
    let of = |ln: &Ln| match ln {
        Ln::Item(i) | Ln::More(i, ..) => Some(*i),
        Ln::Head(_) => None,
    };
    let first = lines.iter().position(|ln| of(ln) == Some(sel)).unwrap_or(0);
    let last = lines.iter().rposition(|ln| of(ln) == Some(sel)).unwrap_or(0);
    let off = (last + 1).saturating_sub(room).min(first.saturating_sub(1));
    // a queued mission's place in its world's line
    let mut queue_pos: HashMap<u32, usize> = HashMap::new();
    let mut by_world: HashMap<&str, usize> = HashMap::new();
    let mut queue: Vec<&crate::proto::Mission> = log.iter().map(|(_, m)| *m).filter(|m| m.state == "queued").collect();
    queue.sort_by_key(|m| m.id);
    for m in queue {
        let n = by_world.entry(m.cwd.as_str()).or_insert(0);
        *n += 1;
        queue_pos.insert(m.id, *n);
    }
    for (k, ln) in lines.iter().skip(off).take(room).enumerate() {
        let r = y + 2 + k as i32;
        let bg = if of(ln) == Some(sel) { SELBG } else { BOX };
        match ln {
            Ln::Head(s) => {
                seg(fb, x + 3, r, s, w - 6, accent * 0.8, Some(BOX));
            }
            Ln::More(_, s, col) => {
                fb.panel(x + 2, r, w - 4, 1, bg);
                seg(fb, x + 7, r, s, w - 10, *col, Some(bg));
            }
            Ln::Item(i) => {
                let m = log[*i].1;
                fb.panel(x + 2, r, w - 4, 1, bg);
                let (g, col) = match m.state.as_str() {
                    "sent" | "active" => ("◆", v3(0.4, 0.85, 1.0) * (0.7 + 0.3 * (t * 3.0).sin().abs())),
                    "queued" => ("○", v3(1.0, 0.85, 0.4)),
                    _ => ("★", v3(1.0, 0.85, 0.4) * 0.8),
                };
                let fold = if open.contains(&m.id) { "▾" } else { "▸" };
                let mut c = seg(fb, x + 3, r, &format!("{fold} {g} "), 4, col, Some(bg));
                c = seg(fb, c, r, &pad(&clip(&base_name(&m.cwd), 14), 15), 15, TEXT, Some(bg));
                let who = match (m.state.as_str(), m.agent.and_then(|id| app.agent(id))) {
                    ("queued", _) => format!("#{} in line", queue_pos.get(&m.id).unwrap_or(&0)),
                    (_, Some(a)) => a.kind.clone(),
                    _ => "recalled".into(),
                };
                let when = match m.state.as_str() {
                    "done" => ago(now.saturating_sub(m.done_at.unwrap_or(m.at)) as f32),
                    _ => ago(now.saturating_sub(m.at) as f32),
                };
                let right = format!("{who} · {when}");
                let tw = (w - 8 - (c - x) - right.chars().count() as i32).max(4);
                // an open mission's text is spelled out below
                let text = if open.contains(&m.id) { String::new() } else { clip(&m.text, tw as usize) };
                seg(fb, c, r, &text, tw, if m.state == "done" { DIM } else { TEXT }, Some(bg));
                seg(fb, x + w - 3 - right.chars().count() as i32, r, &right, 30, DIM, Some(bg));
            }
        }
    }
    seg(fb, x + 3, y + h - 2, "␣ expand · e all · s group by status/world/system/fleet · ⏎ comms · x call off / strike · esc close", w - 6, DIM, Some(BOX));
}

/// Word-wrap `s` to `w` columns, keeping its own line breaks.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let n = line.chars().count();
            if n > 0 && n + 1 + word.chars().count() > w {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            // a word longer than the line is hard-broken
            while line.chars().count() > w {
                out.push(line.chars().take(w).collect());
                line = line.chars().skip(w).collect();
            }
        }
        out.push(line);
    }
    out
}

fn moon(fb: &mut Fb, app: &App, planet: &str, branch: &Line, kind: usize, l: &Lay, t: f32) {
    let (x, y, w, h) = centered(l, 72, 12);
    let accent = v3(0.75, 0.8, 0.9);
    frame_box(fb, x, y, w, h, &format!("☾ NEW MOON FOR {}", base_name(planet).to_uppercase()), accent);
    seg(fb, x + 3, y + 2, "A moon is a git worktree on its own branch, with its own agent:", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + 3, "parallel work that can't step on the world's toes. Branch name:", w - 6, DIM, Some(BOX));
    field(fb, x + 3, y + 5, w - 6, branch, accent, t);
    species(fb, app, x + 3, y + 7, kind);
    seg(fb, x + 3, y + h - 2, "⏎ found it · ↑↓ species · esc cancel", w - 6, DIM, Some(BOX));
}

fn dock(fb: &mut Fb, app: &App, moon: &str, l: &Lay) {
    let (x, y, w, h) = centered(l, 66, 10);
    let accent = v3(0.6, 0.85, 1.0);
    let s = app.survey.worlds.get(moon);
    let branch = s.map_or(String::new(), |s| s.branch.clone());
    let host = s.and_then(|s| s.host.clone()).map(|h| base_name(&h)).unwrap_or_default();
    frame_box(fb, x, y, w, h, &format!("⊕ DOCK MOON {}", branch.to_uppercase()), accent);
    seg(fb, x + 3, y + 2, &format!("Merge branch {branch} into {host}, then recall the moon's"), w - 6, TEXT, Some(BOX));
    seg(fb, x + 3, y + 3, "agents and remove its worktree.", w - 6, TEXT, Some(BOX));
    seg(fb, x + 3, y + 5, "Both need to be committed. A merge conflict is a collision:", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + 6, "the merge is called off and nothing changes.", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 2, "y dock · any other key cancels", w - 6, accent, Some(BOX));
}

/// Progress of the latest fleet order among these agents.
fn fleet_row(rows: &mut Rows, app: &App, agents: &[&Agent]) {
    let ids: Vec<u32> = agents.iter().map(|a| a.id).collect();
    let Some(f) = app.missions.iter().filter(|m| m.agent.is_some_and(|a| ids.contains(&a))).filter_map(|m| m.fleet).max() else { return };
    let all: Vec<&crate::proto::Mission> = app.missions.iter().filter(|m| m.fleet == Some(f)).collect();
    let done = all.iter().filter(|m| m.state == "done").count();
    let col = if done == all.len() { v3(1.0, 0.85, 0.4) } else { v3(0.4, 0.85, 1.0) };
    rows.push(("fleet order", vec![(format!("{} {done}/{} ", meter(done as f32 / all.len() as f32, 5), all.len()), col), (clip(&all[0].text, 30), DIM)]));
}

#[allow(clippy::too_many_arguments)]
fn brief(fb: &mut Fb, app: &App, planet: &str, text: &Line, kind: usize, launch: bool, attachments: &[crate::screenshots::Attachment], sending: bool, l: &Lay, t: f32) {
    let visible = attachments.len().min((l.rows - 17).max(1) as usize);
    let (x, y, w, h) = centered(l, 80, 15 + visible as i32);
    let accent = v3(1.0, 0.85, 0.4);
    frame_box(fb, x, y, w, h, &format!("★ NEW MISSION FOR {}", base_name(planet).to_uppercase()), accent);
    seg(fb, x + 3, y + 2, "What should be done here?", w - 6, DIM, Some(BOX));
    let row = y + 4;
    let x1 = seg(fb, x + 3, row, "› ", 2, accent, Some(BOX));
    let room = (w - 8) as usize;
    let chars: Vec<char> = text.s.chars().collect();
    let start = text.cur.saturating_sub(room - 1);
    for (k, ch) in chars.iter().enumerate().skip(start).take(room) {
        fb.put_bg(x1 + (k - start) as i32, row, *ch, TEXT, Some(BOX));
    }
    if fract(t * 1.6) < 0.6 {
        let ch = chars.get(text.cur).copied().unwrap_or(' ');
        fb.put_bg(x1 + (text.cur - start) as i32, row, ch, BOX, Some(accent));
    }
    let crew = app.agents.iter().filter(|a| a.cwd == planet && a.dying.is_none()).count();
    let (now, later) = if launch { (TEXT, DIM) } else { (DIM, TEXT) };
    for (i, image) in attachments.iter().skip(attachments.len() - visible).enumerate() {
        let prefix = if i == 0 && visible < attachments.len() { format!("▧ {} total · ", attachments.len()) } else { "▧ ".into() };
        seg(fb, x + 3, y + 6 + i as i32, &clip(&format!("{prefix}{}", image.name), (w - 7).max(1) as usize), w - 6, TEXT * 0.85, Some(BOX));
    }
    let r = y + 7 + visible as i32;
    let xx = seg(fb, x + 3, r, if launch { "● " } else { "○ " }, 2, accent, Some(BOX));
    let xx = seg(fb, xx, r, "launch a new ship with it now", w - 6, now, Some(BOX));
    if launch {
        species(fb, app, xx + 2, r, kind);
    }
    let xx = seg(fb, x + 3, r + 1, if launch { "○ " } else { "● " }, 2, accent, Some(BOX));
    let note = if crew == 0 { "queue it for the next agent here (none yet)".to_string() } else { format!("queue it: the first free one of the {crew} here takes it (b gives all of them an order)") };
    seg(fb, xx, r + 1, &note, w - 8, later, Some(BOX));
    seg(fb, x + 3, y + h - 4, "Drop image · Ctrl+V clipboard · Ctrl+O attach path · Ctrl+X remove last", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 2, if sending { "Sending mission…" } else { "⏎ give orders · ⇥ launch/queue · ↑↓ species · esc cancel" }, w - 6, DIM, Some(BOX));
}

fn confirm(fb: &mut Fb, app: &App, id: u32, l: &Lay) {
    let (x, y, w, h) = centered(l, 60, 8);
    let accent = v3(1.0, 0.4, 0.3);
    frame_box(fb, x, y, w, h, "✖ DECOMMISSION", accent);
    let who = app.agent(id).map_or("this agent".into(), |a| format!("{} on {}", a.kind, base_name(&a.cwd)));
    seg(fb, x + 3, y + 2, &format!("Decommission {who}?"), w - 6, TEXT, Some(BOX));
    seg(fb, x + 3, y + 3, "Its process is terminated; unsaved work in it is lost.", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 2, "y decommission · any other key cancels", w - 6, accent, Some(BOX));
}

fn help(fb: &mut Fb, l: &Lay) {
    let lines = [
        ("GALAXY", ""),
        ("↑↓ / j k", "select in the outliner"),
        ("⏎ / click twice", "open comms (live terminal with the agent)"),
        ("⇥", "next agent that needs you"),
        ("n", "colonize: launch an agent in a folder (new or existing)"),
        ("e", "deploy another agent to the selected world"),
        ("x", "decommission the selected agent"),
        ("r", "what the selected world needs, with quick actions"),
        ("t / L", "give a world a mission / the mission log"),
        ("b", "fleet order: one order to every agent in a system or world"),
        ("u / D", "found a moon (git worktree) / dock it back"),
        ("P", "give an agent priority in a border conflict"),
        ("X", "forget a dormant world"),
        ("1-9", "answer the incoming transmission"),
        ("wasd / drag", "pan        ± / wheel  zoom"),
        ("z / f", "fit galaxy / focus selection"),
        ("o", "toggle outliner        m  mute"),
        ("q", "leave (agents keep running in the daemon)"),
        ("", ""),
        ("COMMS", ""),
        ("any key", "goes straight to the agent"),
        ("Ctrl-] / F12", "back to the galaxy"),
    ];
    let (x, y, w, h) = centered(l, 66, lines.len() as i32 + 4);
    let accent = v3(0.45, 0.85, 1.0);
    frame_box(fb, x, y, w, h, "◆ FLEET MANUAL", accent);
    for (k, (key, what)) in lines.iter().enumerate() {
        let r = y + 2 + k as i32;
        if what.is_empty() {
            seg(fb, x + 3, r, key, w - 6, accent, Some(BOX));
        } else {
            let c = seg(fb, x + 3, r, &pad(key, 17), 17, TEXT, Some(BOX));
            seg(fb, c + 1, r, what, w - 22, DIM, Some(BOX));
        }
    }
}

// ──────────────────────────────── comms ────────────────────────────────

fn comms(fb: &mut Fb, app: &App, l: &Lay, t: f32, id: u32) {
    let a = app.agents.iter().find(|a| a.id == id);
    let accent = a.map_or(DIM, |a| a.status.color());
    let title = match a {
        Some(a) => format!("◆ COMMS ▸ {} @ {} · {}", a.kind, base_name(&a.cwd), a.status.label()),
        None => "◆ COMMS".into(),
    };
    frame_box(fb, 0, 1, l.cols, l.rows - 1, &title, accent);
    let hint = " Ctrl-] galaxy ";
    seg(fb, l.cols - hint.chars().count() as i32 - 2, 1, hint, l.cols, DIM, Some(BOX));
    let (iw, ih) = (l.cols - 2, l.rows - 3);
    fb.panel(1, 2, iw, ih, TERM_BG);
    let Some((_, s)) = &app.screen else {
        let m = "establishing uplink…";
        seg(fb, (l.cols - m.chars().count() as i32) / 2, l.rows / 2, m, l.cols, accent * (0.6 + 0.4 * (t * 4.0).sin().abs()), Some(TERM_BG));
        return;
    };
    let c3 = |c: [u8; 3]| v3(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    for (r, line) in s.lines.iter().enumerate().take(ih as usize) {
        let mut col = 0;
        for run in line {
            let fg = run.fg.map_or(TERM_FG, c3);
            let bg = run.bg.map_or(TERM_BG, c3);
            for ch in run.t.chars() {
                if col < iw {
                    let (fg, bg) = if app.pick.is_some_and(|p| p.has(r as i32, col)) { (TERM_BG, v3(0.55, 0.8, 1.0)) } else { (fg, bg) };
                    fb.put_bg(1 + col, 2 + r as i32, ch, fg, Some(bg));
                }
                col += 1;
            }
        }
    }
    if s.scroll > 0 {
        let m = format!(" ↑ {} lines back · wheel down or type to return ", s.scroll);
        seg(fb, (l.cols - m.chars().count() as i32) / 2, l.rows - 1, &m, l.cols, accent, Some(BOX));
    }
    if !s.hide_cursor && fract(t * 1.6) < 0.6 {
        let (cr, cc) = (s.cursor.0 as i32, s.cursor.1 as i32);
        if cr < ih && cc < iw {
            let i = (2 + cr) as usize * fb.cols + (1 + cc) as usize;
            let (ch, fg, bg) = fb.txt[i].map_or((' ', TERM_FG, TERM_BG), |c| (c.ch, c.fg, c.bg.unwrap_or(TERM_BG)));
            let ch = if ch == '\0' { ' ' } else { ch };
            fb.put_bg(1 + cc, 2 + cr, ch, bg, Some(fg));
        }
    }
}
