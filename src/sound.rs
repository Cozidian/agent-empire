//! Spacious grand-strategy UI cues: glassy FM bells, warm detuned synth pads and
//! brass, sub swells and filtered-noise whooshes, all in a long reverb, plus an
//! ambient score composed on the fly. Silent if there's no audio device.

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
    pub music_muted: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl Sound {
    pub fn start(music: bool) -> Option<Self> {
        let dev = cpal::default_host().default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        let fmt = cfg.sample_format();
        let cfg = cfg.config();
        let (tx, rx) = mpsc::channel();
        let muted = Arc::new(AtomicBool::new(false));
        let m = muted.clone();
        let music_muted = Arc::new(AtomicBool::new(!music));
        let mm = music_muted.clone();
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&dev, cfg, rx, m, mm, music),
            SampleFormat::I16 => build::<i16>(&dev, cfg, rx, m, mm, music),
            SampleFormat::U16 => build::<u16>(&dev, cfg, rx, m, mm, music),
            SampleFormat::I32 => build::<i32>(&dev, cfg, rx, m, mm, music),
            _ => None,
        }?;
        Some(Self { tx, muted, music_muted, _stream: stream })
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
    music_muted: Arc<AtomicBool>,
    music: bool,
) -> Option<cpal::Stream> {
    let ch = cfg.channels as usize;
    let mut synth = Synth::new(cfg.sample_rate as f32);
    // a fresh seed each session, so the score never plays the same way twice
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() ^ d.as_secs() as u32);
    let mut music = music.then(|| Music::new(cfg.sample_rate as f32, seed));
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
                    let (mut l, mut r) = synth.tick();
                    if let Some(music) = &mut music {
                        let (ml, mr) = music.output(music_muted.load(Relaxed));
                        l += ml;
                        r += mr;
                    }
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
    /// Detuned saws through two vocal formants: a distant wordless choir.
    Choir,
    /// A soft, flute-like lead: a sine with a whisper of overtones.
    Lead,
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
    /// Vibrato depth as a fraction of pitch, blooming in after the attack.
    vib: f32,
    /// How much of the voice feeds the score's echo.
    send: f32,
    ph: [f32; 3],
    mph: f32,
    svf: Svf,
    svf2: Svf,
    rng: u32,
}

impl Voice {
    fn new(wave: Wave, delay: f32, dur: f32, f0: f32, f1: f32, amp: f32) -> Self {
        Self { wave, t: -delay, dur, f0, f1, cut: (18000.0, 18000.0), att: 0.005, decay: 0.0, rel: 0.6, amp, pan: 0.0, vib: 0.0, send: 0.0, ph: [0.0, 0.33, 0.67], mph: 0.0, svf: Svf::default(), svf2: Svf::default(), rng: 0x2545_f491 ^ (f0 as u32).wrapping_mul(2_654_435_761) }
    }
    fn bell(note: f32, delay: f32, dur: f32, amp: f32) -> Self {
        Self::new(Wave::Bell { ratio: 3.5, index: 1.6 }, delay, dur, hz(note), hz(note), amp).decay(2.2).rel(0.85)
    }
    fn pad(note: f32, delay: f32, dur: f32, amp: f32) -> Self {
        Self::new(Wave::Pad { detune: 0.006 }, delay, dur, hz(note), hz(note), amp).att(0.25).rel(0.55)
    }
    /// A soft, warm pluck for arpeggios: harmonic FM that mellows as it rings.
    fn pluck(note: f32, dur: f32, amp: f32) -> Self {
        Self::new(Wave::Bell { ratio: 1.0, index: 1.1 }, 0.0, dur, hz(note), hz(note), amp).att(0.01).decay(1.4).rel(0.8)
    }
    fn vib(mut self, d: f32) -> Self {
        self.vib = d;
        self
    }
    fn send(mut self, s: f32) -> Self {
        self.send = s;
        self
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
        let mut f = self.f0 * (self.f1 / self.f0).powf(u);
        if self.vib > 0.0 {
            // phase offset by pan so a section of voices doesn't wobble in lockstep
            f *= 1.0 + self.vib * smooth(0.0, 1.0, self.t / (self.att + 0.8)) * ((self.t * 4.6 + self.pan) * TAU).sin();
        }
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
            Wave::Choir => {
                let mut x = 0.0;
                for (k, d) in [-0.005, 0.0, 0.005].into_iter().enumerate() {
                    self.ph[k] = (self.ph[k] + f * (1.0 + d) * dt) % 1.0;
                    x += 2.0 * self.ph[k] - 1.0;
                }
                // an open "ah": a warm body plus the first two vowel formants
                let (body, f1) = self.svf.run(x / 3.0, 720.0, 3.0, sr);
                let (_, f2) = self.svf2.run(x / 3.0, 1150.0, 4.0, sr);
                0.5 * body + 0.2 * f1 + 0.1 * f2
            }
            Wave::Lead => {
                self.ph[0] = (self.ph[0] + f * dt) % 1.0;
                let p = self.ph[0] * TAU;
                p.sin() + 0.18 * (2.0 * p).sin() + 0.05 * (3.0 * p).sin()
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
        (master(l, wl), master(r, wr))
    }
}

// ─────────────────────────────── the score ─────────────────────────────
//
// Composed on the fly: a slow walk through hopeful chord changes in a drifting key,
// coloured by textures that change every section, with a lead that improvises phrases
// from the scale. Seeded per session, so no two sittings sound the same.

/// What carries the harmony.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Bed {
    /// Warm filtered synth pads.
    Pad,
    /// A distant wordless choir over a soft pad bass.
    Choir,
    /// A slow-swelling string section with gentle vibrato.
    Strings,
}

