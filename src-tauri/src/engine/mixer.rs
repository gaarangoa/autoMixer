//! Audio-thread mixer state. Lives entirely on the cpal callback thread
//! after construction. No allocations or locks here.

use std::sync::{atomic::Ordering, Arc};

use crossbeam_channel::Sender;
use rtrb::Consumer;

use super::{
    commands::EngineCommand,
    dsp::{
        compressor::Compressor,
        delay::StereoDelay,
        eq::Eq4Band,
        filter::{CascadeFilter, FilterMode},
        limiter::Limiter,
        reverb::Reverb,
    },
    events::EngineEvent,
    shared::EngineShared,
    smoothed::{db_to_gain, SmoothedParam},
};

pub const MAX_TRACKS: usize = 64;
const SMOOTH_TIME_CONSTANT_MS: f32 = 10.0;

pub struct TrackVoice {
    pub active: bool,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: SmoothedParam,
    pub pan: SmoothedParam,
    pub reverb_send_db: SmoothedParam,
    pub delay_send_db: SmoothedParam,
    pub high_pass: CascadeFilter,
    pub low_pass: CascadeFilter,
    pub eq: Eq4Band,
    pub compressor: Compressor,
    /// Defaults used when no automation overrides them.
    pub default_gain_db: f32,
    pub default_pan: f32,
    pub default_reverb_send_db: f32,
    pub default_delay_send_db: f32,
    pub default_hp_freq: f32,
    pub default_lp_freq: f32,
}

