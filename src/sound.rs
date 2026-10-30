//! Robotic UI cues: formant "robot voice" chirps, R2-style warbles, data bursts,
//! all bitcrushed. Silent if there's no audio device.

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
}

impl Cue {
    /// Lower is more important; only the most important cue of a moment plays.
    pub fn rank(self) -> u8 {
        match self {
            Cue::Blocked => 0,
            Cue::Done => 1,
            Cue::Spawn => 2,
            Cue::Gone => 3,
        }
    }
}

pub const ALL: [Cue; 4] = [Cue::Done, Cue::Blocked, Cue::Spawn, Cue::Gone];

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
    let mut synth = Robo::new(cfg.sample_rate as f32);
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

/// Zero-delay-feedback state variable filter, bandpass output.
#[derive(Default, Clone, Copy)]
struct Bp {
    ic1: f32,
    ic2: f32,
}

impl Bp {
    fn run(&mut self, x: f32, fc: f32, q: f32, sr: f32) -> f32 {
        let g = (PI * fc.clamp(20.0, sr * 0.45) / sr).tan();
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let (a2, a3) = (g * a1, g * g * a1);
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v1
    }
}

/// Vowel formants (F1, F2) in Hz.
#[derive(Clone, Copy)]
enum V {
    A,
    E,
    I,
    O,
    U,
}

impl V {
    fn f(self) -> (f32, f32) {
        match self {
            V::A => (800.0, 1250.0),
            V::E => (420.0, 2000.0),
            V::I => (300.0, 2350.0),
            V::O => (480.0, 850.0),
            V::U => (330.0, 720.0),
        }
    }
}

#[derive(Clone, Copy)]
enum Wave {
    /// Buzzy source through morphing vowel formants: a little robot voice.
    Vox(V, V),
    /// Pure whistle (R2-style when given vibrato).
    Sine,
    /// Hard digital blip.
    Square,
}

#[derive(Clone, Copy)]
struct Voice {
    wave: Wave,
    /// Seconds; negative while waiting to start.
    t: f32,
    dur: f32,
    f0: f32,
    f1: f32,
    /// Vibrato (rate Hz, depth as a pitch ratio).
    vib: (f32, f32),
    /// Ring modulator frequency (0 = off) for a metallic edge.
    ring: f32,
    amp: f32,
    pan: f32,
    ph: f32,
    rph: f32,
    bp: [Bp; 2],
}