/// A section's instrumentation. Arpeggio and lead join once the section has settled.
#[derive(Clone, Copy, Debug)]
struct Texture {
    /// Seconds per beat; every chord holds a four-beat bar.
    beat: f32,
    bed: Bed,
    /// Sparse, soft chimes now and then.
    chimes: bool,
    /// An echoing pluck arpeggio.
    arp: bool,
    /// An improvised melody on a soft, flute-like lead.
    lead: bool,
}

const TEXTURES: [Texture; 6] = [
    Texture { beat: 2.0, bed: Bed::Pad, chimes: true, arp: true, lead: false },
    Texture { beat: 2.0, bed: Bed::Pad, chimes: true, arp: false, lead: false },
    Texture { beat: 1.75, bed: Bed::Strings, chimes: false, arp: false, lead: true },
    Texture { beat: 2.4, bed: Bed::Choir, chimes: true, arp: false, lead: false },
    Texture { beat: 2.0, bed: Bed::Strings, chimes: false, arp: true, lead: false },
    Texture { beat: 2.2, bed: Bed::Choir, chimes: false, arp: false, lead: true },
];

/// Bright and warm modes, weighted towards the hopeful ones.
const MODES: [([i32; 7], u32); 4] = [
    ([0, 2, 4, 5, 7, 9, 11], 4), // major
    ([0, 2, 4, 6, 7, 9, 11], 3), // lydian
    ([0, 2, 3, 5, 7, 8, 10], 2), // aeolian
    ([0, 2, 3, 5, 7, 9, 10], 1), // dorian
];

/// Where each chord (a scale degree, 0 = home) likes to go next, with weights.
/// Chords that come out diminished in the current mode are skipped.
const MOVES: [&[(usize, u32)]; 7] = [
    &[(3, 3), (5, 3), (4, 2), (1, 1), (2, 1), (6, 1)],
    &[(4, 3), (0, 1), (3, 1)],
    &[(3, 3), (5, 2)],
    &[(0, 3), (4, 2), (1, 1), (5, 1), (6, 1)],
    &[(0, 3), (5, 2), (3, 1)],
    &[(3, 3), (1, 2), (4, 1), (0, 1), (6, 2)],
    &[(0, 3), (2, 1), (5, 1)],
];

/// Ping-pong delay: plucks and the lead echo back and forth across the stereo field.
struct Echo {
    buf: [Vec<f32>; 2],
    i: usize,
    damp: [f32; 2],
}

impl Echo {
    fn new(sr: f32) -> Self {
        let n = (sr * 0.55) as usize;
        Self { buf: [vec![0.0; n], vec![0.0; n]], i: 0, damp: [0.0; 2] }
    }

    fn run(&mut self, x: f32) -> (f32, f32) {
        let (yl, yr) = (self.buf[0][self.i], self.buf[1][self.i]);
        // each repeat darkens a little, like light fading with distance
        self.damp[0] += (yr - self.damp[0]) * 0.35;
        self.damp[1] += (yl - self.damp[1]) * 0.35;
        self.buf[0][self.i] = x + self.damp[0] * 0.5;
        self.buf[1][self.i] = self.damp[1] * 0.5;
        self.i = (self.i + 1) % self.buf[0].len();
        (yl, yr)
    }
}

