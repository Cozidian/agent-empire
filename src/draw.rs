//! Rendering: the galaxy (pixels), and the HUD, outliner, dialogs and comms
//! terminal (text cells).

use crate::game::*;
use crate::role::Role;
use crate::gfx::*;
use crate::sound::Sound;
use crate::structure::KINDS;
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

/// A repeatable visual bench: no workspace, agents, files or daemon.
pub fn graphics_preview(fb: &mut Fb, polished: bool, t: f32) {
    fb.clear_text();
    for y in 0..fb.h { for x in 0..fb.w {
        let st = rnd2(x as i32, y as i32, 11);
        let mut c = v3(0.006, 0.008, 0.02);
        if st > 0.9965 { c += hsv(rnd2(x as i32, y as i32, 5) * 0.3 + 0.5, 0.3, 0.15 + (st - 0.9965) / 0.0035 * 0.5); }
        fb.px[y * fb.w + x] = c;
    } }
    let (w, h) = (fb.w as f32, fb.h as f32);
    let radius = (w / 10.0).min(h * 0.19);
    let cy = h * 0.43;
    for (i, (kind, seed, name)) in [(0, 0.218, "PELAGIC / TERRAN"), (4, 0.731, "VESPER / GAS GIANT"), (3, 0.467, "BOREAL / ICE")].into_iter().enumerate() {
        let x = w * (i as f32 + 0.5) / 3.0;
        let p = Planet { key: String::new(), name: name.into(), agents: vec![], terrain: kind, seed, wpos: (x, cy), r: radius,
            host: None, moons: vec![], level: 2, dormant: false, fog: 0.0, last_seen: 0 };
        sphere_style(fb, x, cy, radius, &p, (-1.0, -0.5), t, polished, 5);
        let row = ((cy + radius) / 4.0) as i32 + 2;
        let col = x as i32 / 2 - name.len() as i32 / 2;
        if fb.cols >= 90 { fb.text(col, row, name, DIM); }
        // the same materials at map scale, where silhouette matters more than detail
        sphere_style(fb, x - 10.0, h * 0.82, 4.0, &p, (-1.0, -0.5), t, polished, 5);
        sphere_style(fb, x + 10.0, h * 0.82, 8.0, &p, (-1.0, -0.5), t, polished, 5);
    }
    if fb.cols >= 90 {
        star(fb, (w / 3.0, h * 0.82), 3.2, 31, v3(1.0, 0.72, 0.42), t);
        hull(fb, (w * 2.0 / 3.0, h * 0.82), 1.5, 59, Role::None, v3(0.4, 0.85, 0.95));
    }
    bloom(fb, 0.5, 0.7);
    let label = if polished { "GRAPHICS LAB / POLISHED" } else { "GRAPHICS LAB / CLASSIC" };
    fb.text(3, 2, label, TEXT);
    if fb.cols >= 90 { fb.text(3, 4, "Same worlds. Same light. A quieter, deeper surface.", DIM); }
    fb.text(3, fb.rows as i32 - 2, if fb.cols >= 70 { "Space compare  /  p pause  /  q leave     ·     Offline preview" } else { "Space compare · p pause · q leave" }, DIM);
}

pub fn frame(fb: &mut Fb, app: &mut App, w: &World, l: &Lay, t: f32, sound: Option<&Sound>) {
    fb.clear_text();
    if matches!(app.mode, Mode::Research) { crate::research::draw(fb, &mut app.research, t); return; }
    if let Mode::Workspace { key, sel } = &app.mode { workspace(fb, app, w, key, *sel, l, t); return; }
    if let Mode::Folders { sel } = app.mode { folders(fb, app, sel, l, t); return; }
    if let Mode::Planet(v) = &app.mode {
        planet_view(fb, app, w, v, l, t);
        return;
    }
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
    research_station(fb, app, w, l, t);
    bloom(fb, 0.5, 0.7);
    labels(fb, app, w, l, t);
    top_bar(fb, app, w, l, t);
    if l.pw > 0 {
        outliner(fb, app, w, l, t);
    }
    if matches!(app.mode, Mode::Galaxy) {
        card(fb, app, w, l, t);
        transmission(fb, app, l, t);
        sector_banner(fb, app, l);
    }
    footer(fb, app, w, l, sound);
    if let Some((m, t0)) = &app.toast {
        let a = 1.0 - smoothstep(2.6, 3.2, t - t0);
        if a > 0.0 {
            let cw = l.cols - l.pw;
            let s = format!(" {m} ");
            fb.text((cw - s.chars().count() as i32).max(0) / 2, l.rows - 3, &s, TEXT * a);
        }
    }
    match &app.mode {
        Mode::Root(text) => {
            let (x, y, width, height) = centered(l, 76, 9);
            frame_box(fb, x, y, width, height, "ADD SECTOR ROOT", v3(0.5, 0.85, 1.0));
            seg(fb, x + 3, y + 2, "Folder to scan for Git projects, e.g. ~/work", width - 6, DIM, Some(BOX));
            field(fb, x + 3, y + 4, width - 6, text, TEXT, t);
            seg(fb, x + 3, y + 7, "Enter add · Tab complete · Esc cancel", width - 6, DIM, Some(BOX));
        }
        Mode::Commander => commander(fb, app, w, l),
        Mode::Anomalies { id, typing } => anomalies(fb, app, l, *id, *typing),
        Mode::Trade { sel } => trade_overview(fb, app, w, l, *sel, t),
        Mode::Situation { id, typing } => situation(fb, app, w, l, *id, *typing, t),
        Mode::Colonize(c) => colonize(fb, app, c, l, t),
        Mode::Deploy { planet, kind } => deploy(fb, app, planet, *kind, l),
        Mode::Confirm { id } => confirm(fb, app, *id, l),
        Mode::Ship { id, sel } => ship_card(fb, app, *id, *sel, l, t),
        Mode::Help => help(fb, l),
        Mode::Brief { planet, text, kind, launch, attachments, sending } => brief(fb, app, planet, text, *kind, *launch, attachments, *sending, l, t),
        Mode::Actions { planet, sel } => actions(fb, app, planet, *sel, l),
        Mode::Build { planet, sel } => build_menu(fb, app, planet, *sel, l),
        Mode::Log { world, sel, open } => mission_log(fb, app, world, *sel, open, l, t),
        _ => {}
    }
    if !app.online {
        let m = format!(" ○ daemon offline · {} · reconnecting… ", app.err);
        fb.text(2, 2, &m, v3(1.0, 0.4, 0.3));
    } else if app.loaded && w.planets.is_empty() && matches!(app.mode, Mode::Galaxy) {
        let cw = l.cols - l.pw;
        let lines = ["THE GALAXY IS UNCHARTED", "", "press  n  to colonize your first world"];
        for (i, s) in lines.iter().enumerate() {
            let c = if i == 0 { v3(0.6, 0.85, 1.0) } else { DIM };
            fb.text((cw - s.chars().count() as i32) / 2, l.rows / 2 - 1 + i as i32, s, c);
        }
    }
}

// ─────────────────────────────── galaxy ────────────────────────────────

fn research_station(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let (x, y) = l.to_screen(&app.cam, research_pos(w));
    if x < 15.0 || x > l.vw - 30.0 || y < l.vy0 + 8.0 || y > l.vy0 + l.vh - 20.0 { return; }
    let col = v3(0.43, 0.88, 0.79);
    ellipse(fb, x, y, 11.0, 5.0, col * 0.5);
    ellipse(fb, x, y, 5.0, 11.0, col * 0.5);
    orb(fb, x, y, 2.0, col);
    for i in 0..3 { let a = t * 0.22 + i as f32 * TAU / 3.0; orb(fb, x + a.cos() * 11.0, y + a.sin() * 5.0, 1.3, v3(0.7, 0.58, 0.95)); }
    if app.sel == Some(Sel::Research) { circle(fb, x, y, 15.0, col * 0.7); }
    fb.text(x as i32 / 2 - 6, y as i32 / 4 + 4, "RESEARCH", col);
}

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
fn sphere(fb: &mut Fb, app: &App, cx: f32, cy: f32, r: f32, p: &Planet, light: (f32, f32), t: f32) {
    let buildings = app.survey.worlds.get(&p.key).map_or(0, |g| g.structures.len());
    sphere_style(fb, cx, cy, r, p, light, t, true, buildings);
}

fn sphere_style(fb: &mut Fb, cx: f32, cy: f32, r: f32, p: &Planet, light: (f32, f32), t: f32, polished: bool, buildings: usize) {
    let l = {
        let v = v3(light.0, light.1, 0.55);
        v / (v.x * v.x + v.y * v.y + v.z * v.z).sqrt()
    };
    let air = (r * 0.055).clamp(0.8, 2.8);
    // a little fill keeps tiny worlds legible without flattening the portraits
    let fill = 1.0 - smoothstep(5.0, 14.0, r);
    let e = r + if polished { air * 5.0 } else { 4.0 };
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
                fb.px[i] += if polished {
                    atm * (-((d - 1.0) * r / air).powf(1.25)).exp() * 0.24 * lit * (1.0 - 0.7 * p.fog)
                } else { atm * (-(d - 1.0) * r * 0.9).exp() * 0.4 * lit };
                continue;
            }
            let nz = (1.0 - d2).sqrt();
            let latitude = dx * st + dy * ct;
            let lon = (dx * ct - dy * st).atan2(nz) + p.seed * TAU + t * 0.04 * (0.5 + p.seed);
            let (alb, emit, atm) = terrain(p.terrain, lon, latitude, p.seed);
            let ndl = dx * l.x + dy * l.y + nz * l.z;
            let day = smoothstep(-0.12, 0.5, ndl);
            let mut c = alb * (0.035 + 1.05 * day) + emit * (1.0 - 0.6 * day) + atm * (1.0 - nz).powi(3) * 0.9 * day;
            if polished {
                let diffuse = mixf(ndl.max(0.0), day, fill * 0.22);
                let rim = (1.0 - nz).powi(3);
                c = alb * (0.025 + fill * 0.055 + 1.12 * diffuse) + emit * (1.0 - 0.6 * day);
                c += atm * (0.025 + rim * 0.42) * smoothstep(-0.15, 0.65, ndl);
                c += v3(0.32, 0.12, 0.055) * (-((ndl - 0.025) * 12.0).powi(2)).exp() * rim;
                if p.terrain <= 1 {
                    // the half vector gives water a small, quiet glint
                    let h = v3(l.x, l.y, l.z + 1.0);
                    let nh = (dx * h.x + dy * h.y + nz * h.z) / (h.x * h.x + h.y * h.y + h.z * h.z).sqrt();
                    let sea = smoothstep(0.0, 0.18, alb.z - alb.y);
                    c += v3(0.7, 0.82, 0.9) * nh.max(0.0).powi(48) * sea * diffuse * 0.5;
                    // broad cloud banks drift independently of the surface
                    let u = lon + t * 0.006;
                    let cloud = fbm(u.cos() * 4.0 + 23.0, u.sin() * 4.0 + latitude * 5.0, p.seed.to_bits() ^ 211);
                    let cloud = smoothstep(0.51, 0.72, cloud) * 0.72;
                    c = mix(c * (1.0 - cloud * 0.18), s3(0.06 + fill * 0.05 + diffuse * 0.94), cloud);
                }
            }
            // a neglected world goes grey and dim under the fog
            let grey = s3((c.x + c.y + c.z) / 3.0);
            let c = mix(c, mix(grey, v3(0.35, 0.3, 0.5) * grey.x * 2.0, 0.5), p.fog) * (1.0 - 0.7 * p.fog);
            fb.px[i] = if polished { mix(fb.px[i], c, smoothstep(-0.5, 0.6, (1.0 - d2.sqrt()) * r)) } else { c };
        }
    }
    city_lights(fb, p, (cx, cy), r, l, t, buildings);
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

/// A git worktree: a small modular station with a core, a truss and two solar wings, lit amber while it has a crew.
/// Not a ring like the GitHub station, and never a sphere.
fn worktree_station(fb: &mut Fb, x: f32, y: f32, r: f32, p: &Planet, t: f32) {
    let hull = v3(0.62, 0.68, 0.74);
    let panel = v3(0.2, 0.45, 0.85);
    let crewed = !p.dormant;
    let lamp = if crewed { v3(1.0, 0.78, 0.4) } else { v3(0.28, 0.32, 0.38) };
    if crewed { circle(fb, x, y, r * 1.4, lamp * 0.04); }
    // truss, then two solar panels on each side
    fb.line(x - r * 1.35, y, x + r * 1.35, y, hull * 0.55, hull * 0.55);
    for side in [-1.0, 1.0] {
        for k in 0..2 {
            let px = x + side * r * (0.8 + 0.45 * k as f32);
            let h = r * 0.6;
            fb.line(px, y - h, px, y + h, panel * 0.45, panel * 0.85);
            fb.splat(px + side * 0.7, y, panel * 0.3);
        }
        fb.splat(x + side * r * 1.35, y, lamp * 0.9);
    }
    // the core module, an antenna and a slow beacon
    orb(fb, x, y, (r * 0.38).max(1.4), hull);
    fb.line(x, y - r * 0.4, x, y - r * 0.95, hull * 0.5, hull * 0.5);
    let beacon = if crewed { 0.45 + 0.55 * (t * 2.0 + p.seed * TAU).sin().abs() } else { 0.25 };
    fb.splat(x, y - r * 0.95, if crewed { v3(1.0, 0.45, 0.35) * beacon } else { lamp * beacon });
    fb.splat(x - 0.6, y + 0.4, lamp);
    fb.splat(x + 0.6, y + 0.4, lamp);
}

/// Small settlements on a world's night side, carried with its rotating surface.
fn city_lights(fb: &mut Fb, p: &Planet, (cx, cy): (f32, f32), r: f32, light: V3, t: f32, buildings: usize) {
    if buildings == 0 || r <= 5.0 || p.fog > 0.6 { return; }
    let seed = p.seed.to_bits();
    let (st, ct) = ((rnd(seed ^ 83) - 0.5) * 1.4).sin_cos();
    let phase = p.seed * TAU + t * 0.04 * (0.5 + p.seed);
    let zoom = smoothstep(5.0, 11.0, r);
    // fixed surface sites: adding resources fills settlements without moving existing lights
    let mut lights: HashMap<usize, f32> = HashMap::new();
    for i in 0..(18 + buildings.min(8)) {
        let s = hash(seed ^ (i as u32).wrapping_mul(7919) ^ 401);
        let lon = rnd(seed ^ 397) * TAU + i as f32 * 2.399963;
        let lat = (rnd(s ^ 7) - 0.5) * 1.5;
        for j in 0..(4 + buildings.min(8) / 2 + p.level.min(3) as usize) {
            let k = hash(s ^ (j as u32).wrapping_mul(3571) ^ 409);
            let u = lon + (rnd(k ^ 11) - 0.5) * 0.24 - phase;
            let v = lat + (rnd(k ^ 13) - 0.5) * 0.16;
            let ring = (1.0 - v * v).sqrt();
            let (x, z) = (u.sin() * ring, u.cos() * ring);
            if z <= 0.0 { continue; }
            let (dx, dy) = (x * ct + v * st, -x * st + v * ct);
            let (px, py) = ((cx + dx * r).floor() as i32, (cy + dy * r).floor() as i32);
            let Some(idx) = fb.idx(px, py) else { continue };
            // evaluate the actual pixel so even a tiny world keeps a clean silhouette
            let (nx, ny) = ((px as f32 + 0.5 - cx) / r, (py as f32 + 0.5 - cy) / r);
            let d2 = nx * nx + ny * ny;
            if d2 >= 1.0 { continue; }
            let nz = (1.0 - d2).sqrt();
            let day = nx * light.x + ny * light.y + nz * light.z;
            let glow = smoothstep(0.32, -0.08, day) * smoothstep(0.04, 0.2, nz) * zoom * (0.65 + rnd(k ^ 17) * 0.35);
            // overlapping windows stay quiet, particularly at map scale
            lights.entry(idx).and_modify(|v| *v = v.max(glow)).or_insert(glow);
        }
    }
    for (i, glow) in lights {
        let c = v3(1.0, 0.72, 0.35) * glow;
        let grey = s3((c.x + c.y + c.z) / 3.0);
        fb.px[i] += mix(c, mix(grey, v3(0.35, 0.3, 0.5) * grey.x * 2.0, 0.5), p.fog) * (1.0 - 0.7 * p.fog);
    }
}

