//! Automation snapshot pushed from UI to audio thread.
//!
//! The UI rebuilds and publishes a complete snapshot whenever the session
//! changes. The audio thread reads it lock-free per render call via ArcSwap
//! and applies effective targets per block.

use crate::model::{AutomatableParam, MixSession};

#[derive(Debug, Clone, Copy)]
pub enum AutoTarget {
    GainDb,
    Pan,
    ReverbSendDb,
    DelaySendDb,
    HighPassFrequencyHz,
    LowPassFrequencyHz,
}

impl AutoTarget {
    fn from_param(p: &AutomatableParam) -> Option<Self> {
        Some(match p {
            AutomatableParam::GainDb => AutoTarget::GainDb,
            AutomatableParam::Pan => AutoTarget::Pan,
            AutomatableParam::SendsReverbDb => AutoTarget::ReverbSendDb,
            AutomatableParam::SendsDelayDb => AutoTarget::DelaySendDb,
            AutomatableParam::HighPassFrequencyHz => AutoTarget::HighPassFrequencyHz,
            AutomatableParam::LowPassFrequencyHz => AutoTarget::LowPassFrequencyHz,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AutoEntry {
    pub start_sample: u64,
    pub end_sample: u64,
    pub target: AutoTarget,
    pub value: f32,
}

#[derive(Debug, Clone, Default)]
pub struct TrackAutomation {
    pub entries: Vec<AutoEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct AutomationSnapshot {
    pub by_slot: Vec<TrackAutomation>,
}

pub fn build_snapshot(session: &MixSession) -> AutomationSnapshot {
    let regions: std::collections::HashMap<&str, (u64, u64)> = session
        .regions
        .iter()
        .map(|r| (r.id.as_str(), (r.start_sample, r.end_sample)))
        .collect();

    let mut by_slot: Vec<TrackAutomation> = Vec::with_capacity(session.tracks.len());
    for track in &session.tracks {
        let mut entries = Vec::new();
        for lane in &track.automation {
            let Some(target) = AutoTarget::from_param(&lane.param) else {
                continue;
            };
            // PoC writes two equal points at region boundaries; take the first.
            let value = lane.points.first().map(|p| p.value).unwrap_or(0.0);
            let (start, end) = if let Some(rid) = &lane.region_id {
                if let Some(&r) = regions.get(rid.as_str()) {
                    r
                } else {
                    // Fall back to span of points themselves.
                    let s = lane.points.first().map(|p| p.sample).unwrap_or(0);
                    let e = lane.points.last().map(|p| p.sample).unwrap_or(s);
                    (s, e)
                }
            } else {
                let s = lane.points.first().map(|p| p.sample).unwrap_or(0);
                let e = lane.points.last().map(|p| p.sample).unwrap_or(s);
                (s, e)
            };
            entries.push(AutoEntry {
                start_sample: start,
                end_sample: end,
                target,
                value,
            });
        }
        by_slot.push(TrackAutomation { entries });
    }
    AutomationSnapshot { by_slot }
}
