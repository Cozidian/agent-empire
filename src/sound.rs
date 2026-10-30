//! Spacious grand-strategy UI cues: glassy FM bells, warm detuned synth pads and
//! brass, sub swells and filtered-noise whooshes, all in a long reverb. Silent if
//! there's no audio device.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Clone, Copy, Debug)]
pub enum Cue {
    /// An agent finished its turn.
    Done,
    /// An agent is waiting on you.
    Blocked,
    /// A new agent appeared.
    Spawn,
    /// An agent went away.
    Gone,
    /// A world reached a new development level.
    Fanfare,
}

impl Cue {
    /// Lower is more important; only the most important cue of a moment plays.
    pub fn rank(self) -> u8 {
        match self {
            Cue::Blocked => 0,
            Cue::Fanfare => 1,
            Cue::Done => 2,
            Cue::Spawn => 3,
            Cue::Gone => 4,
        }
    }
}

pub const ALL: [Cue; 5] = [Cue::Done, Cue::Blocked, Cue::Spawn, Cue::Gone, Cue::Fanfare];

pub struct Sound {
    tx: Sender<Cue>,
    pub muted: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl Sound {
    pub fn start() -> Option<Self> {
        let dev = cpal::default_host().default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        let fmt = cfg.sample_format();
        let cfg = cfg.config();
        let (tx, rx) = mpsc::channel();
        let muted = Arc::new(AtomicBool::new(false));
        let m = muted.clone();
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&dev, cfg, rx, m),
            SampleFormat::I16 => build::<i16>(&dev, cfg, rx, m),
            SampleFormat::U16 => build::<u16>(&dev, cfg, rx, m),
            SampleFormat::I32 => build::<i32>(&dev, cfg, rx, m),
            _ => None,
        }?;
        Some(Self { tx, muted, _stream: stream })
    }

    pub fn play(&self, c: Cue) {
        let _ = self.tx.send(c);
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    cfg: cpal::StreamConfig,
    rx: Receiver<Cue>,
    muted: Arc<AtomicBool>,
) -> Option<cpal::Stream> {
    let ch = cfg.channels as usize;
    let mut synth = Synth::new(cfg.sample_rate as f32);
    let stream = dev
        .build_output_stream(
            cfg,
            move |data: &mut [T], _| {
                while let Ok(c) = rx.try_recv() {
                    if !muted.load(Relaxed) {
                        synth.cue(c);
                    }
                }
                for frame in data.chunks_mut(ch) {
                    let (l, r) = synth.tick();
                    for (i, s) in frame.iter_mut().enumerate() {
                        *s = T::from_sample(if i % 2 == 0 { l } else { r });
                    }
                }
            },
            |_| {},
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

// ─────────────────────────────── synthesis ─────────────────────────────

/// Zero-delay-feedback state variable filter: (lowpass, bandpass).
#[derive(Default, Clone, Copy)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    fn run(&mut self, x: f32, fc: f32, q: f32, sr: f32) -> (f32, f32) {
        let g = (PI * fc.clamp(20.0, sr * 0.45) / sr).tan();
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let (a2, a3) = (g * a1, g * g * a1);
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1)
    }
}

/// A MIDI note's frequency.
fn hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

#[derive(Clone, Copy)]
enum Wave {
    /// FM bell: a modulator at `ratio` × pitch, its `index` fading as the note rings.
    Bell { ratio: f32, index: f32 },
    /// Three detuned saws through a lowpass: pads, and brass when the filter opens.
    Pad { detune: f32 },
    /// Filtered noise: whooshes and air.
    Noise,
    Sine,
}

#[derive(Clone, Copy)]
struct Voice {
    wave: Wave,
    /// Seconds; negative while waiting to start.
    t: f32,
    dur: f32,
    /// Pitch (or noise band) glides exponentially from f0 to f1.
    f0: f32,
    f1: f32,
    /// Lowpass cutoff, gliding from the first to the second.
    cut: (f32, f32),
    att: f32,
    /// Exponential decay rate per second (0 = holds).
    decay: f32,
    /// Fraction of `dur` where the release begins.
    rel: f32,
    amp: f32,
    pan: f32,
    ph: [f32; 3],
    mph: f32,
    svf: Svf,
    rng: u32,
}

impl Voice {
    fn new(wave: Wave, delay: f32, dur: f32, f0: f32, f1: f32, amp: f32) -> Self {
        Self { wave, t: -delay, dur, f0, f1, cut: (18000.0, 18000.0), att: 0.005, decay: 0.0, rel: 0.6, amp, pan: 0.0, ph: [0.0, 0.33, 0.67], mph: 0.0, svf: Svf::default(), rng: 0x2545_f491 ^ (f0 as u32).wrapping_mul(2_654_435_761) }
    }
    fn bell(note: f32, delay: f32, dur: f32, amp: f32) -> Self {
        Self::new(Wave::Bell { ratio: 3.5, index: 1.6 }, delay, dur, hz(note), hz(note), amp).decay(2.2).rel(0.85)
    }
    fn pad(note: f32, delay: f32, dur: f32, amp: f32) -> Self {
        Self::new(Wave::Pad { detune: 0.006 }, delay, dur, hz(note), hz(note), amp).att(0.25).rel(0.55)
    }
    fn att(mut self, a: f32) -> Self {
        self.att = a;
        self
    }
    fn decay(mut self, k: f32) -> Self {
        self.decay = k;
        self
    }
    fn rel(mut self, r: f32) -> Self {
        self.rel = r;
        self
    }
    fn cut(mut self, c0: f32, c1: f32) -> Self {
        self.cut = (c0, c1);
        self
    }
    fn pan(mut self, p: f32) -> Self {
        self.pan = p;
        self
    }