impl Voice {
    fn new(wave: Wave, delay: f32, dur: f32, f0: f32, f1: f32, amp: f32) -> Self {
        Self { wave, t: -delay, dur, f0, f1, vib: (0.0, 0.0), ring: 0.0, amp, pan: 0.0, ph: 0.0, rph: 0.0, bp: [Bp::default(); 2] }
    }
    fn vib(mut self, rate: f32, depth: f32) -> Self {
        self.vib = (rate, depth);
        self
    }
    fn ring(mut self, f: f32) -> Self {
        self.ring = f;
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
        // exponential glide plus vibrato
        let f = self.f0 * (self.f1 / self.f0).powf(u) * (1.0 + self.vib.1 * (TAU * self.vib.0 * self.t).sin());
        self.ph = (self.ph + f * dt) % 1.0;
        let env = (self.t / 0.004).min(1.0) * (1.0 - smooth(0.75, 1.0, u));
        let mut s = match self.wave {
            Wave::Sine => (self.ph * TAU).sin(),
            Wave::Square => {
                if self.ph < 0.5 {
                    0.6
                } else {
                    -0.6
                }
            }
            Wave::Vox(a, b) => {
                let ((a1, a2), (b1, b2)) = (a.f(), b.f());
                let k = smooth(0.0, 1.0, u);
                let src = 2.0 * self.ph - 1.0;
                let f1 = self.bp[0].run(src, a1 + (b1 - a1) * k, 6.0, sr);
                let f2 = self.bp[1].run(src, a2 + (b2 - a2) * k, 8.0, sr);
                (f1 + f2 * 0.7) * 2.6
            }
        };
        if self.ring > 0.0 {
            self.rph = (self.rph + self.ring * dt) % 1.0;
            s *= 0.4 + 0.6 * (self.rph * TAU).sin();
        }
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

struct Robo {
    sr: f32,
    voices: Vec<Voice>,
    rng: u32,
    hold: (f32, f32),
    n: u32,
    dl: Vec<f32>,
    dr: Vec<f32>,
    dpos: usize,
}

impl Robo {
    fn new(sr: f32) -> Self {
        let n = (sr * 0.16) as usize;
        Self { sr, voices: Vec::new(), rng: 0x9e37_79b9, hold: (0.0, 0.0), n: 0, dl: vec![0.0; n], dr: vec![0.0; n], dpos: 0 }
    }

    fn rnd(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    fn cue(&mut self, c: Cue) {
        use Wave::*;
        match c {
            // "bee-boop!" then a pleased little "dee-dah"
            Cue::Done => {
                self.voices.push(Voice::new(Square, 0.0, 0.06, 1320.0, 1480.0, 0.10).pan(-0.3));
                self.voices.push(Voice::new(Square, 0.075, 0.07, 1760.0, 2200.0, 0.10).pan(0.3));
                self.voices.push(Voice::new(Vox(V::I, V::E), 0.17, 0.11, 260.0, 340.0, 0.13).vib(30.0, 0.02));
                self.voices.push(Voice::new(Vox(V::E, V::A), 0.29, 0.16, 340.0, 300.0, 0.13).vib(30.0, 0.02));
            }
            // robot asking a question: "hm–hm?" plus an R2 warble
            Cue::Blocked => {
                self.voices.push(Voice::new(Sine, 0.0, 0.28, 1700.0, 2600.0, 0.10).vib(14.0, 0.12).pan(0.4));
                self.voices.push(Voice::new(Vox(V::U, V::O), 0.3, 0.14, 210.0, 190.0, 0.15).ring(55.0));
                self.voices.push(Voice::new(Vox(V::O, V::I), 0.47, 0.26, 190.0, 330.0, 0.15).ring(55.0));
                self.voices.push(Voice::new(Sine, 0.78, 0.12, 2400.0, 3200.0, 0.07).vib(20.0, 0.08).pan(-0.4));
            }
            // computer thinking: a burst of random-pitch data blips
            Cue::Spawn => {
                for i in 0..9 {
                    let f = 900.0 + self.rnd() * 2600.0;
                    let pan = self.rnd() * 1.4 - 0.7;
                    self.voices.push(Voice::new(Square, i as f32 * 0.028, 0.022, f, f, 0.25).pan(pan));
                }
            }
            // power-down sweep
            Cue::Gone => {
                self.voices.push(Voice::new(Square, 0.0, 0.45, 900.0, 70.0, 0.08).ring(37.0));
                self.voices.push(Voice::new(Vox(V::A, V::U), 0.05, 0.4, 220.0, 70.0, 0.1));
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

        // digital grit: ~12 kHz sample-and-hold, 6-bit-ish crush
        self.n = self.n.wrapping_add(1);
        if self.n % 4 == 0 {
            let q = 48.0;
            self.hold = ((l * q).round() / q, (r * q).round() / q);
        }
        let (l, r) = self.hold;

        // short ping-pong slapback for space
        let (yl, yr) = (self.dl[self.dpos], self.dr[self.dpos]);
        self.dl[self.dpos] = r * 0.35 + yr * 0.25;
        self.dr[self.dpos] = l * 0.35 + yl * 0.25;
        self.dpos = (self.dpos + 1) % self.dl.len();
        (((l + yl) * 0.6).tanh() * 0.5, ((r + yr) * 0.6).tanh() * 0.5)
    }
}