fn master(dry: f32, wet: f32) -> f32 {
    ((dry + wet * 0.9) * 0.9).tanh() * 0.6
}

struct Music {
    sr: f32,
    voices: Vec<Voice>,
    hall: Hall,
    echo: Echo,
    /// Frames played since the score began.
    frame: u64,
    /// Frames left until the next half-beat step.
    wait: u64,
    /// Half-beat step within the bar (0..8).
    step: u64,
    rng: u32,
    gain: f32,
    /// Home note (MIDI, 45..57) and its mode's scale.
    tonic: i32,
    scale: [i32; 7],
    /// The current chord as a scale degree, and as bass plus voicing.
    degree: usize,
    chord: [f32; 5],
    texture: Texture,
    /// Bars into the current section, and its length.
    bar: u32,
    bars: u32,
    /// The lead's place in the scale (steps above the tonic), and beats until its note ends.
    mel: i32,
    held: f32,
}

impl Music {
    fn new(sr: f32, seed: u32) -> Self {
        let mut m = Self {
            sr, voices: Vec::with_capacity(48), hall: Hall::new(sr), echo: Echo::new(sr), frame: 0, wait: 0, step: 0,
            rng: seed | 1, gain: 0.0, tonic: 50, scale: MODES[0].0, degree: 0, chord: [0.0; 5], texture: TEXTURES[0], bar: 0, bars: 8, mel: 11, held: 0.0,
        };
        for _ in 0..4 {
            m.roll();
        }
        m.tonic = 45 + (m.roll() % 12) as i32;
        m.texture = TEXTURES[(m.roll() % TEXTURES.len() as u32) as usize];
        m.chord = m.voice(0);
        m
    }

    fn roll(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        // xorshift's low bits are its weakest, and most picks here are small moduli
        self.rng >> 8
    }

    /// True one time in `n`.
    fn chance(&mut self, n: u32) -> bool {
        self.roll() % n == 0
    }

    fn pick<T: Copy>(&mut self, options: &[(T, u32)]) -> T {
        let total: u32 = options.iter().map(|o| o.1).sum();
        let mut r = self.roll() % total;
        for &(t, w) in options {
            if r < w {
                return t;
            }
            r -= w;
        }
        options[0].0
    }

    /// A pitch `k` scale steps above the tonic (k may run past an octave).
    fn note(&self, k: i32) -> i32 {
        self.tonic + self.scale[k.rem_euclid(7) as usize] + 12 * k.div_euclid(7)
    }

    fn diminished(&self, d: usize) -> bool {
        (self.note(d as i32 + 4) - self.note(d as i32)) != 7
    }

    /// The chord on degree `d`: a low bass root, then third, fifth, seventh and ninth spread above it.
    fn voice(&self, d: usize) -> [f32; 5] {
        let k = d as i32;
        let root = self.note(k);
        // a ninth a semitone off the octave would grate, so double the root there instead
        let ninth = if (self.note(k + 8) - root - 12).rem_euclid(12) == 1 { root } else { self.note(k + 8) };
        let place = |n: i32, lo: i32| (lo + (n - lo).rem_euclid(12)) as f32;
        let mut v = [place(self.note(k + 2), 52), place(self.note(k + 4), 55), place(self.note(k + 6), 57), place(ninth, 60)];
        v.sort_by(|a, b| a.total_cmp(b));
        [place(root, 38), v[0], v[1], v[2], v[3]]
    }

    /// Move to the next bar: maybe a new section (texture, key, mode), then a new chord.
    fn next_bar(&mut self) {
        self.bar += 1;
        if self.bar >= self.bars {
            self.bar = 0;
            self.bars = [8, 12, 16][(self.roll() % 3) as usize];
            let old = self.texture.bed;
            for _ in 0..4 {
                self.texture = TEXTURES[(self.roll() % TEXTURES.len() as u32) as usize];
                if self.texture.bed != old {
                    break;
                }
            }
            if self.chance(2) {
                // modulate by a gentle step around the circle of fifths, staying in register
                let shift = self.pick(&[(7, 3), (5, 3), (2, 1), (-2, 1), (3, 1)]);
                self.tonic = 45 + (self.tonic + shift - 45).rem_euclid(12);
                self.scale = self.pick(&MODES);
                self.degree = 0;
                self.chord = self.voice(0);
                return;
            }
        }
        // home on the downbeat of each section, wandering elsewhere
        let moves: Vec<(usize, u32)> = MOVES[self.degree].iter().copied().filter(|&(d, _)| !self.diminished(d)).collect();
        self.degree = if self.bar == 0 || moves.is_empty() { 0 } else { self.pick(&moves) };
        self.chord = self.voice(self.degree);
    }