/// Seeded silhouettes, kept inside the existing ship selection radius; a role has its own.
fn hull(fb: &mut Fb, (x, y): (f32, f32), s: f32, seed: u32, role: Role, col: V3) {
    let angle = rnd(seed ^ 101) * TAU;
    let (sn, cs) = angle.sin_cos();
    let width = 0.8 + rnd(seed ^ 103) * 0.35;
    let point = |(u, v): (f32, f32)| {
        (x + (u * cs - v * width * sn) * s, y + (u * sn + v * width * cs) * s)
    };
    let style = match role {
        Role::None => crate::gfx::hash(seed ^ 107) % 4,
        Role::Scout => 1,
        Role::Builder => 2,
        Role::Inspector => 3,
    };
    let vertices: &[(f32, f32)] = match style {
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
    if a.kind == "shell" {
        let c = kind_color("shell");
        fb.line(x - 3.0 * s, y - 2.0 * s, x + 3.0 * s, y - 2.0 * s, c, c);
        fb.line(x - 3.0 * s, y + 2.0 * s, x + 3.0 * s, y + 2.0 * s, c, c);
        fb.line(x - 3.0 * s, y - 2.0 * s, x - 3.0 * s, y + 2.0 * s, c, c);
        fb.line(x + 3.0 * s, y - 2.0 * s, x + 3.0 * s, y + 2.0 * s, c, c);
        fb.splat(x - s, y, v3(0.45, 0.9, 0.7)); return;
    }
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
    hull(fb, (x, y), s, a.seed.to_bits(), a.role, mix(col, kind_color(&a.kind), 0.25) * grow);
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

/// A calm system map at distances where full-size ship effects would overlap.
fn galaxy_overview(fb: &mut Fb, app: &App, w: &World, l: &Lay) {
    let scr = |p| l.to_screen(&app.cam, p);
    for &(i, j) in w.lanes.iter() {
        let (a, b) = (scr(w.systems[i].wpos), scr(w.systems[j].wpos));
        fb.line(a.0, a.1, b.0, b.1, v3(0.025, 0.055, 0.08), v3(0.025, 0.055, 0.08));
    }
    let selected = app.selected_system(w);
    for s in &w.systems {
        let (x, y) = scr(s.wpos);
        let fleet = app.fleet(w, &Sel::Sys(s.key.clone()));
        let waiting = fleet.iter().any(|a| a.status == Status::Blocked || !a.war.is_empty());
        let done = fleet.iter().any(|a| a.status == Status::Done);
        let col = if waiting { Status::Blocked.color() } else if done { Status::Done.color() } else { s.star };
        // One small star and a steady status ring, independent of fleet size.
        orb(fb, x, y, 1.8, s.star * 0.8);
        if waiting || done { circle(fb, x, y, 4.5, col * 0.28); }
        if app.focused_sector().is_some_and(|g| s.planets.iter().any(|&i| app.sector_has(g, &w.planets[i].key))) { ellipse(fb, x, y, 7.0, 5.0, v3(0.3, 0.45, 0.7)); }
        if selected.is_some_and(|picked| picked.key == s.key) { overview_reticle(fb, x, y); }
    }
    if let Some(p) = w.station {
        let (x, y) = scr(p);
        let col = if all_prs(app, w).iter().any(|(_, p)| p.ci == "fail") { ci_color("fail") } else { v3(0.55, 0.7, 0.9) };
        // A simple diamond: no overlapping PR ships at galaxy scale.
        for (a, b) in [((0.0, -4.0), (4.0, 0.0)), ((4.0, 0.0), (0.0, 4.0)), ((0.0, 4.0), (-4.0, 0.0)), ((-4.0, 0.0), (0.0, -4.0))] {
            fb.line(x + a.0, y + a.1, x + b.0, y + b.1, col * 0.55, col * 0.55);
        }
        if app.sel == Some(Sel::GitHub) { overview_reticle(fb, x, y); }
    }
}

fn overview_reticle(fb: &mut Fb, x: f32, y: f32) {
    for dx in [-1.0, 1.0] {
        for dy in [-1.0, 1.0] {
            fb.line(x + dx * 5.0, y + dy * 7.0, x + dx * 7.0, y + dy * 7.0, TEXT * 0.6, TEXT * 0.6);
            fb.line(x + dx * 7.0, y + dy * 5.0, x + dx * 7.0, y + dy * 7.0, TEXT * 0.6, TEXT * 0.6);
        }
    }
}

pub struct OverviewLabel {
    pub target: Sel,
    pub col: i32,
    pub row: i32,
    pub lines: Vec<(String, V3)>,
}

/// Shared by drawing and picking, so moved labels remain clickable. Prefer full
/// summaries; in a crowded map, retain names and let zooming reveal the rest.
pub fn overview_labels(app: &App, w: &World, l: &Lay) -> Vec<OverviewLabel> {
    let selected = app.selected_system(w);
    let mut entries = Vec::new();
    for s in &w.systems {
        let fleet = app.fleet(w, &Sel::Sys(s.key.clone()));
        let waiting = fleet.iter().filter(|a| a.status == Status::Blocked).count();
        let finished = fleet.iter().filter(|a| a.status == Status::Done).count();
        let working = fleet.iter().filter(|a| a.status == Status::Working).count();
        let conflicts = fleet.iter().any(|a| !a.war.is_empty());
        let picked = selected.is_some_and(|picked| picked.key == s.key);
        let sector = app.focused_sector().is_some_and(|g| s.planets.iter().any(|&i| app.sector_has(g, &w.planets[i].key)));
        let mut lines = vec![
            (format!("{}{}", if sector { "▱ " } else { "" }, s.name.to_uppercase()), if picked { TEXT } else if sector { v3(0.65, 0.8, 1.0) } else { s.star * 0.9 }),
            (format!("{} world{} · {} ship{}", s.planets.len(), if s.planets.len() == 1 { "" } else { "s" }, fleet.len(), if fleet.len() == 1 { "" } else { "s" }), DIM),
        ];
        let mut status = Vec::new();
        let anomalies = app.anomaly_count(&s.key, true);
        if anomalies > 0 { status.push(format!("⟡{anomalies} anomalies")); }
        if waiting > 0 { status.push(format!("{waiting} waiting")); }
        if conflicts { status.push("conflict".into()); }
        if finished > 0 { status.push(format!("{finished} finished")); }
        if working > 0 && status.is_empty() { status.push(format!("{working} working")); }
        if !status.is_empty() { lines.push((status.join(" · "), if waiting > 0 || conflicts { v3(1.0, 0.55, 0.4) } else if finished > 0 { Status::Done.color() * 0.8 } else { DIM })); }
        entries.push((!picked, waiting == 0 && !conflicts, Sel::Sys(s.key.clone()), s.wpos, lines));
    }
    if let Some(pos) = w.station {
        let prs = all_prs(app, w);
        let fails = prs.iter().filter(|(_, p)| p.ci == "fail").count();
        let mut lines = vec![("GITHUB".into(), v3(0.6, 0.75, 0.9)), (format!("{} PR{}", prs.len(), if prs.len() == 1 { "" } else { "s" }), DIM)];
        if fails > 0 { lines.push((format!("{fails} failing"), ci_color("fail"))); }
        entries.push((app.sel != Some(Sel::GitHub), fails == 0, Sel::GitHub, pos, lines));
    }
    place_map_labels(app, l, entries)
}

type MapEntry = (bool, bool, Sel, (f32, f32), Vec<(String, V3)>);

fn place_map_labels(app: &App, l: &Lay, mut entries: Vec<MapEntry>) -> Vec<OverviewLabel> {
    entries.sort_by_key(|e| (e.0, e.1));
    let cw = l.cols - l.pw;
    let overlaps = |a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)| a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3;
    // Reserve every node, including the station, before placing any label.
    let mut occupied: Vec<_> = entries.iter().map(|e| {
        let (x, y) = l.to_screen(&app.cam, e.3);
        ((x / 2.0).floor() as i32 - 4, (y / 4.0).floor() as i32 - 2, 9, 5)
    }).collect();
    let mut labels = Vec::new();
    for (_, _, target, pos, mut lines) in entries {
        let (x, y) = l.to_screen(&app.cam, pos);
        if x < 0.0 || x >= l.vw || y < l.vy0 || y >= l.vy0 + l.vh { continue; }
        let (c, r) = ((x / 2.0).floor() as i32, (y / 4.0).floor() as i32);
        for (text, _) in &mut lines { *text = trunc(text, (cw - 2).clamp(1, 36) as usize); }
        for full in [true, false] {
            if !full { lines.truncate(1); }
            let width = lines.iter().map(|(s, _)| s.chars().count() as i32).max().unwrap_or(1);
            let height = lines.len() as i32;
            let candidates = [(c - width / 2, r + 3), (c - width / 2, r - height - 2), (c + 5, r - height / 2), (c - width - 5, r - height / 2)];
            if let Some((col, row)) = candidates.into_iter().map(|(c, r)| (c.clamp(1, (cw - width - 1).max(1)), r)).find(|&(c, r)| {
                r >= 1 && r + height < l.rows - 1 && !occupied.iter().any(|&rect| overlaps((c - 1, r, width + 2, height), rect))
            }) {
                occupied.push((col - 1, row, width + 2, height));
                labels.push(OverviewLabel { target: target.clone(), col, row, lines });
                break;
            }
        }
    }
    labels
}

fn sector_color(id: u64) -> V3 {
    let colors = [v3(0.4, 0.8, 0.75), v3(0.73, 0.58, 0.9), v3(0.85, 0.73, 0.4), v3(0.48, 0.65, 0.95), v3(0.85, 0.5, 0.65)];
    colors[id.wrapping_sub(1) as usize % colors.len()]
}

/// Sector names lead; world and system labels use the room left around them.
pub fn sector_labels(app: &App, w: &World, l: &Lay) -> Vec<OverviewLabel> {
    let mut entries = Vec::new();
    let regions = app.sector_regions(w);
    for r in &regions {
        let Some(g) = app.sectors.sectors.iter().find(|g| g.id == r.id) else { continue };
        let target = Sel::Sector(r.id, r.system.clone());
        let across = regions.iter().filter(|p| p.id == r.id).count();
        let count = format!("{} world{}", r.worlds.len(), if r.worlds.len() == 1 { "" } else { "s" });
        let detail = if across > 1 { format!("{count} here · {across} systems") } else { count };
        entries.push((app.sel != Some(target.clone()), false, target, (r.center().0, r.bounds.3), vec![
            (format!("▱ {}", g.name.to_uppercase()), sector_color(r.id)), (detail, DIM),
        ]));
    }
    for s in &w.systems {
        entries.push((true, true, Sel::Sys(s.key.clone()), s.wpos, vec![(s.name.to_uppercase(), s.star * 0.65)]));
    }
    for p in w.planets.iter().filter(|p| p.host.is_none()) {
        entries.push((true, true, Sel::Planet(p.key.clone()), p.wpos, vec![(p.name.clone(), DIM)]));
    }
    if let Some(pos) = w.station { entries.push((true, true, Sel::GitHub, pos, vec![("GITHUB".into(), DIM)])); }
    place_map_labels(app, l, entries)
}

fn galaxy_sectors(fb: &mut Fb, app: &App, w: &World, l: &Lay) {
    let scr = |p| l.to_screen(&app.cam, p);
    for r in app.sector_regions(w) {
        let col = sector_color(r.id);
        let picked = app.sel == Some(Sel::Sector(r.id, r.system.clone())) || app.sector_focus == Some(r.id);
        let (a, b) = (scr((r.bounds.0, r.bounds.1)), scr((r.bounds.2, r.bounds.3)));
        for y in (a.1.floor() as i32).max(l.vy0 as i32)..=(b.1.ceil() as i32).min((l.vy0 + l.vh) as i32 - 1) {
            for x in (a.0.floor() as i32).max(0)..=(b.0.ceil() as i32).min(l.vw as i32 - 1) {
                let p = ((x as f32 - l.vw * 0.5) / app.cam.z + app.cam.x, (y as f32 - l.vy0 - l.vh * 0.5) / app.cam.z + app.cam.y);
                if r.contains(p) { fb.add(x, y, col * if picked { 0.045 } else { 0.025 }); }
            }
        }
        for i in 0..r.hull.len() {
            let (a, b) = (scr(r.hull[i]), scr(r.hull[(i + 1) % r.hull.len()]));
            let edge = col * if picked { 0.55 } else { 0.26 };
            fb.line(a.0, a.1, b.0, b.1, edge, edge);
        }
    }
    galaxy_overview(fb, app, w, l);
    for p in w.planets.iter().filter(|p| p.host.is_none()) {
        let (x, y) = scr(p.wpos);
        orb(fb, x, y, 2.0, planet_color(p) * 0.75);
        if app.anomaly_count(&p.key, false) > 0 { circle(fb, x, y, 4.0, v3(0.45, 0.32, 0.12)); }
        if app.sel == Some(Sel::Planet(p.key.clone())) { overview_reticle(fb, x, y); }
    }
}

fn galaxy(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    if app.sector_map() { galaxy_sectors(fb, app, w, l); return; }
    if app.cam.z < OVERVIEW_ZOOM {
        galaxy_overview(fb, app, w, l);
        return;
    }
    let cam = &app.cam;
    let z = cam.z;
    let scr = |p: (f32, f32)| l.to_screen(cam, p);

    for &(i, j) in w.lanes.iter() {
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
        if p.host.is_some() {
            worktree_station(fb, px, py, r, p, t);
        } else {
            sphere(fb, app, px, py, r, p, (lx / ll, ly / ll), t);
        }
        if app.focused_sector().is_some_and(|s| app.sector_has(s, &p.key)) { ellipse(fb, px, py, r + 7.0, r + 5.0, v3(0.3, 0.45, 0.7)); }
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
        Some(Sel::Research) => None,
        Some(Sel::GitHub) => w.station.map(|p| (p, (13.0 * z).max(5.0) * 2.2 + 5.0)),
        Some(Sel::Agent(id)) => app.agent(*id).map(|a| (a.wpos, 6.0 * z.clamp(0.7, 2.2) + 2.0)),
        Some(Sel::Planet(k)) => w.planet(k).map(|p| (p.wpos, (p.r * z).max(2.5) + 6.0)),
        Some(Sel::Sys(k)) => w.systems.iter().find(|s| &s.key == k).map(|s| (s.wpos, (8.0 * z).max(2.2) + 8.0)),
        Some(Sel::Sector(_, _)) | None => None,
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

/// Keep the file or final folder visible when its parents do not fit.
fn path_tail(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.into() }
    else { format!("…{}", s.chars().skip(s.chars().count().saturating_sub(w.saturating_sub(1))).collect::<String>()) }
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
fn all_prs<'a>(app: &'a App, world: &'a World) -> Vec<(&'a String, &'a survey::Pr)> {
    let mut v: Vec<_> = world.planets.iter().filter(|p| p.host.is_none()).filter_map(|p| {
        app.survey.worlds.get(&p.key).filter(|s| s.github).map(|s| (&p.key, s))
    }).flat_map(|(k, s)| s.prs.iter().map(move |p| (k, p))).collect();
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
    let prs = all_prs(app, w);
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

pub fn station_label(app: &App, w: &World, l: &Lay) -> Option<(i32, i32, String)> {
    let (sx, sy) = l.to_screen(&app.cam, w.station?);
    let prs = all_prs(app, w);
    let fails = prs.iter().filter(|(_, p)| p.ci == "fail").count();
    let mut text = format!("◇ GITHUB · {} PR{}", prs.len(), if prs.len() == 1 { "" } else { "s" });
    if fails > 0 { text.push_str(&format!(" · ✖{fails}")); }
    let len = text.chars().count() as i32;
    let c = ((sx / 2.0) as i32 - len / 2).min(l.cols - l.pw - len - 1);
    let r = ((sy + (13.0 * app.cam.z).max(5.0) * 1.3 + 8.0) / 4.0) as i32;
    Some((c, r, text))
}

fn trade_layout(l: &Lay, count: usize, selected: usize) -> (i32, i32, i32, i32, usize, usize) {
    let (x, y, width, height) = centered(l, 96, (count as i32 + 9).max(13).min(l.rows - 2));
    let room = (height - 8).max(1) as usize;
    let selected = selected.min(count.saturating_sub(1));
    let offset = (selected + 1).saturating_sub(room);
    (x, y, width, height, offset, room)
}

pub fn trade_hit(l: &Lay, count: usize, selected: usize, col: i32, row: i32) -> Option<usize> {
    let (x, y, width, _, offset, room) = trade_layout(l, count, selected);
    if col < x + 2 || col >= x + width - 2 || row < y + 4 || row >= y + 4 + room as i32 { return None; }
    let index = offset + (row - y - 4) as usize;
    (index < count).then_some(index)
}

fn trade_overview(fb: &mut Fb, app: &App, world: &World, l: &Lay, selected: usize, t: f32) {
    let rows = app.trade_visible_rows(world);
    let selected = selected.min(rows.len().saturating_sub(1));
    let (x, y, width, height, offset, room) = trade_layout(l, rows.len(), selected);
    let accent = v3(0.6, 0.8, 1.0);
    let projects = rows.iter().filter(|r| r.pr.is_none()).count();
    let prs = rows.len() - projects;
    let place = app.trade_world.as_deref().map_or_else(|| format!("{projects} projects"), base_name);
    frame_box(fb, x, y, width, height, &format!("◇ GITHUB · {place} · {prs} PRs · {}", app.trade_view.label()), accent);
    let views = [TradeView::All, TradeView::Review, TradeView::Mine].map(|v| if v == app.trade_view { format!("[{}]", v.label()) } else { v.label().to_string() }).join("  ");
    seg(fb, x + 3, y + 2, &format!("{} · ⇥ {views}", if app.trade_world.is_some() { "This repository" } else { "Projects in this galaxy" }), width - 6, DIM, Some(BOX));
    if rows.is_empty() {
        seg(fb, x + 3, y + 4, "No GitHub projects in this galaxy yet.", width - 6, TEXT, Some(BOX));
    }
    for (i, row) in rows.iter().enumerate().skip(offset).take(room) {
        let r = y + 4 + (i - offset) as i32;
        let bg = if i == selected { SELBG } else { BOX };
        fb.panel(x + 2, r, width - 4, 1, bg);
        if let Some(pr) = &row.pr {
            let (icon, status) = match pr.ci { "fail" => ("✖", "failing"), "pending" => ("◷", "pending"), "pass" => ("✔", "passing"), _ => ("○", "no checks") };
            let prefix = format!("  {icon} #{} ", pr.number);
            let c = seg(fb, x + 3, r, &prefix, width - 18, ci_color(pr.ci), Some(bg));
            // someone else's: who opened it, and whether it waits on your review
            let mine = app.survey.worlds.get(&row.path).is_some_and(|s| s.prs.iter().any(|m| m.number == pr.number));
            let (who, who_col) = if mine { (String::new(), DIM) }
                else if pr.asks_you { (format!("◆ review · @{}", pr.author), v3(1.0, 0.75, 0.3)) }
                else { (format!("@{}", pr.author), DIM) };
            let who = clip(&who, 24);
            let end = x + width - 15 - who.chars().count() as i32 - if who.is_empty() { 0 } else { 2 };
            let title = if pr.draft { format!("{} (draft)", pr.title) } else { pr.title.clone() };
            seg(fb, c, r, &clip(&title, (end - c).max(1) as usize), (end - c).max(1), if pr.draft { DIM } else { TEXT }, Some(bg));
            seg(fb, end + 1, r, &who, who.chars().count() as i32, who_col, Some(bg));
            seg(fb, x + width - 13, r, status, 10, ci_color(pr.ci), Some(bg));
        } else if let Some(s) = app.survey.worlds.get(&row.path) {
            let state = if s.prs_error.is_some() { "PRs unavailable".to_string() }
                else if s.prs_checked.is_none() { "checking PRs…".to_string() }
                else {
                    let asks = s.others.iter().filter(|p| p.asks_you).count();
                    let mut parts = vec![format!("{} yours", s.prs.len())];
                    if asks > 0 { parts.push(format!("{asks} to review")); }
                    parts.push(format!("{} others", s.others.len()));
                    parts.join(" · ")
                };
            let summary = format!("{state} · ↑{} unpushed", s.ahead);
            let room = (width - summary.chars().count() as i32 - 9).max(1);
            seg(fb, x + 3, r, &clip(&format!("● {}", base_name(&row.path)), room as usize), room, accent, Some(bg));
            seg(fb, x + width - 3 - summary.chars().count() as i32, r, &summary, summary.chars().count() as i32, DIM, Some(bg));
        }
    }
    if let Some(row) = rows.get(selected) {
        let s = &app.survey.worlds[&row.path];
        let note = if let Some((message, _)) = app.toast.as_ref().filter(|(_, at)| t - at < 3.2) { message.clone() }
            else if let Some(error) = &s.prs_error { format!("{error} · showing cached PRs") }
            else if let Some(at) = s.prs_checked { format!("{} · checked {} ago · up to 100 PRs/project", tilde(&row.path), ago((survey::now() - at).max(0) as f32)) }
            else { format!("{} · waiting for GitHub survey", tilde(&row.path)) };
        seg(fb, x + 3, y + height - 3, &clip(&note, (width - 6).max(1) as usize), width - 6, DIM, Some(BOX));
    }
    seg(fb, x + 3, y + height - 2, "↑↓ select · ⇥ view · t mission (review / fix a PR) · ⏎ focus / open PR · esc close", width - 6, DIM, Some(BOX));
}

struct PlanetLabel {
    col: i32, row: i32, name: String, parts: Vec<(String, V3)>, signal: Option<PrSignal>,
}
impl PlanetLabel {
    fn marker_col(&self) -> i32 { self.col + self.name.chars().count() as i32 + 1 }
}

/// One shared label geometry for painting and clicking its PR marker.
fn planet_label(app: &App, p: &Planet, l: &Lay, t: f32) -> Option<PlanetLabel> {
    let z = app.cam.z;
    if z < OVERVIEW_ZOOM || app.sector_map() || (p.r * z < 3.0 && app.sel != Some(Sel::Planet(p.key.clone()))) { return None; }
    let (px, py) = l.to_screen(&app.cam, p.wpos);
    let width = l.cols - l.pw;
    if px < 0.0 || px >= width as f32 * 2.0 { return None; }
    let row = ((py + (p.r * z).max(2.5) + 7.0) / 4.0) as i32;
    if row < 1 || row >= l.rows - 1 { return None; }
    let parts = tally(p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none()), t);
    let signal = app.pr_signal(p);
    let extra = parts.iter().map(|(s, _)| s.chars().count().min(8) as i32 + 1).sum::<i32>()
        + signal.as_ref().map_or(0, |s| s.label().chars().count() as i32 + 1);
    let room = width - extra - 2;
    if room < 1 { return None; }
    let anomalies = app.anomaly_count(&p.key, false);
    let name = clip(&format!("{}{}{}", if p.host.is_some() { "▣ " } else { "" }, p.name, if anomalies > 0 { format!(" ⟡{anomalies}") } else { String::new() }), room as usize);
    let len = name.chars().count() as i32 + extra;
    Some(PlanetLabel { col: ((px / 2.0) as i32 - len / 2).clamp(0, width - len - 1), row, name, parts, signal })
}

pub fn pr_marker_hit(app: &App, w: &World, l: &Lay, col: i32, row: i32, t: f32) -> Option<String> {
    if !matches!(app.mode, Mode::Galaxy) { return None; }
    if l.pw > 0 && col >= l.cols - l.pw {
        let Sel::Planet(key) = panel_hit(app, w, l, row)? else { return None };
        let signal = app.pr_signal(w.planet(&key)?)?;
        let start = l.cols - 1 - signal.label().chars().count() as i32;
        return (col >= start && col < l.cols - 1).then_some(key);
    }
    w.planets.iter().rev().find_map(|p| {
        let label = planet_label(app, p, l, t)?;
        let signal = label.signal.as_ref()?;
        let start = label.marker_col();
        (row == label.row && col >= start && col < start + signal.label().chars().count() as i32).then(|| p.key.clone())
    })
}

fn labels(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    if app.cam.z < OVERVIEW_ZOOM || app.sector_map() {
        for label in if app.sector_map() { sector_labels(app, w, l) } else { overview_labels(app, w, l) } {
            for (i, (text, col)) in label.lines.iter().enumerate() {
                fb.text(label.col, label.row + i as i32, text, *col);
            }
        }
        return;
    }
    let cam = &app.cam;
    let z = cam.z;
    let cw = l.cols - l.pw;
    let in_view = |c: i32, r: i32| c >= 0 && c < cw && r >= 1 && r < l.rows - 1;
    for s in &w.systems {
        let (sx, sy) = l.to_screen(cam, s.wpos);
        let n = app.anomaly_count(&s.key, true);
        let name = format!("{}{}", s.name.to_uppercase(), if n > 0 { format!(" ⟡{n}") } else { String::new() });
        let len = name.chars().count() as i32;
        let (c, r) = ((sx / 2.0) as i32 - len / 2, ((sy - (8.0 * z).max(2.2) - 6.0) / 4.0) as i32);
        if in_view(c, r) && in_view(c + len - 1, r) {
            fb.text(c, r, &name, s.star * 0.75);
        }
    }
    if let Some(st) = w.station {
        let (sx, _) = l.to_screen(cam, st);
        let prs = all_prs(app, w);
        let fails = prs.iter().filter(|(_, p)| p.ci == "fail").count();
        let (c, r, s) = station_label(app, w, l).unwrap();
        if in_view(c, r) && (sx / 2.0) < cw as f32 {
            fb.text(c, r, &s, if fails > 0 { v3(1.0, 0.45, 0.4) } else { v3(0.6, 0.75, 0.9) });
        }
    }
    for p in &w.planets {
        let selected = app.sel.as_ref() == Some(&Sel::Planet(p.key.clone()));
        let Some(label) = planet_label(app, p, l, t) else { continue };
        let (px, _) = l.to_screen(cam, p.wpos);
        let r = label.row;
        let mut c = seg(fb, label.col, r, &label.name, cw - label.col, if selected { s3(1.0) } else { TEXT * (1.0 - 0.65 * p.fog) }, None);
        if let Some(signal) = label.signal { c = seg(fb, c + 1, r, &signal.label(), cw - c - 1, signal.color(), None); }
        for (s, col) in label.parts {
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
    let title = if l.cols < 32 { " ◆ " } else if l.cols < 60 { " ◆ EMPIRE " }
        else if app.sector_map() { " ◆ AGENT EMPIRE · SECTORS " } else { " ◆ AGENT EMPIRE " };
    for (k, ch) in title.chars().enumerate() {
        fb.put_bg(k as i32, 0, ch, hsv(0.52 + k as f32 * 0.015 - t * 0.04, 0.45, 1.0), Some(BAR));
    }
    let live: Vec<&Agent> = app.agents.iter().filter(|a| a.dying.is_none()).collect();
    let stations = w.planets.iter().filter(|p| p.host.is_some()).count();
    let worlds = w.planets.len() - stations;
    let stations = if stations > 0 { format!(" · {stations} station{}", if stations == 1 { "" } else { "s" }) } else { String::new() };
    let stats = [format!(" {} systems · {worlds} worlds{stations} · {} agents  ", w.systems.len(), live.len()),
        format!(" {worlds} worlds · {} agents  ", live.len()), format!(" {} agents  ", live.len())];
    let mark = if app.online { "●" } else { "○" };
    let right = if l.cols >= 100 { format!("{mark} daemon  {} ", clock()) }
        else { format!("{mark} {} ", if app.online { "online" } else { "offline" }) };
    let rc = l.cols - right.chars().count() as i32;
    let start = title.chars().count() as i32;
    // measure whole groups before drawing; leave connection status its own space
    for (summary, all, detail) in [(0, true, 2), (0, true, 1), (1, true, 1), (2, true, 0), (2, false, 0), (2, false, -1)] {
        let counts = tally(live.iter().copied().filter(|a| all || a.status.needs_you()), t);
        let parts = if detail < 0 { vec![] } else { resources(&app.survey.limits, detail) };
        let sw = stats[summary].chars().count() as i32 + counts.iter().map(|(s, _)| s.chars().count() as i32 + 2).sum::<i32>();
        let rw: i32 = parts.iter().map(|(s, _)| s.chars().count() as i32).sum();
        if start + sw + rw + 2 > rc { continue; }
        let mut x = seg(fb, start, 0, &stats[summary], sw, DIM, Some(BAR));
        for (s, c) in counts { x = seg(fb, x, 0, &format!("{s}  "), rc - x, c, Some(BAR)); }
        let mut x = rc - rw - 2;
        for (s, c) in parts { x = seg(fb, x, 0, &s, rc - x, c, Some(BAR)); }
        break;
    }
    let c = if app.online { v3(0.35, 1.0, 0.55) } else { v3(1.0, 0.35, 0.3) };
    seg(fb, rc, 0, &right, l.cols - rc, c, Some(BAR));
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
fn resources(limits: &[survey::Limit], detail: i32) -> Vec<(String, V3)> {
    let mut out = Vec::new();
    let mut who: Vec<&str> = limits.iter().map(|l| l.who).collect();
    who.dedup();
    for name in who {
        let mut ls: Vec<&survey::Limit> = limits.iter().filter(|l| l.who == name).collect();
        if detail == 0 {
            // keep the tightest window and its label when allowances must be compact
            if let Some(l) = ls.iter().copied().max_by(|a, b| a.used.total_cmp(&b.used)) { ls = vec![l]; }
        }
        out.push((format!("{} ", name.to_uppercase()), kind_color(name)));
        for (i, lim) in ls.iter().enumerate() {
            if i > 0 {
                out.push((" · ".into(), DIM * 0.7));
            }
            out.push((format!("{} ", lim.window), DIM));
            if detail == 2 {
                out.push((format!("{} ", meter(lim.used / 100.0, 5)), strain(lim.used)));
            }
            out.push((format!("{:.0}%", lim.used), strain(lim.used)));
        }
        // when the tightest window will refill, once it starts to matter
        if let Some(l) = ls.iter().max_by(|a, b| a.used.total_cmp(&b.used))
            && detail > 0 && l.used >= 50.0
            && let Some(r) = l.resets
        {
            out.push((format!(" ↻{}", until(r - survey::now())), DIM));
        }
        out.push((if detail == 0 { "  " } else { "   " }.into(), DIM));
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
    let groups: Vec<_> = app.sectors.sectors.iter().filter(|s| app.sector_has(s, &p.key)).map(|s| s.name.as_str()).collect();
    if !groups.is_empty() { rows.push(("sectors", vec![(format!("{} · V", groups.join(", ")), v3(0.65, 0.8, 1.0))])); }
    let built = app.survey.worlds.get(&p.key).map(|g| g.structures.clone()).unwrap_or_default();
    let mut st: Vec<(String, V3)> = built.iter().map(|b| (format!("{} {}{} ", b.kind.glyph(), b.kind.what(), if b.count > 1 { format!(" ×{}", b.count) } else { String::new() }), b.kind.color())).collect();
    st.push((if built.is_empty() { "no structures yet · B build".into() } else { format!("{} plots free · B", KINDS.len() - built.len()) }, DIM));
    rows.push(("structures", st));
    if p.dormant && p.host.is_none() {
        let age = ago((survey::now() as u64).saturating_sub(p.last_seen) as f32);
        let txt = if p.fog > 0.8 { "lost in the nebula" } else if p.fog > 0.5 { "fading" } else { "dormant" };
        rows.push(("status", vec![(format!("{txt} · quiet for {age}"), v3(0.6, 0.5, 0.85)), ("  e deploy · x forget".into(), DIM)]));
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
    let anomalies = app.anomaly_count(&p.key, false);
    if anomalies > 0 { rows.push(("anomalies", vec![(format!("⟡ {anomalies} active · A inspect"), v3(0.8, 0.65, 1.0))])); }
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
    let anomalies = app.anomaly_count(&s.key, true);
    if anomalies > 0 { rows.push(("anomalies", vec![(format!("⟡ {anomalies} active · A inspect"), v3(0.8, 0.65, 1.0))])); }
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

/// One ship: what it's on, what it has cost, and the orders it carries.
fn ship_rows(app: &App, w: &World, a: &Agent, t: f32) -> Rows {
    let mut rows: Rows = Vec::new();
    if let Some(q) = &a.ask {
        rows.push(("asking", vec![(clip(&q.question, 42), Status::Blocked.color())]));
    }
    rows.push(("task", vec![(if a.title.is_empty() { "—".into() } else { clip(&a.title, 42) }, TEXT)]));
    if a.role != Role::None {
        rows.push(("role", vec![(format!("{} ", a.role.name()), a.role.color()), (clip(a.role.about(), 36), DIM)]));
    }
    let mut home = vec![];
    if let Some(p) = w.planet(&a.cwd) {
        home.push((format!("{} ", p.name), planet_color(p)));
        if let Some(h) = p.host {
            home.push((format!("▣ station of {} ", w.planets[h].name), DIM));
        }
    }
    if let Some(g) = app.survey.worlds.get(&a.cwd).filter(|g| g.git) {
        home.push((format!("⎇ {}", g.branch), v3(0.6, 0.85, 1.0)));
    }
    if !home.is_empty() {
        rows.push(("world", home));
    }
    let rep = &a.report;
    if let Some(m) = &rep.model {
        rows.push(("model", vec![(m.clone(), kind_color(&a.kind))]));
    }
    if let Some(c) = rep.ctx {
        rows.push(("context", vec![(format!("{} {c:.0}%", meter(c / 100.0, 8)), strain(c))]));
    }
    let mut out = vec![(format!("{} turn{} ", a.turns, if a.turns == 1 { "" } else { "s" }), TEXT)];
    if rep.added.is_some() || rep.removed.is_some() {
        out.push((format!("+{} ", num(rep.added.unwrap_or(0))), v3(0.4, 0.95, 0.6)));
        out.push((format!("−{}", num(rep.removed.unwrap_or(0))), v3(1.0, 0.45, 0.4)));
    }
    if a.status == Status::Working {
        out.push((format!("  working {}", ago(t - a.busy_at)), DIM));
    }
    rows.push(("output", out));
    if let Some(c) = rep.cost {
        rows.push(("upkeep", vec![(format!("${c:.2}"), v3(1.0, 0.85, 0.4)), (" this session".into(), DIM)]));
    }
    for war in &a.war {
        let rival = app.agent(war.rival).map_or(format!("#{}", war.rival), |r| format!("{} #{}", r.kind, r.id));
        rows.push(("conflict", vec![(format!("⚔ vs {rival} "), v3(1.0, 0.45, 0.3)), (war.files.join(", "), TEXT)]));
    }
    fleet_row(&mut rows, app, &[a]);
    // its own orders: what it's on now first, then what it finished
    let mut ms: Vec<&crate::proto::Mission> = app.missions.iter().filter(|m| m.agent == Some(a.id)).collect();
    ms.sort_by_key(|m| (m.state == "done", std::cmp::Reverse(m.done_at.unwrap_or(m.at))));
    for (i, m) in ms.iter().take(4).enumerate() {
        let (g, c) = if m.state == "done" { ("★", v3(1.0, 0.85, 0.4)) } else { ("◆", v3(0.4, 0.85, 1.0)) };
        rows.push((if i == 0 { "missions" } else { "" }, vec![(format!("{g} "), c), (clip(&m.text, 38), if m.state == "done" { DIM } else { TEXT })]));
    }
    if ms.len() > 4 {
        rows.push(("", vec![(format!("+{} more", ms.len() - 4), DIM)]));
    }
    rows
}

/// A Stellaris-style info card for the selected ship, world or system, bottom left of the map.
fn card(fb: &mut Fb, app: &App, w: &World, l: &Lay, t: f32) {
    let planet_key = match &app.sel {
        Some(Sel::Planet(k)) => Some(k.clone()),
        _ => None,
    };
    let (title, sub, accent, rows) = if let Some(Sel::Agent(id)) = &app.sel
        && let Some(a) = app.agent(*id)
    {
        let sub = format!("{} {} · {} · {}", a.status.glyph(t), a.status.label(), ago(t - a.since), tilde(&a.cwd));
        (format!("◆ {} #{} · {}", a.kind.to_uppercase(), a.id, base_name(&a.cwd).to_uppercase()), sub, kind_color(&a.kind), ship_rows(app, w, a, t))
    } else if let Some(p) = planet_key.as_deref().and_then(|k| w.planet(k)) {
        if let Some(h) = p.host {
            let sub = format!("station of {} · worktree at {}", w.planets[h].name, tilde(&p.key));
            (format!("▣ {}", p.name.to_uppercase()), sub, planet_color(p), planet_rows(app, w, p, t))
        } else {
            let class = CLASSES[p.terrain as usize % CLASSES.len()];
            (format!("◉ {}", p.name.to_uppercase()), format!("{class} world · {}", tilde(&p.key)), planet_color(p), planet_rows(app, w, p, t))
        }
    } else if let Some(Sel::Sys(k)) = &app.sel
        && let Some(s) = w.systems.iter().find(|s| &s.key == k)
    {
        (format!("✶ {} SYSTEM", s.name.to_uppercase()), tilde(&s.key), s.star, system_rows(app, w, s, t))
    } else if let Some(Sel::Sector(id, system)) = &app.sel
        && let Some(g) = app.sectors.sectors.iter().find(|g| g.id == *id)
    {
        let stats = app.sector_stats(g);
        (format!("▱ {} SECTOR", g.name.to_uppercase()), format!("{} · {} worlds across the galaxy", base_name(system), stats.worlds), sector_color(*id), vec![
            ("fleet", vec![(stats.activity(), TEXT)]), ("work", vec![(stats.work(), DIM)]), ("browse", vec![("⏎ zoom into these worlds".into(), TEXT)]),
        ])
    } else if app.sel == Some(Sel::Research) {
        ("◎ RESEARCH STATION".into(), "Improve how your agents work".into(), v3(0.43, 0.88, 0.79), vec![("research", vec![("Snapshots · trials · evidence · resources".into(), TEXT)]), ("open", vec![("Enter or R to enter the laboratory".into(), TEXT)])])
    } else if app.sel == Some(Sel::GitHub) {
        let projects = app.trade_rows(w).iter().filter(|r| r.pr.is_none()).count();
        let rows = vec![
            ("projects", vec![(projects.to_string(), TEXT)]),
            ("your PRs", vec![(all_prs(app, w).len().to_string(), TEXT)]),
            ("browse", vec![("⏎ projects and pull requests".into(), TEXT)]),
        ];
        ("◇ GITHUB STATION".into(), "Projects in this galaxy".into(), v3(0.6, 0.8, 1.0), rows)
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
    // the command (or file) it wants is the point of the message: wrapped in full and lit up
    let cmd = v3(1.0, 0.82, 0.45);
    let lines = |s: &str, max: usize, c: V3| {
        let mut w = wrap(s, inner);
        if w.len() > max { w.truncate(max); w[max - 1] = trunc(&format!("{}…", w[max - 1]), inner); }
        w.into_iter().map(move |l| (l, c))
    };
    let mut context: Vec<Vec<(String, V3)>> = q.context.iter().map(|c| if commandish(c) { lines(c, 4, cmd).collect() } else { lines(c, 2, DIM).collect() }).collect();
    let mut rest: Vec<(String, V3)> = lines(&q.question, 2, TEXT).collect();
    for d in &q.detail {
        rest.extend(if commandish(d) { lines(d, 8, cmd).collect::<Vec<_>>() } else { lines(d, 3, TEXT * 0.8).collect() });
    }
    rest.push((String::new(), DIM));
    // on a short terminal the oldest context gives way first
    let room = (l.rows - 6 - q.options.len().min(9) as i32 - 3).max(0) as usize;
    while !context.is_empty() && context.iter().map(Vec::len).sum::<usize>() + rest.len() > room { context.remove(0); }
    let body: Vec<(String, V3)> = context.into_iter().flatten().chain(rest).collect();
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
    let hint = format!(" 1-{} answer · c comms{more} ", q.options.len().min(9));
    seg(fb, x + width - 2 - hint.chars().count() as i32, y + height - 1, &hint, width, DIM, Some(BOX));
}

/// A transmission line naming what the agent wants to run: Codex's `$ cmd` or `• Running …`, Claude's `Bash(…)`.
fn commandish(l: &str) -> bool {
    let l = l.trim_start_matches(['•', '⏺', '└', '●', ' ']).to_lowercase();
    l.starts_with("$ ") || l.starts_with("running ") || l.starts_with("bash(") || l.starts_with("command:")
}

pub fn commander_button(app: &App, w: &World) -> String {
    format!(" C commander ({}) ", app.attention(w).len())
}

fn footer(fb: &mut Fb, app: &App, _w: &World, l: &Lay, sound: Option<&Sound>) {
    fb.panel(0, l.rows - 1, l.cols, 1, BAR);
    let mute = sound.map(|s| if s.muted.load(Relaxed) { "m alerts off" } else { "m alerts on" });
    let music = sound.map(|s| if s.music_muted.load(Relaxed) { "M music off" } else { "M music on" });
    let ctx: &[&str] = match &app.sel {
        Some(Sel::Planet(key)) if key != crate::workspace::HOME => &["Enter project", "t task", "b terminal", "e agent", "p PRs", "x remove"],
        Some(Sel::Planet(_)) => &["Enter home"],
        Some(Sel::Agent(_)) => &["Enter terminal", "b terminal", "x dismiss"],
        Some(Sel::Research) => &["Enter research"],
        Some(Sel::GitHub) => &["Enter pull requests"],
        _ => &[],
    };
    // (hint, rank): on a narrow terminal the lowest-ranked hints give way, whole, rather than clipping help and leave
    let mut hints: Vec<(&str, u8)> = ctx.iter().enumerate().map(|(i, h)| (*h, if i == 0 { 0 } else { 2 })).collect();
    hints.extend([("R research", 3), ("H home", 3), ("n folder", 3), ("V sectors", 3), ("Tab next alert", 1), ("A anomalies", 3), ("? help", 0), ("q leave", 0)]);
    hints.extend(music.into_iter().chain(mute).map(|h| (h, 4)));
    let mut keep = vec![false; hints.len()];
    let mut used = 0;
    for rank in 0..=4 {
        for (i, (h, r)) in hints.iter().enumerate() {
            let n = h.chars().count() + if used > 0 { 3 } else { 0 };
            if *r == rank && used + n <= l.cols as usize { keep[i] = true; used += n; }
        }
    }
    let s = hints.iter().zip(keep).filter(|(_, k)| *k).map(|((h, _), _)| *h).collect::<Vec<_>>().join(" · ");
    seg(fb, 0, l.rows - 1, &s, l.cols, DIM, Some(BAR));
}

/// Outliner rows on screen: (rows, first screen row, visible count, scroll offset).
fn outliner_view(app: &App, w: &World, l: &Lay) -> (Vec<Sel>, i32, i32, usize) {
    let rows = app.rows(w);
    let log_h = ((l.rows - 6) / 3).clamp(3, 10);
    let first = 3;
    let vis = (l.rows - 2 - log_h - 2 - first).max(1);
    let max = rows.len().saturating_sub(vis as usize);
    let off = app.out_off.min(max);
    // once the selection moves away from where the wheel left it, keep it in view
    let off = match app.sel.as_ref().and_then(|s| rows.iter().position(|r| r == s)) {
        Some(si) if app.out_pin != app.sel => off.clamp(si.saturating_sub(vis as usize - 1), si),
        _ => off,
    };
    (rows, first, vis, off)
}

pub fn panel_hit(app: &App, w: &World, l: &Lay, row: i32) -> Option<Sel> {
    let (rows, first, vis, off) = outliner_view(app, w, l);
    let k = row - first;
    (k >= 0 && k < vis).then(|| rows.get(off + k as usize).cloned()).flatten()
}

/// Situation log rows in the side panel: (first entry's screen row, visible count, scroll offset).
fn panel_log_view(app: &App, w: &World, l: &Lay) -> (i32, usize, usize) {
    let (_, first, vis, _) = outliner_view(app, w, l);
    let r0 = first + vis + 3;
    let n = (l.rows - 2 - r0).max(0) as usize;
    (r0, n, app.log_off.min(app.log.len().saturating_sub(n)))
}

/// Wheel over the side panel: scroll whichever list is under the row by `d` lines.
pub fn panel_scroll(app: &mut App, w: &World, l: &Lay, row: i32, d: isize) {
    let (_, first, vis, off) = outliner_view(app, w, l);
    if row < first + vis {
        app.out_off = off.saturating_add_signed(d);
        app.out_pin = app.sel.clone();
    } else {
        let (_, _, off) = panel_log_view(app, w, l);
        app.log_off = off.saturating_add_signed(d);
    }
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
            Sel::Research => { seg(fb, c0, row, "◎ RESEARCH STATION", inner as i32, v3(0.43, 0.88, 0.79), Some(bg)); }
            Sel::GitHub => {
                seg(fb, c0, row, "◇ GITHUB STATION", inner as i32, v3(0.6, 0.8, 1.0), Some(bg));
            }
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
            Sel::Sector(id, _) => {
                if let Some(g) = app.sectors.sectors.iter().find(|g| g.id == *id) { seg(fb, c0 + 1, row, &format!("▱ {}", g.name), inner as i32 - 1, sector_color(*id), Some(bg)); }
            }
            Sel::Planet(key) => {
                let Some(p) = w.planet(key) else { continue };
                let parts = tally(p.agents.iter().map(|&i| &app.agents[i]).filter(|a| a.dying.is_none()), t);
                let tw: usize = parts.iter().map(|(s, _)| s.chars().count() + 1).sum();
                let (glyph, indent) = if p.host.is_some() { ("▣ ", 3) } else if p.dormant { ("◌ ", 1) } else { ("● ", 1) };
                let x = seg(fb, c0 + indent, row, glyph, 2, planet_color(p) * (1.0 - 0.6 * p.fog), Some(bg));
                let signal = app.pr_signal(p);
                let mut right = x0 + pw - 1;
                if let Some(signal) = signal {
                    let text = signal.label(); let len = text.chars().count() as i32;
                    seg(fb, right - len, row, &text, len, signal.color(), Some(bg));
                    right -= len + 1;
                }
                if p.dormant && p.host.is_none() {
                    let age = ago((survey::now() as u64).saturating_sub(p.last_seen) as f32);
                    seg(fb, x, row, &p.name, (right - x - age.len() as i32 - 1).max(0), DIM * (1.1 - 0.5 * p.fog), Some(bg));
                    seg(fb, right - age.len() as i32, row, &age, age.len() as i32, DIM * 0.7, Some(bg));
                    continue;
                }
                seg(fb, x, row, &p.name, (right - x - tw as i32 - 1).max(0), TEXT, Some(bg));
                let mut x = right - tw as i32;
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
                if a.role != Role::None {
                    x = seg(fb, x, row, a.role.name(), 9, a.role.color(), Some(bg)) + 1;
                }
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
    let x = seg(fb, x0 + 2, ly, "SITUATION LOG", pw, v3(0.6, 0.85, 1.0), Some(PANEL));
    seg(fb, x + 1, ly, "S", 1, DIM * 0.7, Some(PANEL));
    seg(fb, x0 + 2, ly + 1, &"─".repeat(pw as usize - 3), pw, v3(0.1, 0.2, 0.26), Some(PANEL));
    let (r0, n, loff) = panel_log_view(app, w, l);
    for (k, e) in app.log.iter().skip(loff).enumerate().take(n) {
        let r = r0 + k as i32;
        let x = seg(fb, x0 + 2, r, &e.clock, 5, DIM * 0.8, Some(PANEL)) + 1;
        seg(fb, x, r, &trunc(&e.msg, (x0 + pw - 1 - x) as usize), x0 + pw - 1 - x, e.col, Some(PANEL));
    }
}

/// The situation log entry under a row of the side panel, if any (its heading means the newest).
pub fn panel_log_hit(app: &App, w: &World, l: &Lay, row: i32) -> Option<u64> {
    let (r0, _, off) = panel_log_view(app, w, l);
    let k = row - r0;
    if k < -2 || row >= l.rows - 2 {
        return None;
    }
    app.log.get(if k < 0 { 0 } else { off + k as usize }).map(|e| e.id)
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

fn colonize(fb: &mut Fb, _app: &App, c: &Colonize, l: &Lay, t: f32) {
    let (x, y, w, h) = centered(l, 72, 17);
    let accent = v3(0.45, 0.85, 1.0);
    frame_box(fb, x, y, w, h, "◆ ADD A PROJECT FOLDER", accent);
    seg(fb, x + 3, y + 2, "Type a path: fuzzy matches ignore case. Or name a new folder.", w - 6, DIM, Some(BOX));
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
        let (m, col) = if exists { ("an existing folder — add it to your galaxy", v3(0.4, 0.9, 0.6)) } else { ("✦ uncharted space — this folder will be created", v3(1.0, 0.8, 0.4)) };
        seg(fb, x + 5, row + 2, m, w - 8, col, Some(BOX));
    }
    let selected = c.comp_i.saturating_sub(1) % c.comps.len().max(1);
    let first = selected.saturating_sub(5);
    for (k, comp) in c.comps.iter().enumerate().skip(first).take(6) {
        let hot = c.comp_i > 0 && (c.comp_i - 1) % c.comps.len() == k;
        seg(fb, x + 5, row + 2 + (k - first) as i32, &trunc(comp, (w - 8) as usize), w - 8, if hot { TEXT } else { DIM }, Some(if hot { SELBG } else { BOX }));
    }

    seg(fb, x + 3, y + h - 4, "Ctrl/Alt+Backspace or Ctrl+W: erase path segment", w - 6, DIM, Some(BOX));
    if c.mkdir {
        let m = format!("found new world at {}?  press ⏎ again", c.path.s.trim());
        seg(fb, x + 3, y + h - 3, &trunc(&m, (w - 6) as usize), w - 6, v3(1.0, 0.8, 0.4), Some(BOX));
    }
    seg(fb, x + 3, y + h - 2, "⏎ add folder · Tab/⇧Tab match · esc cancel", w - 6, DIM, Some(BOX));
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
    frame_box(fb, x, y, w, h, &format!("◆ HOME → {}", base_name(planet).to_uppercase()), accent);
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

/// Shared cell geometry for the landed view and its mouse targets.
pub struct PlanetLayout {
    pub split: i32,
    pub y: i32,
    pub step: i32,
    pub buildings: usize,
    pub files: usize,
    pub preview_y: i32,
    pub preview_rows: i32,
    pub width: i32,
}

impl PlanetLayout {
    pub fn new(l: &Lay) -> Self {
        let split = (l.cols / 3).clamp(22, 38).min(l.cols / 2);
        let y = 6;
        let step = if l.rows >= 27 { 2 } else { 1 };
        let buildings = ((l.rows - y - 3) / step).clamp(1, 8) as usize;
        let files = ((l.rows - 14) / 3).clamp(1, 7) as usize;
        let preview_y = y + files as i32 + 2;
        Self { split, y, step, buildings, files, preview_y, preview_rows: (l.rows - preview_y - 3).max(1), width: (l.cols - split - 4).max(1) }
    }
}

/// Scrollable sector roots and manually charted worlds.
pub struct FoldersLayout { pub start: usize, pub page: usize }
impl FoldersLayout {
    pub fn new(sel: usize, l: &Lay) -> Self {
        let page = ((l.rows - 9) / 2).max(1) as usize;
        Self { start: sel.saturating_sub(page - 1), page }
    }
}

fn folders(fb: &mut Fb, app: &App, sel: usize, l: &Lay, t: f32) {
    fb.px.fill(PANEL);
    let accent = v3(0.5, 0.85, 1.0);
    fb.panel(0, 0, l.cols, 5, BAR);
    seg(fb, 2, 0, "‹ Esc home", l.cols - 4, DIM, Some(BAR));
    seg(fb, 2, 2, "SECTORS & FOLDERS", l.cols - 4, TEXT, Some(BAR));
    seg(fb, 2, 4, "Sector roots discover Git projects below them.", l.cols - 4, accent, Some(BAR));
    let sources = app.workspace.sources();
    let sel = sel.min(sources.len().saturating_sub(1));
    let layout = FoldersLayout::new(sel, l);
    for (i, (root, path)) in sources.iter().enumerate().skip(layout.start).take(layout.page) {
        let y = 6 + (i - layout.start) as i32 * 2;
        let bg = if i == sel { SELBG } else { PANEL };
        fb.panel(1, y, l.cols - 2, 2, bg);
        seg(fb, 2, y, if i == sel { "›" } else { " " }, 1, accent, Some(bg));
        seg(fb, 4, y, &format!("{} · {}", if *root { "Sector root" } else { "Planet" }, base_name(path)), l.cols - 6, TEXT, Some(bg));
        seg(fb, 4, y + 1, &tilde(path), l.cols - 6, DIM, Some(bg));
    }
    if sources.is_empty() { seg(fb, 2, 6, "n adds your first sector root", l.cols - 4, TEXT, None); }
    seg(fb, 2, l.rows - 3, "Removal keeps files and sessions.", l.cols - 4, DIM, None);
    fb.panel(0, l.rows - 2, l.cols, 2, BAR);
    seg(fb, 2, l.rows - 2, "n sector · x remove · Esc home", l.cols - 4, TEXT, Some(BAR));
    let note = if app.discovering.is_some() { "Discovering repositories…".into() }
        else { format!("{} roots · {} folders · ↑↓ select", app.workspace.discovery_roots().len(), app.workspace.folders.len()) };
    seg(fb, 2, l.rows - 1, &note, l.cols - 4, DIM, Some(BAR));
    if let Some((msg, when)) = &app.toast && t - when < 3.2 { seg(fb, 2, l.rows - 3, msg, l.cols - 4, accent, Some(PANEL)); }
}

/// Visible workspace rows; headers and notes share the same geometry for drawing and clicks.
pub struct WorkspaceLayout {
    pub width: i32,
    pub items: Vec<(usize, i32, i32)>,
    headings: Vec<(i32, String)>,
}

impl WorkspaceLayout {
    pub fn new(app: &App, rows: &[(String, String, WorkAct)], key: &str, sel: usize, l: &Lay) -> Self {
        let mut layout = Self { width: if l.cols >= 105 { l.cols * 2 / 3 } else { l.cols }, items: vec![], headings: vec![] };
        let mut off = 0;
        loop {
            layout.items.clear(); layout.headings.clear();
            let mut y = 7;
            let mut group = None;
            for (i, (_, _, action)) in rows.iter().enumerate().skip(off) {
                let next = Some(app.work_group(key, action));
                let h = if matches!(action, WorkAct::Agent(_)) || matches!(action, WorkAct::Project(p) if app.home_need(p).is_some()) { 2 } else { 1 };
                let heading = next != group;
                let gap = if heading { if group.is_some() { 2 } else { 1 } } else { 0 };
                if y + gap + h > l.rows - 3 { break; }
                if heading { y += gap; layout.headings.push((y - 1, next.clone().unwrap())); group = next; }
                layout.items.push((i, y, h)); y += h;
            }
            if layout.items.iter().any(|(i, _, _)| *i == sel) || off >= sel { break; }
            off += 1;
        }
        layout
    }
}

/// A planet's sessions and everyday actions, with room for its landscape.
fn workspace(fb: &mut Fb, app: &App, w: &World, key: &str, sel: usize, l: &Lay, t: f32) {
    fb.px.fill(PANEL);
    let home = key == crate::workspace::HOME;
    let accent = if home { v3(0.5, 0.85, 1.0) } else { v3(0.65, 0.9, 0.75) };
    fb.panel(0, 0, l.cols, 6, BAR);
    seg(fb, 2, 0, "‹ Esc galaxy", l.cols - 4, DIM, Some(BAR));
    seg(fb, 2, 2, &format!("{}  {}", if home { "⌂" } else { "◉" }, if home { "HOME".into() } else { base_name(key) }), l.cols - 4, TEXT, Some(BAR));
    let summary = if home {
        let ships = app.agents.iter().filter(|a| a.kind != "shell" && a.dying.is_none()).count();
        let waiting = app.agents.iter().filter(|a| a.kind != "shell" && a.status.needs_you() && a.dying.is_none()).count();
        let terminals = app.agents.iter().filter(|a| a.kind == "shell" && a.dying.is_none()).count();
        let mut parts = vec![format!("{} projects", w.planets.iter().filter(|p| p.key != key).count())];
        if ships > 0 { parts.push(format!("{ships} ships")); }
        if waiting > 0 { parts.push(format!("{waiting} need you")); }
        if terminals > 0 { parts.push(format!("{terminals} terminals")); }
        parts.join(" · ")
    } else if let Some(s) = app.survey.worlds.get(key).filter(|s| s.git) {
        format!("{} · {} changed · ↑{} unpushed · ↓{} behind", s.branch, s.dirty, s.ahead, s.behind)
    } else { tilde(key) };
    seg(fb, 2, 4, &summary, l.cols - 4, accent, Some(BAR));
    let rows = app.work_rows(w, key);
    let sel = sel.min(rows.len().saturating_sub(1));
    let layout = WorkspaceLayout::new(app, &rows, key, sel, l);
    let width = layout.width;
    let output = rows.get(sel).and_then(|(_, _, a)| if let WorkAct::Agent(id) = a { app.outputs.get(id) } else { None });
    if width < l.cols && output.is_some() {
        seg(fb, width + 2, 7, "LATEST TERMINAL OUTPUT", l.cols - width - 4, accent, None);
        for (i, line) in wrap(output.unwrap(), (l.cols - width - 4).max(1) as usize).iter().take((l.rows - 12).max(0) as usize).enumerate() {
            seg(fb, width + 2, 9 + i as i32, line, l.cols - width - 4, TEXT, None);
        }
    } else if width < l.cols && let Some(p) = w.planet(key) {
        let x = (width + (l.cols - width) / 2) as f32 * 2.0;
        let y = l.rows as f32 * 2.1;
        let radius = ((l.cols - width) as f32 * 0.75).min(l.rows as f32 * 1.3);
        sphere(fb, app, x, y, radius, p, (-1.0, -0.5), t);
        seg(fb, width + 2, l.rows - 6, if home { "Your fleet starts here." } else { "A world of your own." }, l.cols - width - 4, DIM, None);
    }
    for (y, heading) in &layout.headings {
        seg(fb, 4, *y, heading, width - 6, if heading == "NEEDS YOU" { v3(1.0, 0.78, 0.45) } else { accent * 0.8 }, None);
    }
    for &(i, y, height) in &layout.items {
        let (title, note, action) = &rows[i];
        let bg = if i == sel { SELBG } else { PANEL };
        fb.panel(1, y, width - 2, height, bg);
        seg(fb, 2, y, if i == sel { "›" } else { " " }, 1, accent, Some(bg));
        if home {
            let need = if let WorkAct::Project(p) = action { app.home_need(p) } else { None };
            let warn = need.filter(|a| a.status == Status::Done).and_then(|a| a.outcome.as_ref()).and_then(|o| o.warn());
            let label = need.map_or_else(|| if matches!(action, WorkAct::Project(_)) { note.as_str() } else { "" }, |a| if a.status == Status::Blocked { "Needs input" } else { warn.map_or("Ready to check", |w| w.text) });
            let color = warn.map_or_else(|| need.map_or(DIM, |a| a.status.color()), |w| warn_color(w.red));
            let size = (label.chars().count() as i32).min((width - 10) / 2).max(0);
            seg(fb, 4, y, title, width - 7 - if size > 0 { size + 2 } else { 0 }, if i == sel { accent } else { TEXT }, Some(bg));
            seg(fb, width - 3 - size, y, label, size, color, Some(bg));
            if height == 2 { seg(fb, 6, y + 1, note, width - 9, DIM, Some(bg)); }
        } else if let WorkAct::Edit(kind, _) = action {
            seg(fb, 4, y, &kind.glyph().to_string(), 1, kind.color(), Some(bg));
            seg(fb, 6, y, title, width - 8, if i == sel { accent } else { TEXT }, Some(bg));
        } else {
            seg(fb, 4, y, title, width - 6, if i == sel { accent } else { TEXT }, Some(bg));
            if height == 2 { seg(fb, 4, y + 1, note, width - 6, DIM, Some(bg)); }
        }
    }
    fb.panel(0, l.rows - 2, l.cols, 2, BAR);
    let terminal = rows.get(sel).is_some_and(|r| matches!(r.2, WorkAct::Agent(id) if app.agent(id).is_some_and(|a| a.kind == "shell")));
    seg(fb, 2, l.rows - 2, if home { "Enter open · t task · b terminal · e agent · p PRs · x remove · n folder · r sectors · ^R refresh · Esc galaxy" }
        else if terminal { "Enter reopen · b new terminal · x close · ↑↓ choose · H home · Esc galaxy" }
        else if rows.get(sel).is_some_and(|r| matches!(r.2, WorkAct::Edit(..))) { "Enter edit in your editor · ↑↓ choose · t task · b terminal · H home · Esc galaxy" }
        else { "↑↓ choose · Enter open · t task · b terminal · e agent · p PRs · a anomalies · x dismiss · H home" }, l.cols - 4, TEXT, Some(BAR));
    let context = rows.get(sel).map(|(_, note, a)| if home { if let WorkAct::Project(p) = a { tilde(p) } else { note.clone() } }
        else if terminal { "F12 / Ctrl-] returns here and keeps the terminal running.".into() } else { note.clone() });
    seg(fb, 2, l.rows - 1, context.as_deref().unwrap_or("Agents and terminals keep running when you leave."), l.cols - 4, DIM, Some(BAR));
    if let (Some((first, _, _)), Some((last, _, _))) = (layout.items.first(), layout.items.last()) && (*first > 0 || *last + 1 < rows.len()) {
        seg(fb, 4, l.rows - 3, &format!("{}–{} of {} · scroll for more", first + 1, last + 1, rows.len()), width - 6, DIM, None);
    }
    if let Some((msg, at)) = &app.toast && t - at < 3.2 { seg(fb, 2, 5, msg, l.cols - 4, accent, Some(BAR)); }
}

fn planet_view(fb: &mut Fb, app: &App, w: &World, v: &PlanetView, l: &Lay, t: f32) {
    fb.px.fill(PANEL);
    fb.panel(0, 0, l.cols, 4, BAR);
    let accent = KINDS[v.sel].color();
    seg(fb, 2, 0, "‹ Esc project", l.cols - 4, DIM, Some(BAR));
    seg(fb, 2, 1, &format!("PLANET  {}", base_name(&v.key)), l.cols - 17, TEXT, Some(BAR));
    seg(fb, 2, 2, &tilde(&v.key), l.cols - 17, DIM, Some(BAR));
    if let Some(p) = w.planet(&v.key) {
        for row in 0..4 {
            for col in (l.cols - 13).max(0)..l.cols { fb.txt[(row * l.cols + col) as usize] = None; }
        }
        sphere(fb, app, l.cols as f32 * 2.0 - 15.0, 8.0, 6.0, p, (-1.0, -0.5), t);
    }
    if l.cols < 48 || l.rows < 18 {
        seg(fb, 2, 6, "Enlarge the terminal to browse buildings.", l.cols - 4, TEXT, None);
        return;
    }
    let g = PlanetLayout::new(l);
    fb.panel(g.split, 4, 1, l.rows - 6, BAR);
    seg(fb, 2, 4, if v.focus == 0 { "▸ BUILDINGS" } else { "  BUILDINGS" }, g.split - 4, accent, None);
    let start = v.sel.saturating_sub(g.buildings - 1);
    for (i, k) in KINDS.iter().enumerate().skip(start).take(g.buildings) {
        let y = g.y + (i - start) as i32 * g.step;
        let built = v.built.iter().find(|b| b.kind == *k);
        let bg = if i == v.sel { SELBG } else { PANEL };
        fb.panel(1, y, g.split - 2, g.step, bg);
        let mark = if built.is_some() { k.glyph() } else { '·' };
        seg(fb, 2, y, &format!("{} {mark} {}", i + 1, k.name()), g.split - 4, if i == v.sel { k.color() } else { TEXT }, Some(bg));
        if g.step == 2 {
            seg(fb, 4, y + 1, k.what(), g.split - 6, DIM, Some(bg));
        }
    }
    if l.rows >= 30 {
        if let Some(p) = w.planet(&v.key) {
            let cy = (l.rows as f32 - 6.0) * 4.0;
            sphere(fb, app, g.split as f32, cy, 12.0, p, (-1.0, -0.5), t);
        }
    }
    let x = g.split + 2;
    let kind = KINDS[v.sel];
    let count = v.building().map_or("empty plot".into(), |b| format!("{} built", b.count));
    seg(fb, x, 4, &format!("{}{} · {count}", if v.focus == 1 { "▸ " } else { "" }, kind.what()), g.width, accent, None);
    let start = v.file.saturating_sub(g.files - 1);
    if v.files().is_empty() {
        seg(fb, x, g.y, "No files yet", g.width, DIM, None);
        seg(fb, x, g.y + 1, kind.about(), g.width, DIM, None);
    }
    for (i, path) in v.files().iter().enumerate().skip(start).take(g.files) {
        let y = g.y + (i - start) as i32;
        let bg = if i == v.file { SELBG } else { PANEL };
        fb.panel(x, y, g.width, 1, bg);
        // retain the filename when a skill's parent path is wider than the pane
        let label = path_tail(path, (g.width - 2) as usize);
        seg(fb, x, y, &format!("{} {label}", if i == v.file { '›' } else { ' ' }), g.width, TEXT, Some(bg));
    }
    let title = v.path().unwrap_or_else(|| "Construction plot".into());
    seg(fb, x, g.preview_y - 1, &format!("{}{}", if v.focus == 2 { "▸ " } else { "  " }, path_tail(&title, (g.width - 2) as usize)), g.width, accent, None);
    let lines = v.lines(g.width as usize);
    let scroll = v.scroll.min(lines.len().saturating_sub(g.preview_rows as usize));
    for (i, line) in lines.iter().skip(scroll).take(g.preview_rows as usize).enumerate() {
        seg(fb, x, g.preview_y + i as i32, line, g.width, TEXT, None);
    }
    seg(fb, x, l.rows - 3, &format!("{} · {} files · lines {}–{} / {}", if v.building().is_some() { "e edit · B renovate" } else { "Enter write · B construct" }, v.files().len(), scroll + 1, (scroll + g.preview_rows as usize).min(lines.len()), lines.len()), g.width, DIM, None);
    fb.panel(0, l.rows - 2, l.cols, 2, BAR);
    let keys = if l.cols >= 80 { "↑↓ choose · Tab pane · e edit · n new · B brief a ship · 1–8 building · Esc back" } else if l.cols >= 50 { "↑↓ choose · e edit · n new · B brief · Esc back" } else { "e edit · n new · Esc back" };
    seg(fb, 2, l.rows - 2, keys, l.cols - 4, TEXT, Some(BAR));
    seg(fb, 2, l.rows - 1, "PgUp/Dn read · ^R refresh · c comms · b terminal", l.cols - 4, DIM, Some(BAR));
    if let Some((msg, at)) = &app.toast && t - at < 3.2 {
        seg(fb, 2, 3, msg, l.cols - 4, v3(0.6, 0.95, 0.7), Some(BAR));
    }
}

/// A world's structures: what stands there, and what can still be built.
fn build_menu(fb: &mut Fb, app: &App, planet: &str, sel: usize, l: &Lay) {
    let (x, y, w, h) = centered(l, 84, KINDS.len() as i32 * 2 + 7);
    let accent = v3(1.0, 0.85, 0.45);
    frame_box(fb, x, y, w, h, &format!("⌂ {} STRUCTURES", base_name(planet).to_uppercase()), accent);
    seg(fb, x + 3, y + 2, "The AI resources this world has built for its agents. Pick one to build.", w - 6, DIM, Some(BOX));
    for (i, k) in KINDS.iter().enumerate() {
        let r = y + 4 + i as i32 * 2;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 2, r, w - 4, 2, bg);
        let built = app.structure(planet, *k);
        let c = seg(fb, x + 3, r, &format!(" {} ", i + 1), 3, BOX, Some(k.color() * if built.is_some() { 0.9 } else { 0.4 }));
        let c = seg(fb, c + 1, r, &format!("{} {:<13}", k.glyph(), k.name()), 16, if built.is_some() { k.color() } else { DIM }, Some(bg));
        seg(fb, c + 1, r, k.what(), 22, TEXT, Some(bg));
        let (state, what) = match built {
            Some(b) => (if b.count > 1 { format!("built ×{}", b.count) } else { "built".into() }, "renovate"),
            None => ("empty plot".into(), "construct"),
        };
        let tag = format!("{state} · {what}");
        seg(fb, x + w - 3 - tag.chars().count() as i32, r, &tag, 24, if built.is_some() { v3(0.4, 0.95, 0.6) } else { DIM }, Some(bg));
        let detail = built.map_or_else(|| k.about().to_string(), |b| b.files.join("  "));
        seg(fb, x + 8, r + 1, &clip(&detail, (w - 12) as usize), w - 12, DIM, Some(bg));
    }
    seg(fb, x + 3, y + h - 2, "1-8 / ⏎ draft a planet task · ↑↓ choose · esc close", w - 6, DIM, Some(BOX));
}

/// A world's mission log, grouped by status, world or station, or fleet order; open missions show in full.
fn mission_log(fb: &mut Fb, app: &App, world: &str, sel: usize, open: &HashSet<u32>, l: &Lay, t: f32) {
    let log = app.mission_log(world);
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
            if let Some(w) = m.outcome.as_ref().and_then(|o| o.warn()) {
                info.push(w.text.to_lowercase());
            }
            lines.push(Ln::More(i, info.join(" · "), DIM * 0.8));
        }
    }
    let (x, y, w, h) = centered(l, w, (l.rows - 6).min(lines.len() as i32 + 6).max(12));
    let under = log.iter().filter(|(_, m)| matches!(m.state.as_str(), "sent" | "active")).count();
    let queued = log.iter().filter(|(_, m)| m.state == "queued").count();
    let title = format!("★ {} MISSIONS · {under} under way · {queued} queued · {} done · by {}", base_name(world).to_uppercase(), log.len() - under - queued, app.log_sort.label());
    frame_box(fb, x, y, w, h, &title, accent);
    if log.is_empty() {
        seg(fb, x + 3, y + 2, "No missions here yet. Press t to give this world one, or r for what it needs.", w - 6, DIM, Some(BOX));
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
                // a done mission whose turn looked failed or half done wears a warning, never a good-news badge
                let warn = m.outcome.as_ref().and_then(|o| o.warn()).filter(|_| m.state == "done");
                let flag = warn.map_or(String::new(), |w| format!("{} {} · ", w.glyph, w.text.to_lowercase()));
                let right_w = right.chars().count() as i32 + flag.chars().count() as i32;
                let tw = (w - 8 - (c - x) - right_w).max(4);
                // an open mission's text is spelled out below
                let text = if open.contains(&m.id) { String::new() } else { clip(&m.text, tw as usize) };
                seg(fb, c, r, &text, tw, if m.state == "done" { DIM } else { TEXT }, Some(bg));
                seg(fb, x + w - 3 - right.chars().count() as i32, r, &right, 30, DIM, Some(bg));
                if let Some(wn) = warn { seg(fb, x + w - 3 - right_w, r, &flag, flag.chars().count() as i32, warn_color(wn.red), Some(bg)); }
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
/// The mission composer's frame, the height of its text, how many images it lists, and the
/// row its options start on: launch, its role and what that means, a row per ship here, queue.
fn brief_box(l: &Lay, text: &Line, images: usize, _ships: usize) -> ((i32, i32, i32, i32), i32, usize, i32) {
    let width = 80.min(l.cols - 2);
    let lines = (layout(&text.s, width - 8).last().map_or(0, |p| p.0) + 1).clamp(3, 8).min((l.rows - 12).max(1));
    let visible = images.min((l.rows - 12 - lines).max(0) as usize);
    let (x, y, w, h) = centered(l, width, 12 + lines + visible as i32);
    ((x, y, w, h), lines, visible, y + lines + 5 + visible as i32)
}

/// Where each role's name sits on the role row: (role index, first column, last column + 1).

/// What a click on the mission composer lands on.
pub enum BriefHit {
    Launch,
    /// Give it to the i-th ship here.
    Queue,
}

pub fn brief_hit(l: &Lay, text: &Line, images: usize, ships: usize, c: i32, r: i32) -> Option<BriefHit> {
    let ((x, _, w, _), _, _, row) = brief_box(l, text, images, ships);
    if c < x || c >= x + w {
        return None;
    }
    match r - row {
        0 => Some(BriefHit::Queue),
        1 => Some(BriefHit::Launch),
        _ => None,
    }
}

/// What Advisor suggests for the mission, under its text.

#[allow(clippy::too_many_arguments)]
fn brief(fb: &mut Fb, app: &App, planet: &str, text: &Line, kind: usize, launch: bool, attachments: &[crate::screenshots::Attachment], sending: bool, l: &Lay, t: f32) {
    let ((x, y, w, h), lines, visible, r) = brief_box(l, text, attachments.len(), 0);
    let accent = v3(1.0, 0.85, 0.4);
    frame_box(fb, x, y, w, h, &format!("TASK · {}", base_name(planet)), accent);
    seg(fb, x + 3, y + 2, "What should be done here?", w - 6, DIM, Some(BOX));
    area(fb, (x + 3, y + 4, w - 6, lines), text, accent, t);
    for (i, image) in attachments.iter().rev().take(visible).enumerate() {
        seg(fb, x + 3, y + lines + 4 + i as i32, &format!("▧ {} total · {}", attachments.len(), image.name), w - 6, DIM, Some(BOX));
    }
    seg(fb, x + 3, r, if launch { "○ Next available agent on this planet" } else { "● Next available agent on this planet" }, w - 6, if launch { DIM } else { TEXT }, Some(BOX));
    seg(fb, x + 3, r + 1, if launch { "● Launch from Home" } else { "○ Launch from Home" }, w - 6, if launch { TEXT } else { DIM }, Some(BOX));
    if launch { species(fb, app, x + 5, r + 2, kind); }
    let queue = app.missions.iter().filter(|m| m.cwd == planet && m.state == "queued").count();
    seg(fb, x + 3, r + 3, &format!("{queue} queued · working agents finish before taking another task"), w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 3, "Ctrl+V image · Ctrl+O attach path · Ctrl+X remove image", w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 2, if sending { "Sending…" } else { "Enter send · Alt+Enter newline · Tab destination · ↑↓ species · Esc" }, w - 6, DIM, Some(BOX));
}

/// Where each character of `s` lands when wrapped to `width` columns, as (row, column),
/// plus one more entry for the end of the text.
pub fn layout(s: &str, width: i32) -> Vec<(i32, i32)> {
    let width = width.max(1);
    let (mut r, mut c) = (0, 0);
    let mut out = Vec::new();
    for ch in s.chars() {
        if c >= width {
            (r, c) = (r + 1, 0);
        }
        out.push((r, c));
        if ch == '\n' {
            (r, c) = (r + 1, 0);
        } else {
            c += 1;
        }
    }
    if c >= width {
        (r, c) = (r + 1, 0);
    }
    out.push((r, c));
    out
}

/// A text area that wraps and keeps the cursor in view.
fn area(fb: &mut Fb, (x, y, width, rows): (i32, i32, i32, i32), text: &Line, accent: V3, t: f32) {
    let chars: Vec<char> = text.s.chars().collect();
    let pos = layout(&text.s, width);
    let cur = pos[text.cur.min(chars.len())];
    let off = (cur.0 - rows + 1).max(0);
    for (ch, &(r, c)) in chars.iter().zip(&pos) {
        if *ch != '\n' && r >= off && r < off + rows {
            fb.put_bg(x + c, y + r - off, *ch, TEXT, Some(BOX));
        }
    }
    if off > 0 {
        seg(fb, x - 2, y, "↑", 1, DIM, Some(BOX));
    }
    if fract(t * 1.6) < 0.6 {
        let ch = chars.get(text.cur).copied().filter(|&c| c != '\n').unwrap_or(' ');
        fb.put_bg(x + cur.1, y + cur.0 - off, ch, BOX, Some(accent));
    }
}

/// The ship card's frame: five lines about the ship, then its menu.
/// The ship card's info rows: a refit ordered for it gets a row of its own.
fn card_rows(app: &App, id: u32) -> i32 { 5 + app.agent(id).is_some_and(|a| a.refit.is_some()) as i32 }

fn ship_box(app: &App, id: u32, l: &Lay) -> (i32, i32, i32, i32) {
    centered(l, 84, app.ship_acts(id).len() as i32 + card_rows(app, id) + 7)
}

/// Where a click lands on the ship card: outside it (None), on menu item i, or elsewhere on it.
pub fn ship_hit(app: &App, id: u32, l: &Lay, c: i32, r: i32) -> Option<Option<usize>> {
    let (x, y, w, h) = ship_box(app, id, l);
    if c < x || c >= x + w || r < y || r >= y + h {
        return None;
    }
    let i = r - (y + 4 + card_rows(app, id));
    Some((i >= 0 && (i as usize) < app.ship_acts(id).len()).then_some(i as usize))
}

fn ship_card(fb: &mut Fb, app: &App, id: u32, sel: usize, l: &Lay, t: f32) {
    let Some(a) = app.agent(id) else { return };
    let (x, y, w, h) = ship_box(app, id, l);
    let accent = kind_color(&a.kind);
    let role = if a.role == Role::None { String::new() } else { format!(" {}", a.role.name().to_uppercase()) };
    let design = if a.design.is_empty() { String::new() } else { format!(" · {} design", a.design) };
    frame_box(fb, x, y, w, h, &format!("◆ {}{role} · {}{design}", a.kind.to_uppercase(), base_name(&a.cwd).to_uppercase()), accent);
    let iw = w - 6;
    let c = seg(fb, x + 3, y + 2, &format!("{} {}", a.status.glyph(t), a.status.label()), iw, a.status.color(), Some(BOX));
    seg(fb, c, y + 2, &format!(" · {} · {} turns", ago(t - a.since), a.turns), x + w - 3 - c, DIM, Some(BOX));
    let mission = app.missions.iter().filter(|m| m.agent == Some(id) && m.state != "done").map(|m| m.text.as_str()).last();
    let rep = &a.report;
    let mut stats: Vec<String> = Vec::new();
    stats.extend(rep.model.clone());
    stats.extend(rep.ctx.map(|c| format!("{c:.0}% context")));
    stats.extend(rep.cost.map(|c| format!("${c:.2}")));
    if rep.added.is_some() || rep.removed.is_some() {
        stats.push(format!("+{} −{}", rep.added.unwrap_or(0), rep.removed.unwrap_or(0)));
    }
    let mut lines = vec![
        ("task", if a.title.is_empty() { "—".to_string() } else { a.title.clone() }, TEXT),
        ("mission", mission.map_or("none".into(), |m| m.replace('\n', " ")), TEXT),
        ("world", tilde(&a.cwd), TEXT),
        ("report", if stats.is_empty() { "—".into() } else { stats.join(" · ") }, TEXT),
        ("outfit", outfit_line(a.role, &a.spec), TEXT),
    ];
    // a refit is shown apart from what's aboard: only "active" means it's running with it
    if let Some(r) = &a.refit {
        let (label, col) = refit_label(r);
        lines.push(("refit", label, col));
    }
    for (i, (k, v, col)) in lines.iter().enumerate() {
        let r = y + 3 + i as i32;
        seg(fb, x + 3, r, k, 9, DIM * 0.9, Some(BOX));
        seg(fb, x + 13, r, &clip(v, (iw - 10) as usize), iw - 10, *col, Some(BOX));
    }
    for (i, act) in app.ship_acts(id).iter().enumerate() {
        let r = y + 4 + card_rows(app, id) + i as i32;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 2, r, w - 4, 1, bg);
        let col = if *act == ShipAct::Decommission { v3(1.0, 0.4, 0.3) } else { accent };
        let c = seg(fb, x + 3, r, &format!(" {} ", i + 1), 3, BOX, Some(col * 0.9));
        seg(fb, c + 1, r, act.label(), iw - 4, TEXT, Some(bg));
    }
    seg(fb, x + 3, y + h - 2, "1-9 / ⏎ act · ↑↓ choose · esc / click outside closes", iw, DIM, Some(BOX));
}

/// What a ship is running with: its role, design, and orders and skills of its own.
fn outfit_line(role: Role, spec: &crate::proto::ShipSpec) -> String {
    let mut v = vec![if role == Role::None { "no role".to_string() } else { role.name().to_string() }];
    if !spec.design.is_empty() { v.push(format!("{} design", spec.design)); }
    if !spec.orders.trim().is_empty() { v.push("own orders".into()); }
    if !spec.skills.is_empty() { v.push(format!("skills {}", spec.skills.join(", "))); }
    v.join(" · ")
}

/// A refit's place, in words and colour: only "active" is aboard.
pub fn refit_label(r: &crate::proto::RefitState) -> (String, V3) {
    let what = refit_name(r);
    let note = |s: &str| if r.note.is_empty() { s.to_string() } else { r.note.clone() };
    match r.state.as_str() {
        "pending" => (format!("◌ PENDING {what}: restarts on its own session when idle"), v3(1.0, 0.75, 0.3)),
        "applying" => (format!("⟳ APPLYING {what}: relaunching on its own session"), v3(0.55, 0.85, 1.0)),
        "active" => (format!("✓ ACTIVE {what} aboard"), v3(0.5, 0.9, 0.6)),
        "failed" => (format!("✖ FAILED {what}: {}", note("old outfit kept")), v3(1.0, 0.4, 0.3)),
        "unsupported" => (format!("⊘ UNSUPPORTED {what}: {}", note("can't resume its session")), v3(0.65, 0.65, 0.7)),
        s => (format!("{s} {what}"), DIM),
    }
}

fn confirm(fb: &mut Fb, app: &App, id: u32, l: &Lay) {
    let (x, y, w, h) = centered(l, 60, 8);
    let accent = v3(1.0, 0.4, 0.3);
    let shell = app.agent(id).is_some_and(|a| a.kind == "shell");
    frame_box(fb, x, y, w, h, if shell { "CLOSE TERMINAL" } else { "✖ DECOMMISSION" }, accent);
    let who = app.agent(id).map_or("this agent".into(), |a| format!("{} on {}", a.kind, base_name(&a.cwd)));
    seg(fb, x + 3, y + 2, &if shell { format!("Close Terminal #{id}?") } else { format!("Decommission {who}?") }, w - 6, TEXT, Some(BOX));
    seg(fb, x + 3, y + 3, if shell { "Its shell and running commands will be stopped." } else { "Its process is terminated; unsaved work in it is lost." }, w - 6, DIM, Some(BOX));
    seg(fb, x + 3, y + h - 2, if shell { "y close terminal · any other key cancels" } else { "y decommission · any other key cancels" }, w - 6, accent, Some(BOX));
}

fn help(fb: &mut Fb, l: &Lay) {
    let lines = [
        ("GALAXY", ""),
        ("Enter / double click", "open project or native agent terminal"),
        ("t / b / e", "give planet a task / new terminal / launch agent"),
        ("c / 1-9", "comms with a transmission or ship / answer it"),
        ("x", "dismiss ship / remove planet (keeps its files)"),
        ("Tab / C", "next agent needing you / all attention"),
        ("H / n / V", "home planet / add a folder / sectors and folders"),
        ("R / A", "Research Station / anomalies"),
        ("p / click PR marker", "open this planet's pull requests"),
        ("L / S", "planet task history / situation log"),
        ("wasd / drag", "pan · ± / wheel zoom · z fit · f focus"),
        ("o / m / M", "outliner / alert sounds / music"),
        ("q", "leave; agents and terminals keep running"),
        ("PROJECT", ""),
        ("Enter / ↑↓ g G", "open / select action or session"),
        ("t / b / e", "quick task / new terminal / new agent"),
        ("a / x / Ctrl+R", "anomalies / dismiss session / refresh"),
        ("H / Esc", "home (r sectors, x remove project) / galaxy"),
        ("PROJECT RESOURCES", ""),
        ("Enter / e", "write the file in your editor ($EDITOR, else nvim)"),
        ("n / B / b", "lay a new one / brief a ship to build it / terminal"),
        ("TERMINAL", ""),
        ("Ctrl-] / F12", "return; all other keys go to the terminal"),
        ("QUICK TASK", ""),
        ("Enter / Alt+Enter", "send / new line"),
        ("Tab / ↑↓", "available agent or new ship / species"),
        ("Ctrl+V", "paste or attach screenshot"),
    ];
    let (x, y, w, h) = centered(l, 78, lines.len() as i32 + 4);
    let accent = v3(0.45, 0.85, 1.0);
    frame_box(fb, x, y, w, h, "◆ FLEET MANUAL", accent);
    for (k, (key, what)) in lines.iter().enumerate() {
        let r = y + 2 + k as i32;
        if what.is_empty() {
            seg(fb, x + 3, r, key, w - 6, accent, Some(BOX));
        } else {
            let c = seg(fb, x + 3, r, &pad(key, 21), 21, TEXT, Some(BOX));
            seg(fb, c + 1, r, what, w - 27, DIM, Some(BOX));
        }
    }
}

// ──────────────────────────────── comms ────────────────────────────────

fn comms(fb: &mut Fb, app: &App, l: &Lay, t: f32, id: u32) {
    let a = app.agents.iter().find(|a| a.id == id);
    let accent = a.map_or(DIM, |a| a.status.color());
    let title = match a {
        Some(a) if !a.run.is_empty() => format!("✎ WRITING {} @ {} · quit the editor to return", a.run.last().map_or("", |f| f.as_str()), base_name(&a.cwd)),
        Some(a) => format!("◆ COMMS ▸ {} @ {} · {}", a.kind, base_name(&a.cwd), a.status.label()),
        None => "◆ COMMS".into(),
    };
    frame_box(fb, 0, 1, l.cols, l.rows - 1, &title, accent);
    let hint = " Ctrl-] galaxy ";
    seg(fb, l.cols - hint.chars().count() as i32 - 2, 1, hint, l.cols, DIM, Some(BOX));
    if let Some(n) = term(fb, app, (1, 2, l.cols - 2, l.rows - 3), accent, t, true) {
        let m = format!(" ↑ {n} lines back · wheel down or type to return ");
        seg(fb, (l.cols - m.chars().count() as i32) / 2, l.rows - 1, &m, l.cols, accent, Some(BOX));
    }
}

/// The agent's terminal in a box; how far back it's scrolled, if it is.
fn term(fb: &mut Fb, app: &App, (x, y, iw, ih): (i32, i32, i32, i32), accent: V3, t: f32, cursor: bool) -> Option<usize> {
    fb.panel(x, y, iw, ih, TERM_BG);
    let Some((_, s)) = &app.screen else {
        let m = "establishing uplink…";
        seg(fb, x + (iw - m.chars().count() as i32) / 2, y + ih / 2, m, iw, accent * (0.6 + 0.4 * (t * 4.0).sin().abs()), Some(TERM_BG));
        return None;
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
                    fb.put_bg(x + col, y + r as i32, ch, fg, Some(bg));
                }
                col += 1;
            }
        }
    }
    if cursor && !s.hide_cursor && fract(t * 1.6) < 0.6 {
        let (cr, cc) = (s.cursor.0 as i32, s.cursor.1 as i32);
        if cr < ih && cc < iw {
            let i = (y + cr) as usize * fb.cols + (x + cc) as usize;
            let (ch, fg, bg) = fb.txt[i].map_or((' ', TERM_FG, TERM_BG), |c| (c.ch, c.fg, c.bg.unwrap_or(TERM_BG)));
            let ch = if ch == '\0' { ' ' } else { ch };
            fb.put_bg(x + cc, y + cr, ch, bg, Some(fg));
        }
    }
    (s.scroll > 0).then_some(s.scroll)
}

// ─────────────────────────────── commander ───────────────────────────────

fn commander_layout(l: &Lay, sel: usize) -> (i32, i32, i32, i32, usize, usize, usize) {
    let (x, y, w, h) = centered(l, 120, l.rows - 4);
    let detail = ((h - 7) / 3).clamp(2, 10) as usize;
    let room = ((h - detail as i32 - 6) / 2).max(1) as usize;
    (x, y, w, h, room, (sel + 1).saturating_sub(room), detail)
}

pub fn commander_hit(l: &Lay, count: usize, sel: usize, col: i32, row: i32) -> Option<usize> {
    let (x, y, w, _, room, off, _) = commander_layout(l, sel);
    if col < x + 2 || col >= x + w - 2 || row < y + 3 || row >= y + 3 + 2 * room as i32 { return None; }
    let i = off + ((row - y - 3) / 2) as usize;
    (i < count).then_some(i)
}

fn commander(fb: &mut Fb, app: &mut App, world: &World, l: &Lay) {
    let items = app.attention(world);
    let sel = app.attention_selection(&items);
    let id = items.get(sel).map_or(String::new(), |r| r.id.clone());
    if app.commander != id { app.commander_detail = 0; }
    app.commander = id;
    let (x, y, w, h, room, off, detail_h) = commander_layout(l, sel);
    let accent = v3(0.65, 0.85, 1.0);
    frame_box(fb, x, y, w, h, &format!("COMMANDER · {} need your attention", items.len()), accent);
    seg(fb, x + 3, y + 1, "All star systems · questions, approvals and work to inspect", w - 6, DIM, Some(BOX));
    if items.is_empty() {
        seg(fb, x + 3, y + 4, if app.online { "Nothing needs your attention. Your agents can keep working." } else { "Waiting for the daemon to reconnect." }, w - 6, TEXT, Some(BOX));
    }
    for (i, item) in items.iter().enumerate().skip(off).take(room) {
        let r = y + 3 + 2 * (i - off) as i32;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 2, r, w - 4, 2, bg);
        let col = match item.kind { "Input" | "Conflict" | "CI failed" => v3(1.0, 0.65, 0.4), "Finished" => v3(0.4, 0.9, 0.6), _ => accent };
        seg(fb, x + 3, r, &pad(item.kind, 11), 11, col, Some(bg));
        seg(fb, x + 14, r, &trunc(&item.title, (w - 17).max(1) as usize), w - 17, TEXT, Some(bg));
        seg(fb, x + 14, r + 1, &tilde(&item.world), w - 17, DIM, Some(bg));
    }
    if let Some(item) = items.get(sel) {
        let dy = y + h - detail_h as i32 - 3;
        let place = world.planets.iter().position(|p| p.key == item.world).and_then(|i| {
            let p = &world.planets[i];
            world.system_of(p.host.unwrap_or(i))
        }).map_or_else(|| tilde(&item.world), |s| format!("{} · {}", s.name, tilde(&item.world)));
        seg(fb, x + 3, dy, &format!("{} / {} · {place}", sel + 1, items.len()), w - 6, accent, Some(BOX));
        let detail = wrap(&item.detail, (w - 6).max(1) as usize);
        app.commander_detail = app.commander_detail.min(detail.len().saturating_sub(detail_h));
        let scroll = app.commander_detail;
        for (i, line) in detail.iter().skip(scroll).take(detail_h).enumerate() {
            seg(fb, x + 3, dy + 1 + i as i32, line, w - 6, TEXT, Some(BOX));
        }
        let action = match item.target {
            AttentionTarget::Agent(_) => "⏎ comms / answer",
            AttentionTarget::Anomaly(_) => "⏎ inspect anomaly",
            AttentionTarget::Needs => "⏎ world actions",
            AttentionTarget::Deploy => "⏎ deploy agent",
            AttentionTarget::Pr(_) => "⏎ open PR · t mission",
        };
        let hint = format!("↑↓ select · {action} · PgUp/Dn details · Esc galaxy");
        seg(fb, x + 3, y + h - 2, &hint, w - 6, DIM, Some(BOX));
    } else {
        seg(fb, x + 3, y + h - 2, "Esc back to galaxy", w - 6, DIM, Some(BOX));
    }
}

// ─────────────────────────────── sectors ───────────────────────────────

#[allow(clippy::too_many_arguments)]

fn sector_banner(fb: &mut Fb, app: &App, l: &Lay) {
    if let Some(s) = app.focused_sector() {
        let stats = app.sector_stats(s);
        let width = l.cols - l.pw - 4;
        seg(fb, 2, 1, &format!("▱ {} · {} worlds · {}", s.name, stats.worlds, stats.activity()), width, v3(0.65, 0.8, 1.0), Some(BOX));
        seg(fb, 2, 2, &format!("{} · V sectors · Esc clear", stats.work()), width, DIM, Some(BOX));
    }
}

// ─────────────────────────────── anomalies ───────────────────────────────

fn anomaly_layout(l: &Lay, sel: usize) -> (i32, i32, i32, i32, usize, usize, usize) {
    let (x, y, w, h) = centered(l, 120, l.rows - 2);
    let detail = ((h - 10) / 2).max(1) as usize;
    let room = (h - detail as i32 - 9).max(1) as usize;
    (x, y, w, h, room, (sel + 1).saturating_sub(room), detail)
}

pub fn anomaly_hit(l: &Lay, count: usize, sel: usize, col: i32, row: i32) -> Option<usize> {
    let (x, y, w, _, room, off, _) = anomaly_layout(l, sel);
    if col < x + 2 || col >= x + w - 2 || row < y + 5 || row >= y + 5 + room as i32 { return None; }
    let i = off + (row - y - 5) as usize;
    (i < count).then_some(i)
}

fn anomalies(fb: &mut Fb, app: &mut App, l: &Lay, id: u64, typing: bool) {
    let rows = app.anomaly_rows();
    let sel = rows.iter().position(|f| f.id == id).unwrap_or(0);
    let (x, y, w, h, room, off, detail_h) = anomaly_layout(l, sel);
    let accent = v3(0.8, 0.65, 1.0);
    frame_box(fb, x, y, w, h, &format!("⟡ ANOMALIES · {} findings", rows.len()), accent);
    let filter = &app.anomaly_filter;
    let place = filter.world.as_deref().or(filter.system.as_deref()).map(tilde).unwrap_or_else(|| "all systems".into());
    let place = filter.sector.as_ref().map_or_else(|| place.clone(), |(name, _)| format!("{name} sector · {place}"));
    let chips = format!("v {} · f {} · w world / s system: {place}", crate::anomaly::STATES[filter.state], crate::anomaly::kind_label(crate::anomaly::KINDS[filter.kind]));
    seg(fb, x + 2, y + 1, &chips, w - 4, DIM, Some(BOX));
    let search = if app.anomaly_error.is_empty() { format!("/ Search: {}{}", filter.query, if typing { "▏" } else { "" }) } else { app.anomaly_error.clone() };
    seg(fb, x + 2, y + 2, &search, w - 4, if typing { TEXT } else { DIM }, Some(BOX));
    seg(fb, x + 2, y + 3, "Status · source · world · finding", w - 4, DIM, Some(BOX));
    if rows.is_empty() {
        seg(fb, x + 2, y + 5, "No matching anomalies. x clears filters; v includes history.", w - 4, TEXT, Some(BOX));
    }
    for (i, f) in rows.iter().enumerate().skip(off).take(room) {
        let r = y + 5 + (i - off) as i32;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 1, r, w - 2, 1, bg);
        let text = format!("{} · {} · {} · #{} {}", f.state.label(), crate::anomaly::kind_label(&f.kind), base_name(&f.world), f.id, f.title);
        seg(fb, x + 2, r, &text, w - 4, if f.state.active() { TEXT } else { DIM }, Some(bg));
    }
    let dy = y + 6 + room as i32;
    let mut detail = Vec::new();
    if let Some(f) = rows.get(sel) {
        let mut lines = vec![format!("#{} {}", f.id, f.title), format!("World: {}", tilde(&f.world)), format!("System: {} · source: {}", tilde(&f.system), crate::anomaly::kind_label(&f.kind)),
            format!("Discovered {} ago · last observed {} ago", ago((survey::now() - f.first).max(0) as f32), ago((survey::now() - f.last).max(0) as f32))];
        lines.extend(f.evidence.lines().map(String::from));
        lines.push("History (newest first):".into());
        let mut history: Vec<_> = f.history.iter().collect();
        history.sort_by_key(|e| std::cmp::Reverse(e.at));
        lines.extend(history.iter().map(|e| format!("{} ago · {}", ago((survey::now() - e.at).max(0) as f32), e.note)));
        for line in lines { detail.extend(wrap(&line, (w - 4).max(1) as usize)); }
    }
    app.anomaly_detail = app.anomaly_detail.min(detail.len().saturating_sub(detail_h));
    for (i, line) in detail.iter().skip(app.anomaly_detail).take(detail_h).enumerate() {
        seg(fb, x + 2, dy + i as i32, line, w - 4, TEXT, Some(BOX));
    }
    seg(fb, x + 2, y + h - 3, "⏎ locate · t investigate · d defer · x dismiss · r resolve · o reopen", w - 4, accent, Some(BOX));
    seg(fb, x + 2, y + h - 2, "↑↓ select · PgUp/Dn evidence/history · / search · ⌫ reset · Esc galaxy", w - 4, DIM, Some(BOX));
}

// ─────────────────────────────── situation log ───────────────────────────────

/// What a click in the situation log view landed on.
pub enum SitHit {
    Row(usize),
    Kind(Option<Ev>),
}

const DETAIL_H: i32 = 7;

/// The view's box, and its list: first row, rows, and how far it's scrolled to show `sel`.
fn situation_layout(l: &Lay, sel: usize) -> (i32, i32, i32, i32, i32, usize, usize) {
    let (x, y, w, h) = centered(l, 120, l.rows - 6);
    let list_y = y + 5;
    let room = (h - 5 - DETAIL_H - 2).max(1) as usize;
    let off = (sel + 1).saturating_sub(room);
    (x, y, w, h, list_y, room, off)
}

/// The filter chips along the top: label, the kind it picks, and where it sits.
fn situation_chips(app: &App, x: i32) -> Vec<(String, Option<Ev>, i32)> {
    let mut c = x + 3;
    let mut out = Vec::new();
    let all = std::iter::once((format!("0 all {}", app.log.len()), None));
    let kinds = Ev::ALL.iter().enumerate().map(|(i, &k)| (format!("{} {} {}", i + 1, k.label(), app.log.iter().filter(|e| e.kind == k).count()), Some(k)));
    for (label, k) in all.chain(kinds) {
        let n = label.chars().count() as i32 + 2;
        out.push((label, k, c));
        c += n + 1;
    }
    out
}

pub fn situation_hit(app: &App, l: &Lay, sel: usize, col: i32, row: i32) -> Option<SitHit> {
    let (x, y, w, _, list_y, room, off) = situation_layout(l, sel);
    if row == y + 2 {
        return situation_chips(app, x)
            .into_iter()
            .find(|(label, _, c)| col >= *c && col < c + label.chars().count() as i32 + 2)
            .map(|(_, k, _)| SitHit::Kind(k));
    }
    if col < x + 2 || col >= x + w - 2 || row < list_y || row >= list_y + room as i32 {
        return None;
    }
    let i = off + (row - list_y) as usize;
    (i < app.situation().len()).then_some(SitHit::Row(i))
}

fn situation(fb: &mut Fb, app: &App, w: &World, l: &Lay, id: u64, typing: bool, t: f32) {
    let list = app.situation();
    let sel = list.iter().position(|e| e.id == id).unwrap_or(0);
    let (x, y, bw, h, list_y, room, off) = situation_layout(l, sel);
    let accent = v3(0.6, 0.85, 1.0);
    frame_box(fb, x, y, bw, h, &format!("◉ SITUATION LOG · {} of {} entries", list.len(), app.log.len()), accent);

    // filters: kind chips, then world and search
    for (label, k, c) in situation_chips(app, x) {
        let on = app.sit.kind == k;
        let n = label.chars().count() as i32 + 2;
        if c + n > x + bw - 3 {
            break;
        }
        seg(fb, c, y + 2, &format!(" {label} "), n, if on { TEXT } else { DIM }, Some(if on { SELBG } else { BOX }));
    }
    let mut c = x + 3;
    let world = app.sit.world.as_deref().map_or("every world".into(), base_name);
    c = seg(fb, c, y + 3, "world ", 6, DIM, Some(BOX));
    c = seg(fb, c, y + 3, &world, 30, if app.sit.world.is_some() { TEXT } else { DIM }, Some(BOX));
    c = seg(fb, c, y + 3, "   search ", 10, DIM, Some(BOX));
    let cursor = if typing && (t * 2.0).fract() < 0.5 { "▏" } else { "" };
    let query = if app.sit.query.is_empty() && !typing { "—".to_string() } else { format!("{}{cursor}", app.sit.query) };
    seg(fb, c, y + 3, &query, x + bw - 3 - c, if typing { TEXT } else if app.sit.query.is_empty() { DIM } else { accent }, Some(if typing { SELBG } else { BOX }));

    // the entries
    if list.is_empty() {
        let why = if app.log.is_empty() { "Nothing has happened yet." } else { "Nothing matches these filters. Press x to clear them." };
        seg(fb, x + 3, list_y, why, bw - 6, DIM, Some(BOX));
    }
    for (i, e) in list.iter().enumerate().skip(off).take(room) {
        let r = list_y + (i - off) as i32;
        let bg = if i == sel { SELBG } else { BOX };
        fb.panel(x + 2, r, bw - 4, 1, bg);
        let mut c = seg(fb, x + 3, r, &e.clock, 5, DIM * 0.8, Some(bg)) + 1;
        c = seg(fb, c, r, &pad(e.kind.label(), 9), 9, DIM, Some(bg));
        let place = e.world.as_deref().map(base_name).unwrap_or_default();
        c = seg(fb, c, r, &pad(&place, 17), 17, DIM, Some(bg));
        seg(fb, c, r, &trunc(&e.msg, (x + bw - 3 - c).max(0) as usize), x + bw - 3 - c, e.col, Some(bg));
    }

    // the selected entry, in full
    let dy = y + h - 2 - DETAIL_H;
    seg(fb, x + 2, dy, &"─".repeat(bw as usize - 4), bw - 4, accent * 0.3, Some(BOX));
    if let Some(e) = list.get(sel) {
        let mut rows: Vec<(String, Vec<(String, V3)>)> = Vec::new();
        for (i, line) in wrap(&e.msg, (bw - 17) as usize).into_iter().take(2).enumerate() {
            rows.push((if i == 0 { "entry" } else { "" }.into(), vec![(line, e.col)]));
        }
        let age = ago((survey::now() - e.at) as f32);
        rows.push(("when".into(), vec![(format!("{} · {age} ago · ", e.clock), TEXT), (e.kind.label().into(), DIM)]));
        let place = match &e.world {
            Some(p) => {
                let mut v = vec![(tilde(p), TEXT)];
                if let Some(h) = app.moon_host(p) {
                    v.push((format!("  ▣ station of {}", base_name(&h)), DIM));
                }
                if w.planet(p).is_none() {
                    v.push(("  no longer charted".into(), DIM));
                }
                v
            }
            None => vec![("—".into(), DIM)],
        };
        rows.push(("world".into(), place));
        let agent = match e.agent.map(|a| (a, app.agent(a))) {
            Some((_, Some(a))) => {
                let mut v = vec![(format!("{} #{} ", a.kind, a.id), kind_color(&a.kind)), (format!("{} {} ", a.status.glyph(t), a.status.label()), a.status.color())];
                if !a.title.is_empty() {
                    v.push((format!("· {}", a.title), DIM));
                }
                v
            }
            Some((id, None)) => vec![(format!("#{id} · has left"), DIM)],
            None => vec![("—".into(), DIM)],
        };
        rows.push(("agent".into(), agent));
        for (k, (label, parts)) in rows.iter().enumerate() {
            let r = dy + 1 + k as i32;
            seg(fb, x + 3, r, label, 9, DIM, Some(BOX));
            let mut c = x + 13;
            for (s, col) in parts {
                c = seg(fb, c, r, s, (x + bw - 3 - c).max(0), *col, Some(BOX));
            }
        }
    }
    let help = if typing {
        "type to search · ⏎ / esc done".to_string()
    } else if let Some((m, _)) = app.toast.as_ref().filter(|(_, at)| t - at < 3.2) {
        m.clone()
    } else {
        "↑↓ select · 0-7 / f kind · w this world · / search · ⌫ clear · ⏎ go to · c comms · esc close".into()
    };
    seg(fb, x + 3, y + h - 2, &help, bw - 6, DIM, Some(BOX));
}
