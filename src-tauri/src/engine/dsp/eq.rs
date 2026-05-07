//! Per-track 4-band EQ: low shelf, two peaks, high shelf.

use crate::engine::smoothed::SmoothedParam;

use super::biquad::{BiquadCoefs, BiquadKind, BiquadState};

const SMOOTH_MS: f32 = 25.0;

pub struct EqBand {
    pub kind: BiquadKind,
    pub freq: SmoothedParam,
    pub gain_db: SmoothedParam,
    pub q: SmoothedParam,
    pub bypassed: bool,
    pub left: BiquadState,
    pub right: BiquadState,
    pub coefs: BiquadCoefs,
}

impl EqBand {
    pub fn new(kind: BiquadKind, fs: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        Self {
            kind,
            freq: SmoothedParam::new(freq, fs, SMOOTH_MS),
            gain_db: SmoothedParam::new(gain_db, fs, SMOOTH_MS),
            q: SmoothedParam::new(q, fs, SMOOTH_MS),
            bypassed: false,
            left: BiquadState::default(),
            right: BiquadState::default(),
            coefs: BiquadCoefs::design(kind, fs, freq, q, gain_db),
        }
    }

    pub fn refresh(&mut self, fs: f32) {
        let f = self.freq.next();
        let g = self.gain_db.next();
        let q = self.q.next();
        self.coefs = BiquadCoefs::design(self.kind, fs, f, q, g);
    }

    #[inline]
    pub fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.bypassed {
            return (l, r);
        }
        let lo = self.left.process(&self.coefs, l);
        let ro = self.right.process(&self.coefs, r);
        (lo, ro)
    }
}

pub struct Eq4Band {
    pub bands: [EqBand; 4],
}

impl Eq4Band {
    pub fn new(fs: f32) -> Self {
        Self {
            bands: [
                EqBand::new(BiquadKind::LowShelf, fs, 100.0, 0.7, 0.0),
                EqBand::new(BiquadKind::Peak, fs, 400.0, 1.0, 0.0),
                EqBand::new(BiquadKind::Peak, fs, 2500.0, 1.0, 0.0),
                EqBand::new(BiquadKind::HighShelf, fs, 8000.0, 0.7, 0.0),
            ],
        }
    }

    pub fn refresh_block(&mut self, fs: f32) {
        for b in self.bands.iter_mut() {
            b.refresh(fs);
        }
    }

    #[inline]
    pub fn process_stereo(&mut self, mut l: f32, mut r: f32) -> (f32, f32) {
        for b in self.bands.iter_mut() {
            let (nl, nr) = b.process_stereo(l, r);
            l = nl;
            r = nr;
        }
        (l, r)
    }

    pub fn set_band(&mut self, idx: usize, freq: f32, gain_db: f32, q: f32) {
        if let Some(b) = self.bands.get_mut(idx) {
            b.freq.set_target(freq.clamp(20.0, 20_000.0));
            b.gain_db.set_target(gain_db);
            b.q.set_target(q.max(0.05));
        }
    }
}