impl TrackVoice {
    fn new(sample_rate: f32) -> Self {
        Self {
            active: false,
            muted: false,
            solo: false,
            gain_db: SmoothedParam::new(0.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            pan: SmoothedParam::new(0.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            reverb_send_db: SmoothedParam::new(-60.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            delay_send_db: SmoothedParam::new(-60.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            high_pass: CascadeFilter::new(FilterMode::HighPass, sample_rate, 40.0),
            low_pass: CascadeFilter::new(FilterMode::LowPass, sample_rate, 18_000.0),
            eq: Eq4Band::new(sample_rate),
            compressor: Compressor::new(sample_rate),
            default_gain_db: 0.0,
            default_pan: 0.0,
            default_reverb_send_db: -60.0,
            default_delay_send_db: -60.0,
            default_hp_freq: 40.0,
            default_lp_freq: 18_000.0,
        }
    }
}

pub struct Mixer {
    pub sample_rate: f32,
    pub channels: u16,
    pub commands: Consumer<EngineCommand>,
    pub events: Sender<EngineEvent>,
    pub shared: Arc<EngineShared>,
    pub tracks: Vec<TrackVoice>,
    pub master_gain_db: SmoothedParam,
    pub master_ceiling_db: SmoothedParam,
    pub playing: bool,
    pub playhead: f64,
    pub session_rate: u32,
    pub master_bypass: bool,
    pub reverb: Reverb,
    pub delay: StereoDelay,
    pub limiter: Limiter,
}

impl Mixer {
    pub fn new(
        sample_rate: f32,
        channels: u16,
        commands: Consumer<EngineCommand>,
        events: Sender<EngineEvent>,
        shared: Arc<EngineShared>,
    ) -> Self {
        let mut tracks = Vec::with_capacity(MAX_TRACKS);
        for _ in 0..MAX_TRACKS {
            tracks.push(TrackVoice::new(sample_rate));
        }
        Self {
            sample_rate,
            channels: channels.max(1),
            commands,
            events,
            shared,
            tracks,
            master_gain_db: SmoothedParam::new(0.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            master_ceiling_db: SmoothedParam::new(-1.0, sample_rate, SMOOTH_TIME_CONSTANT_MS),
            playing: false,
            playhead: 0.0,
            session_rate: sample_rate as u32,
            master_bypass: false,
            reverb: Reverb::new(sample_rate),
            delay: StereoDelay::new(sample_rate),
            limiter: Limiter::new(sample_rate),
        }
    }

    fn handle(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::SetMasterGainDb(db) => self.master_gain_db.set_target(db),
            EngineCommand::SetMasterCeilingDb(db) => {
                self.master_ceiling_db.set_target(db);
                self.limiter.set(db, 60.0);
            }
            EngineCommand::SetTrackGainDb { slot, db } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.default_gain_db = db;
                    t.gain_db.set_target(db);
                }
            }
            EngineCommand::SetTrackPan { slot, pan } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    let p = pan.clamp(-1.0, 1.0);
                    t.default_pan = p;
                    t.pan.set_target(p);
                }
            }
            EngineCommand::SetTrackMuted { slot, muted } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.muted = muted;
                }
            }
            EngineCommand::SetTrackSolo { slot, solo } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.solo = solo;
                }
            }
            EngineCommand::SetTrackActive { slot, active } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.active = active;
                }
            }
            EngineCommand::SetTrackHighPass { slot, enabled, frequency_hz, slope_db_oct } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.default_hp_freq = frequency_hz;
                    t.high_pass.set(enabled, frequency_hz, slope_db_oct);
                }
            }
            EngineCommand::SetTrackLowPass { slot, enabled, frequency_hz, slope_db_oct } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.default_lp_freq = frequency_hz;
                    t.low_pass.set(enabled, frequency_hz, slope_db_oct);
                }
            }
            EngineCommand::SetTrackEqBand { slot, band, frequency_hz, gain_db, q } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.eq.set_band(band as usize, frequency_hz, gain_db, q);
                }
            }
            EngineCommand::SetTrackCompressor {
                slot,
                enabled,
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                knee_db,
                makeup_db,
            } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.compressor.set(
                        enabled,
                        threshold_db,
                        ratio,
                        attack_ms,
                        release_ms,
                        knee_db,
                        makeup_db,
                    );
                }
            }
            EngineCommand::SetTrackReverbSendDb { slot, db } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.default_reverb_send_db = db;
                    t.reverb_send_db.set_target(db);
                }
            }
            EngineCommand::SetTrackDelaySendDb { slot, db } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.default_delay_send_db = db;
                    t.delay_send_db.set_target(db);
                }
            }
            EngineCommand::Play => {
                self.playing = true;
            }
            EngineCommand::Pause => {
                self.playing = false;
            }
            EngineCommand::Stop => {
                self.playing = false;
                self.playhead = 0.0;
                self.shared.playhead.store(0, Ordering::Relaxed);
            }
            EngineCommand::Seek { sample } => {
                self.playhead = sample as f64;
                self.shared.playhead.store(sample, Ordering::Relaxed);
            }
            EngineCommand::SetSessionRate { rate } => {
                if rate > 0 {
                    self.session_rate = rate;
                }
            }
            EngineCommand::SetMasterBypass { enabled } => {
                self.master_bypass = enabled;
            }
        }
    }

    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            self.handle(cmd);
        }
    }

    fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.active && t.solo)
    }

    pub fn render(&mut self, output: &mut [f32]) {
        self.drain_commands();
        let channels = self.channels as usize;
        let frames = output.len() / channels;

        if !self.playing {
            for s in output.iter_mut() {
                *s = 0.0;
            }
            return;
        }

        let any_solo = self.any_solo();
        let fs = self.sample_rate;
        let session_rate = self.session_rate.max(1) as f64;
        let session_per_output = session_rate / self.sample_rate as f64;

        // Apply automation overlays at the block start. Each parameter that
        // an active region drives is overridden; otherwise the default value
        // (set by the most recent EngineCommand) wins.
        let auto = self.shared.automation.load_full();
        let block_position: u64 = self.playhead as u64;
        for (i, voice) in self.tracks.iter_mut().enumerate() {
            let mut gain = voice.default_gain_db;
            let mut pan = voice.default_pan;
            let mut rvb = voice.default_reverb_send_db;
            let mut dly = voice.default_delay_send_db;
            let mut hp = voice.default_hp_freq;
            let mut lp = voice.default_lp_freq;
            if let Some(track_auto) = auto.by_slot.get(i) {
                for entry in &track_auto.entries {
                    if block_position < entry.start_sample || block_position >= entry.end_sample {
                        continue;
                    }
                    use super::automation::AutoTarget::*;
                    match entry.target {
                        GainDb => gain = entry.value,
                        Pan => pan = entry.value.clamp(-1.0, 1.0),
                        ReverbSendDb => rvb = entry.value,
                        DelaySendDb => dly = entry.value,
                        HighPassFrequencyHz => hp = entry.value,
                        LowPassFrequencyHz => lp = entry.value,
                    }
                }
            }
            voice.gain_db.set_target(gain);
            voice.pan.set_target(pan);
            voice.reverb_send_db.set_target(rvb);
            voice.delay_send_db.set_target(dly);
            voice.high_pass.freq.set_target(hp);
            voice.low_pass.freq.set_target(lp);
        }

        // Refresh per-block filter/EQ coefficients once.
        for t in self.tracks.iter_mut() {
            t.high_pass.refresh(fs);
            t.low_pass.refresh(fs);
            t.eq.refresh_block(fs);
        }

        let mut max_peak = 0.0_f32;
        let mut sources: [Option<Arc<super::shared::TrackSource>>; MAX_TRACKS] =
            std::array::from_fn(|_| None);
        for (i, slot) in self.shared.source_slots.iter().enumerate() {
            sources[i] = slot.load_full();
        }
        let mut track_peak = [0.0_f32; MAX_TRACKS];

        for frame in 0..frames {
            let mut left = 0.0_f32;
            let mut right = 0.0_f32;
            let mut send_reverb_l = 0.0_f32;
            let mut send_reverb_r = 0.0_f32;
            let mut send_delay_l = 0.0_f32;
            let mut send_delay_r = 0.0_f32;

            for (i, track) in self.tracks.iter_mut().enumerate() {
                let g_db = track.gain_db.next();
                let pan = track.pan.next();
                let rvb = track.reverb_send_db.next();
                let dly = track.delay_send_db.next();

                if !track.active || track.muted || (any_solo && !track.solo) {
                    continue;
                }
                let Some(track_source) = sources[i].as_ref() else {
                    continue;
                };
                let session_pos = self.playhead + frame as f64 * session_per_output;
                let mut l = 0.0_f32;
                let mut r = 0.0_f32;
                let mut has_clip = false;
                for source in &track_source.clips {
                    let start_f = source.start_sample as f64;
                    if session_pos < start_f {
                        continue;
                    }
                    let frame_in_track_f = session_pos - start_f;
                    if frame_in_track_f >= source.duration_samples as f64 {
                        continue;
                    }
                    let i_floor = frame_in_track_f as u64;
                    let source_frame = source.source_offset_sample.saturating_add(i_floor);
                    let frac = (frame_in_track_f - i_floor as f64) as f32;
                    let ch = source.channels as usize;
                    let buf = &source.buffer;
                    let idx0 = source_frame as usize * ch;
                    if idx0 + ch > buf.len() {
                        continue;
                    }
                    let l0 = buf[idx0];
                    let r0 = if source.channels >= 2 { buf[idx0 + 1] } else { l0 };
                    let (clip_l, clip_r) = if i_floor + 1 < source.duration_samples && idx0 + 2 * ch <= buf.len() {
                        let idx1 = idx0 + ch;
                        let l1 = buf[idx1];
                        let r1 = if source.channels >= 2 { buf[idx1 + 1] } else { l1 };
                        (l0 + frac * (l1 - l0), r0 + frac * (r1 - r0))
                    } else {
                        (l0, r0)
                    };
                    let clip_gain = db_to_gain(source.gain_db);
                    l += clip_l * clip_gain;
                    r += clip_r * clip_gain;
                    has_clip = true;
                }
                if !has_clip {
                    continue;
                }

                let (lc, rc) = if self.master_bypass {
                    // A/B compare: source samples pass through with no DSP, no gain,
                    // no pan offset, no sends. Mute/solo are still respected above.
                    let _ = (g_db, pan, rvb, dly);
                    (l, r)
                } else {
                    // HP -> LP -> EQ -> Compressor -> Pan -> Gain
                    let (hl, hr) = track.high_pass.process_stereo(l, r);
                    l = hl;
                    r = hr;
                    let (ll, lr) = track.low_pass.process_stereo(l, r);
                    l = ll;
                    r = lr;
                    let (el, er) = track.eq.process_stereo(l, r);
                    l = el;
                    r = er;
                    let comp = track.compressor.process_link(l, r);
                    l *= comp;
                    r *= comp;

                    let gain = db_to_gain(g_db);
                    let l_pan = if pan <= 0.0 { 1.0 } else { 1.0 - pan };
                    let r_pan = if pan >= 0.0 { 1.0 } else { 1.0 + pan };
                    let lc = l * gain * l_pan;
                    let rc = r * gain * r_pan;

                    let rvb_g = db_to_gain(rvb);
                    let dly_g = db_to_gain(dly);
                    send_reverb_l += lc * rvb_g;
                    send_reverb_r += rc * rvb_g;
                    send_delay_l += lc * dly_g;
                    send_delay_r += rc * dly_g;
                    (lc, rc)
                };
                left += lc;
                right += rc;

                let p = lc.abs().max(rc.abs());
                if p > track_peak[i] {
                    track_peak[i] = p;
                }
            }

            // Smooth the master params even in bypass to avoid clicks on toggle.
            let master_gain_db_next = self.master_gain_db.next();
            let _ = self.master_ceiling_db.next();

            let (mut l_out, mut r_out) = if self.master_bypass {
                (left, right)
            } else {
                let (rvb_l, rvb_r) = self.reverb.pull_wet(send_reverb_l, send_reverb_r);
                let (dly_l, dly_r) = self.delay.pull_wet(send_delay_l, send_delay_r);
                left += rvb_l + dly_l;
                right += rvb_r + dly_r;

                let master_gain = db_to_gain(master_gain_db_next);
                let mut pre_l = left * master_gain;
                let mut pre_r = right * master_gain;
                if !pre_l.is_finite() {
                    pre_l = 0.0;
                }
                if !pre_r.is_finite() {
                    pre_r = 0.0;
                }
                self.limiter.process(pre_l, pre_r)
            };
            if !l_out.is_finite() {
                l_out = 0.0;
            }
            if !r_out.is_finite() {
                r_out = 0.0;
            }

            let abs_max = l_out.abs().max(r_out.abs());
            if abs_max > max_peak {
                max_peak = abs_max;
            }

            let off = frame * channels;
            output[off] = l_out;
            if channels >= 2 {
                output[off + 1] = r_out;
            }
            for c in 2..channels {
                output[off + c] = 0.0;
            }
        }

        self.playhead += frames as f64 * session_per_output;
        self.shared.playhead.store(self.playhead as u64, Ordering::Relaxed);
        self.shared
            .master_peak
            .store((max_peak.clamp(0.0, 4.0) * 1_000_000.0) as u32, Ordering::Relaxed);
        for (i, peak) in track_peak.iter().enumerate() {
            self.shared.track_peaks[i]
                .store((peak.clamp(0.0, 4.0) * 1_000_000.0) as u32, Ordering::Relaxed);
        }
    }
}
