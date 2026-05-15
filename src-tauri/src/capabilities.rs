use crate::model::{SkillCard, SkillCatalog};

pub fn skill_catalog() -> SkillCatalog {
    SkillCatalog {
        skills: vec![
            card(
                "session_prep",
                "Session Prep",
                "Use for raw multitrack organization: rename tracks, assign roles, mute unusable alternates, and prepare a rough layout.",
                &["organize", "prep", "raw", "multitrack", "tracks", "takes", "roles", "rename"],
                &["rename_track", "set_track_role", "mute_track", "set_track_gain", "set_track_pan"],
            ),
            card(
                "balance",
                "Balance",
                "Use for level, mute, solo, and pan moves.",
                &["louder", "quieter", "front", "back", "wide", "center"],
                &["set_track_gain", "adjust_track_gain", "set_track_pan", "mute_track", "solo_track"],
            ),
            card(
                "tonal_eq",
                "Tonal EQ",
                "Use for brightness, warmth, mud, harshness, rumble, and presence.",
                &["bright", "dark", "warm", "muddy", "harsh", "presence"],
                &["set_high_pass", "set_low_pass", "set_eq_band"],
            ),
            card(
                "dynamics",
                "Dynamics",
                "Use for compression, punch, control, and steadiness.",
                &["compress", "punch", "tight", "controlled", "steady"],
                &["set_compressor"],
            ),
            card(
                "space_depth",
                "Space And Depth",
                "Use for reverb, delay, dry/close, and depth moves.",
                &["reverb", "delay", "space", "dry", "closer", "deeper"],
                &["set_reverb_send", "set_delay_send"],
            ),
            card(
                "mastering",
                "Mastering",
                "Use for whole-mix loudness, headroom, and master-bus level moves.",
                &["louder mix", "quieter mix", "master", "loudness", "headroom", "lufs", "ceiling", "whole mix"],
                &["set_master_gain", "adjust_master_gain"],
            ),
            card(
                "region_automation",
                "Region Automation",
                "Use for section-scoped moves on selected regions.",
                &["chorus", "verse", "hook", "section", "only here"],
                &["create_region", "set_region_gain", "apply_section_automation"],
            ),
            card(
                "safety_undo",
                "Safety And Undo",
                "Use for undo and redo requests.",
                &["undo", "redo", "revert"],
                &["undo", "redo"],
            ),
            card(
                "render_export",
                "Render Export",
                "Use when the user asks to render or export the current mix.",
                &["render", "export", "bounce", "wav"],
                &["render_mix"],
            ),
            card(
                "critique",
                "Mix Critique",
                "Use when the user asks for a rating, critique, evaluation, assessment, review, or feedback about the mix or a specific track. Returns analysis only — emits no actions.",
                &["rate", "critique", "evaluate", "assess", "review", "feedback", "score", "how is", "what do you think"],
                &[],
            ),
        ],
    }
}

pub fn select_skills(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut skills = Vec::new();
    if has_any(&lower, &["undo", "redo", "revert"]) {
        skills.push("safety_undo".into());
    }
    if has_any(&lower, &["organize", "prep", "raw", "multitrack", "tracks", "takes", "roles", "rename"]) {
        skills.push("session_prep".into());
    }
    if has_any(&lower, &["eq", "bright", "dark", "warm", "mud", "harsh", "presence", "rumble", "air", "low", "mid", "high"]) {
        skills.push("tonal_eq".into());
    }
    if has_any(&lower, &["compress", "punch", "tight", "dynamic", "control"]) {
        skills.push("dynamics".into());
    }
    if has_any(&lower, &["reverb", "delay", "space", "room", "dry", "closer", "deeper"]) {
        skills.push("space_depth".into());
    }
    if has_any(&lower, &["chorus", "verse", "hook", "section", "region"]) {
        skills.push("region_automation".into());
    }
    if has_any(&lower, &["render", "export", "bounce", "wav"]) {
        skills.push("render_export".into());
    }
    if skills.is_empty() || has_any(&lower, &["louder", "quieter", "front", "forward", "back", "pan", "wide", "mute", "solo"]) {
        skills.insert(0, "balance".into());
    }
    skills.sort();
    skills.dedup();
    skills
}

pub fn allowed_actions(selected_skills: &[String]) -> Vec<String> {
    let catalog = skill_catalog();
    catalog
        .skills
        .into_iter()
        .filter(|skill| selected_skills.contains(&skill.skill_id))
        .flat_map(|skill| skill.summary_actions)
        .collect()
}

fn card(id: &str, name: &str, when: &str, intents: &[&str], actions: &[&str]) -> SkillCard {
    SkillCard {
        skill_id: id.to_string(),
        display_name: name.to_string(),
        when_to_use: when.to_string(),
        musical_intents: intents.iter().map(|item| item.to_string()).collect(),
        summary_actions: actions.iter().map(|item| item.to_string()).collect(),
        required_context: vec!["track names".into(), "track roles".into(), "current parameter values".into()],
    }
}

fn has_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.contains(word))
}
