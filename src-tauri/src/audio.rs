//! Compatibility shim: render through the engine's offline graph.

use std::path::Path;

use crate::model::MixSession;

pub fn render_mix(session: &MixSession, output_path: &Path) -> Result<(), String> {
    crate::engine::render::render_session(session, output_path).map(|_| ())
}