    fn tick(&mut self, sr: f32) -> f32 {
        let dt = 1.0 / sr;
        self.t += dt;
        if self.t < 0.0 {
            return 0.0;
        }
        let u = (self.t / self.dur).min(1.0);
        let f = self.f0 * (self.f1 / self.f0).powf(u);
        let env = smooth(0.0, 1.0, self.t / self.att) * (-self.decay * self.t).exp() * (1.0 - smooth(self.rel, 1.0, u));
        let s = match self.wave {
            Wave::Bell { ratio, index } => {
                self.ph[0] = (self.ph[0] + f * dt) % 1.0;
                self.mph = (self.mph + f * ratio * dt) % 1.0;
                let m = (self.mph * TAU).sin() * index * (-self.t * 4.0).exp();
                // a quieter octave partial for shimmer
                self.ph[1] = (self.ph[1] + f * 2.0 * dt) % 1.0;
                (self.ph[0] * TAU + m).sin() + 0.15 * (self.ph[1] * TAU).sin()
            }
            Wave::Pad { detune } => {
                let mut x = 0.0;
                for (k, d) in [-detune, 0.0, detune].into_iter().enumerate() {
                    self.ph[k] = (self.ph[k] + f * (1.0 + d) * dt) % 1.0;
                    x += 2.0 * self.ph[k] - 1.0;
                }
                let fc = self.cut.0 * (self.cut.1 / self.cut.0).powf(smooth(0.0, 1.0, u));
                self.svf.run(x / 3.0, fc, 0.9, sr).0
            }
            Wave::Noise => {
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 17;
                self.rng ^= self.rng << 5;
                let n = (self.rng >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                self.svf.run(n, f, 2.5, sr).1
            }
            Wave::Sine => {
                self.ph[0] = (self.ph[0] + f * dt) % 1.0;
                (self.ph[0] * TAU).sin()
            }
        };
        s * env * self.amp
    }

    fn finished(&self) -> bool {
        self.t > self.dur
    }
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Feedback comb with a damped loop, Freeverb-style.
struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

impl Comb {
    fn run(&mut self, x: f32, fb: f32, damp: f32) -> f32 {
        let y = self.buf[self.i];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.i] = x + self.store * fb;
        self.i = (self.i + 1) % self.buf.len();
        y
    }
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

impl Allpass {
    fn run(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        self.buf[self.i] = x + b * 0.5;
        self.i = (self.i + 1) % self.buf.len();
        b - x
    }
}

/// A large, dark hall: the space everything rings out into.
struct Hall {
    combs: [Vec<Comb>; 2],
    aps: [Vec<Allpass>; 2],
}

impl Hall {
    fn new(sr: f32) -> Self {
        let k = sr / 44100.0;
        let len = |n: usize| ((n as f32 * k) as usize).max(1);
        let combs = |spread: usize| [1557usize, 1617, 1491, 1422, 1277, 1356].iter().map(|&n| Comb { buf: vec![0.0; len(n + spread)], i: 0, store: 0.0 }).collect();
        let aps = |spread: usize| [556usize, 441, 341].iter().map(|&n| Allpass { buf: vec![0.0; len(n + spread)], i: 0 }).collect();
        Self { combs: [combs(0), combs(23)], aps: [aps(0), aps(23)] }
    }

    fn run(&mut self, l: f32, r: f32) -> (f32, f32) {
        let x = (l + r) * 0.08;
        let mut out = [0.0; 2];
        for c in 0..2 {
            let mut y: f32 = self.combs[c].iter_mut().map(|cb| cb.run(x, 0.88, 0.35)).sum();
            for ap in &mut self.aps[c] {
                y = ap.run(y);
            }
            out[c] = y;
        }
        (out[0], out[1])
    }
}

struct Synth {
    sr: f32,
    voices: Vec<Voice>,
    hall: Hall,
}

impl Synth {
    fn new(sr: f32) -> Self {
        Self { sr, voices: Vec::new(), hall: Hall::new(sr) }
    }

    fn cue(&mut self, c: Cue) {
        use Wave::*;
        let v = &mut self.voices;
        match c {
            // a rising fifth of glass bells over a soft swell: task complete
            Cue::Done => {
                v.push(Voice::bell(76.0, 0.0, 2.2, 0.16).pan(-0.25));
                v.push(Voice::bell(83.0, 0.14, 2.6, 0.13).pan(0.25));
                v.push(Voice::pad(52.0, 0.0, 2.0, 0.07).cut(300.0, 900.0));
                v.push(Voice::pad(59.0, 0.05, 2.0, 0.05).cut(300.0, 900.0));
            }
            // incoming transmission: a doubled ping, then a low minor swell that asks for you
            Cue::Blocked => {
                v.push(Voice::bell(81.0, 0.0, 1.4, 0.15).pan(-0.2));
                v.push(Voice::bell(81.0, 0.2, 1.8, 0.13).pan(0.2));
                for (i, n) in [50.0, 53.0, 57.0].into_iter().enumerate() {
                    v.push(Voice::pad(n, 0.3 + i as f32 * 0.04, 2.0, 0.07).att(0.35).cut(250.0, 1800.0).pan(i as f32 * 0.3 - 0.3));
                }
                v.push(Voice::new(Sine, 0.3, 1.8, hz(38.0), hz(38.0), 0.16).att(0.3).rel(0.5));
            }
            // a ship launches: rising whoosh and sub, a high shimmer as it clears
            Cue::Spawn => {
                v.push(Voice::new(Noise, 0.0, 1.1, 250.0, 3500.0, 0.14).att(0.5).rel(0.7));
                v.push(Voice::new(Sine, 0.0, 1.1, 45.0, 95.0, 0.1).att(0.3).rel(0.6));
                v.push(Voice::bell(79.0, 0.75, 1.8, 0.07).pan(-0.4));
                v.push(Voice::bell(86.0, 0.85, 1.8, 0.06).pan(0.4));
            }
            // a ship leaves: a dark chord sinking away, wind trailing off
            Cue::Gone => {
                v.push(Voice::new(Pad { detune: 0.006 }, 0.0, 1.8, hz(48.0), hz(46.0), 0.08).att(0.05).cut(1400.0, 180.0).rel(0.4));
                v.push(Voice::new(Pad { detune: 0.006 }, 0.0, 1.8, hz(55.0), hz(53.0), 0.06).att(0.05).cut(1400.0, 180.0).rel(0.4));
                v.push(Voice::new(Noise, 0.0, 1.4, 2200.0, 220.0, 0.12).att(0.05).rel(0.4));
                v.push(Voice::bell(67.0, 0.05, 1.6, 0.05));
            }
            // a world grows: synth brass rising I–V–I, timpani under, bells on the landing
            Cue::Fanfare => {
                let brass = |n: f32, at: f32, dur: f32, amp: f32| Voice::new(Pad { detune: 0.004 }, at, dur, hz(n), hz(n), amp).att(0.06).cut(500.0, 3200.0).rel(0.7);
                for n in [60.0, 64.0, 67.0] {
                    v.push(brass(n, 0.0, 0.75, 0.06));
                }
                for n in [62.0, 67.0, 71.0] {
                    v.push(brass(n, 0.55, 0.75, 0.06));
                }
                for n in [48.0, 60.0, 64.0, 67.0, 72.0] {
                    v.push(brass(n, 1.1, 2.6, 0.06));
                }
                for at in [0.0, 1.1] {
                    v.push(Voice::new(Sine, at, 0.9, 95.0, 48.0, 0.3).decay(5.0));
                }
                v.push(Voice::bell(84.0, 1.15, 2.6, 0.07).pan(-0.3));
                v.push(Voice::bell(88.0, 1.25, 2.6, 0.06).pan(0.3));
            }
        }
    }

    fn tick(&mut self) -> (f32, f32) {
        let sr = self.sr;
        let (mut l, mut r) = (0.0, 0.0);
        for v in &mut self.voices {
            let s = v.tick(sr);
            l += s * (1.0 - v.pan);
            r += s * (1.0 + v.pan);
        }
        self.voices.retain(|v| !v.finished());
        let (wl, wr) = self.hall.run(l, r);
        (((l + wl * 0.9) * 0.9).tanh() * 0.6, ((r + wr * 0.9) * 0.9).tanh() * 0.6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cue_is_audible_unclipped_and_rings_out() {
        let sr = 48000.0;
        for c in ALL {
            let mut s = Synth::new(sr);
            s.cue(c);
            let mut peak: f32 = 0.0;
            for _ in 0..(sr * 4.0) as usize {
                let (l, r) = s.tick();
                assert!(l.is_finite() && r.is_finite(), "{c:?} went non-finite");
                peak = peak.max(l.abs()).max(r.abs());
            }
            assert!(peak > 0.05, "{c:?} is too quiet: {peak}");
            // the output stage tops out at 0.6; staying well under keeps it clean
            assert!(peak < 0.45, "{c:?} is overdriven: {peak}");
            // the hall tail fades well below the cue within a few more seconds
            let mut tail: f32 = 0.0;
            for _ in 0..(sr * 4.0) as usize {
                let (l, r) = s.tick();
                tail = tail.max(l.abs()).max(r.abs());
            }
            assert!(tail < peak * 0.05, "{c:?} rings on: {tail} after a {peak} peak");
        }
    }
}