    /// Start whatever falls on this half-beat step.
    fn play(&mut self) {
        let s = self.step;
        if s == 0 {
            self.next_bar();
        }
        let (t, chord) = (self.texture, self.chord);
        let (bar, beat) = (t.beat * 4.0, t.beat);
        if s == 0 {
            for (i, &n) in chord.iter().enumerate() {
                let pan = i as f32 * 0.3 - 0.6;
                // the bass note always sits in a soft pad; the bed carries the voicing
                let v = match (t.bed, i) {
                    (Bed::Pad, _) | (_, 0) => Voice::pad(n, 0.0, bar * 1.5, 0.03).att(bar * 0.3).rel(0.55).cut(450.0, 850.0),
                    (Bed::Choir, _) => Voice::new(Wave::Choir, 0.0, bar * 1.6, hz(n), hz(n), 0.032).att(bar * 0.4).rel(0.5).vib(0.002),
                    (Bed::Strings, _) => Voice::new(Wave::Pad { detune: 0.008 }, 0.0, bar * 1.5, hz(n), hz(n), 0.024).att(bar * 0.35).rel(0.55).cut(600.0, 1500.0).vib(0.0025),
                };
                self.voices.push(v.pan(pan));
            }
            let sub = hz(chord[0] - 12.0);
            self.voices.push(Voice::new(Wave::Sine, 0.0, bar * 1.4, sub, sub, 0.025).att(2.0).rel(0.6));
        }
        let voicing = [chord[1], chord[2], chord[3], chord[4]];
        // the section opens on its bed alone; arpeggio and lead join after a few bars
        let settled = self.bar >= 4;
        if t.arp && settled {
            // a rising and falling figure with the odd breath left out
            let n = voicing[[0, 1, 2, 3, 2, 1, 3, 1][s as usize]] + 12.0;
            if !self.chance(5) {
                let pan = if s % 2 == 0 { -0.35 } else { 0.35 };
                self.voices.push(Voice::pluck(n, beat * 1.6, 0.014).pan(pan).send(0.6));
            }
        } else if t.chimes && s % 2 == 0 && self.chance(3) {
            let n = voicing[(self.roll() % 4) as usize] + 12.0;
            let pan = if self.chance(2) { -0.5 } else { 0.5 };
            // a gentle FM index and slow attack keep these soft and round rather than glassy
            self.voices.push(Voice::new(Wave::Bell { ratio: 3.5, index: 0.6 }, 0.0, beat * 2.75, hz(n), hz(n), 0.009).att(0.15).decay(0.65).rel(0.85).pan(pan).send(0.3));
        }
        self.held -= 0.5;
        if t.lead && settled && self.held <= 0.0 {
            self.sing(s, beat);
        }
        self.step = (s + 1) % 8;
    }

    /// The lead improvises: two-bar phrases with two bars of rest, stepping through the scale
    /// and landing on chord tones on the strong beats.
    fn sing(&mut self, s: u64, beat: f32) {
        if (self.bar / 2) % 2 == 1 {
            return;
        }
        let odds = match s {
            0 => 2,
            4 => 3,
            2 | 6 => 4,
            _ => 10,
        };
        if !self.chance(odds) {
            return;
        }
        // climb early in a phrase, settle back down towards its end
        let up = self.bar % 2 == 0;
        let step: i32 = self.pick(&[(1, 3), (2, 2), (-1, if up { 1 } else { 3 }), (-2, 1), (0, 1)]);
        self.mel = (self.mel + if up { step } else { -step.abs() }).clamp(7, 15);
        let mut n = self.note(self.mel);
        if s % 4 == 0 {
            // nearest chord tone on the strong beats
            let near = |c: f32| {
                let d = (c as i32 - n).rem_euclid(12);
                if d > 6 { d - 12 } else { d }
            };
            if let Some(d) = self.chord[1..].iter().map(|&c| near(c)).min_by_key(|d| d.abs()) {
                n += d;
            }
        }
        let len = self.pick(&[(1.0, 2), (1.5, 1), (2.0, 3), (3.0, 1)]);
        self.held = len;
        let d = len * beat;
        self.voices.push(Voice::new(Wave::Lead, 0.0, d + 1.2, hz(n as f32), hz(n as f32), 0.022).att(0.35).rel(d / (d + 1.2)).vib(0.004).send(0.4));
    }

