export const RECORDING_TIMELINE_CHUNK_SECONDS = 60;
export const RECORDING_TIMELINE_LOOKAHEAD_SECONDS = 30;

/**
 * Keep enough empty timeline visible for a recording to continue without a
 * fixed end. The horizon grows in stable chunks so the lanes do not rescale on
 * every animation frame.
 */
export function recordingTimelineDuration(baseDuration: number, playhead: number): number {
  const safeBase = Number.isFinite(baseDuration) ? Math.max(0, baseDuration) : 0;
  const safePlayhead = Number.isFinite(playhead) ? Math.max(0, playhead) : 0;
  const requiredDuration = safePlayhead + RECORDING_TIMELINE_LOOKAHEAD_SECONDS;
  if (requiredDuration <= safeBase) return safeBase;
  return Math.ceil(requiredDuration / RECORDING_TIMELINE_CHUNK_SECONDS)
    * RECORDING_TIMELINE_CHUNK_SECONDS;
}

export function shouldStopPlaybackAtTimelineEnd(
  elapsed: number,
  duration: number,
  recordingActive: boolean,
): boolean {
  return !recordingActive && duration > 0 && elapsed >= duration;
}
