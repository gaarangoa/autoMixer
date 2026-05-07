use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EngineEvent {
    Underrun { slot: u32 },
    Stopped,
    EndOfStream,
}