    fn tick(&mut self) -> (f32, f32) {
        if self.wait == 0 {
            self.play();
            self.wait = (self.texture.beat * self.sr / 2.0).round() as u64;
        }
        self.wait -= 1;
        self.frame += 1;
        let sr = self.sr;
        let (mut l, mut r, mut send) = (0.0, 0.0, 0.0);
        for v in &mut self.voices {
            let s = v.tick(sr);
            l += s * (1.0 - v.pan);
            r += s * (1.0 + v.pan);
            send += s * v.send;
        }
        self.voices.retain(|v| !v.finished());
        let (el, er) = self.echo.run(send);
        let (l, r) = (l + el, r + er);
        let (wl, wr) = self.hall.run(l, r);
        (master(l, wl), master(r, wr))
    }

    fn output(&mut self, muted: bool) -> (f32, f32) {
        // A short ramp prevents clicks and silences the reverb too. Keep time
        // moving while muted, so unmuting resumes the current part of the score.
        let step = 1.0 / (self.sr * 0.03);
        self.gain = (self.gain + if muted { -step } else { step }).clamp(0.0, 1.0);
        let (l, r) = self.tick();
        (l * self.gain, r * self.gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_mute_silences_reverb_and_resumes_without_restarting() {
        for sr in [44100, 48000] {
            let mut music = Music::new(sr as f32, 7);
            for _ in 0..sr * 3 {
                music.output(false);
            }
            let mut previous = music.output(false);
            for _ in 0..sr / 20 {
                let sample = music.output(true);
                assert!((sample.0 - previous.0).abs().max((sample.1 - previous.1).abs()) < 0.03);
                previous = sample;
            }
            for _ in 0..sr {
                assert_eq!(music.output(true), (0.0, 0.0), "muting must silence the hall too");
            }
            let frame = music.frame;
            let mut energy = 0.0;
            for _ in 0..sr / 10 {
                let sample = music.output(false);
                assert!((sample.0 - previous.0).abs().max((sample.1 - previous.1).abs()) < 0.03);
                energy += sample.0 * sample.0 + sample.1 * sample.1;
                previous = sample;
            }
            assert_eq!(music.frame, frame + sr / 10);
            assert!(energy > 0.001, "unmuting must restore audible music");
        }
    }

    #[test]
    fn generated_score_flows_for_minutes_without_gaps_or_clipping() {
        let sr = 12000;
        for seed in [1, 0xdead_beef] {
            let mut music = Music::new(sr as f32, seed);
            let mut previous = (0.0_f32, 0.0_f32);
            let mut beds = Vec::new();
            for second in 0..12 * 60 {
                let mut energy = 0.0;
                for _ in 0..sr {
                    let (l, r) = music.tick();
                    assert!(l.is_finite() && r.is_finite());
                    assert!(l.abs().max(r.abs()) < 0.25, "background should stay quiet");
                    assert!((l - previous.0).abs().max((r - previous.1).abs()) < 0.08, "audible discontinuity at second {second}");
                    energy += l * l + r * r;
                    previous = (l, r);
                }
                assert!(energy / sr as f32 > 0.000001, "music fell silent at second {second}");
                assert!(music.voices.len() < 60, "voices must not pile up: {}", music.voices.len());
                if !beds.contains(&music.texture.bed) {
                    beds.push(music.texture.bed);
                }
            }
            assert!(beds.len() > 1, "seed {seed}: the texture should change over twelve minutes");
        }
    }

    #[test]
    fn generated_harmony_stays_consonant_and_in_register() {
        for seed in 1..200 {
            let mut m = Music::new(48000.0, seed);
            let mut keys = Vec::new();
            for _ in 0..400 {
                m.next_bar();
                assert!(!m.diminished(m.degree), "seed {seed}: a diminished chord slipped in");
                let [bass, v @ ..] = m.chord;
                assert!((38.0..50.0).contains(&bass), "seed {seed}: bass {bass} out of register");
                assert!(v.iter().all(|n| (52.0..72.0).contains(n)), "seed {seed}: voicing {v:?} out of register");
                assert!((45..57).contains(&m.tonic));
                if !keys.contains(&m.tonic) {
                    keys.push(m.tonic);
                }
            }
            assert!(keys.len() > 1, "seed {seed}: the key never moved");
        }
    }

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
