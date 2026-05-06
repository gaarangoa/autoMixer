import { v4 as uuid } from "uuid";
import type { HistoryEntry, JsonPatch, MixAction, MixProject, MixSession, Track } from "../../shared/types";

type ApplyResult = {
  project: MixProject;
  entry?: HistoryEntry;
};

export function applyActions(project: MixProject, actions: MixAction[], source: "user" | "assistant", explanation?: string): ApplyResult {
  const forwardPatch: JsonPatch[] = [];
  const inversePatch: JsonPatch[] = [];

  for (const action of actions) {
    applyAction(project.session, action, forwardPatch, inversePatch);
  }

  if (!forwardPatch.length) return { project };

  return {
    project,
    entry: {
      id: uuid(),
      timestamp: Date.now(),
      source,
      explanation,
      forwardPatch,
      inversePatch
    }
  };
}

export function undo(project: MixProject): HistoryEntry | undefined {
  const entry = project.history.pop();
  if (!entry) return undefined;
  applyPatch(project.session, entry.inversePatch);
  project.redoStack.push(entry);
  return entry;
}

export function redo(project: MixProject): HistoryEntry | undefined {
  const entry = project.redoStack.pop();
  if (!entry) return undefined;
  applyPatch(project.session, entry.forwardPatch);
  project.history.push(entry);
  return entry;
}

export function validateActions(session: MixSession, actions: MixAction[]) {
  for (const action of actions) {
    switch (action.tool) {
      case "create_region":
        range(action.startSample, 0, Number.MAX_SAFE_INTEGER, "startSample");
        range(action.endSample, action.startSample + 1, Number.MAX_SAFE_INTEGER, "endSample");
        break;
      case "set_track_gain":
        requireTrack(session, action.trackId);
        range(action.gainDb, -24, 24, "gainDb");
        break;
      case "adjust_track_gain":
        requireTrack(session, action.trackId);
        range(action.deltaDb, -12, 12, "deltaDb");
        break;
      case "set_track_pan":
        requireTrack(session, action.trackId);
        range(action.pan, -1, 1, "pan");
        break;
      case "mute_track":
      case "solo_track":
        requireTrack(session, action.trackId);
        break;
      case "set_high_pass":
      case "set_low_pass":
        requireTrack(session, action.trackId);
        range(action.frequencyHz, 20, 20000, "frequencyHz");
        if (![12, 24].includes(action.slopeDbOct)) throw new Error("slopeDbOct must be 12 or 24");
        break;
      case "set_eq_band":
        requireTrack(session, action.trackId);
        range(action.band, 0, 3, "band");
        range(action.frequencyHz, 20, 20000, "frequencyHz");
        range(action.gainDb, -12, 12, "gainDb");
        range(action.q, 0.2, 10, "q");
        break;
      case "set_compressor":
        requireTrack(session, action.trackId);
        range(action.thresholdDb, -60, 0, "thresholdDb");
        range(action.ratio, 1, 20, "ratio");
        range(action.attackMs, 1, 200, "attackMs");
        range(action.releaseMs, 20, 1000, "releaseMs");
        range(action.kneeDb, 0, 24, "kneeDb");
        range(action.makeupDb, -12, 12, "makeupDb");
        break;
      case "set_reverb_send":
      case "set_delay_send":
        requireTrack(session, action.trackId);
        range(action.levelDb, -60, 6, "levelDb");
        break;
      case "set_processor_param":
        requireTrack(session, action.targetId);
        range(action.value, -100000, 100000, "value");
        break;
      case "set_region_gain":
      case "apply_section_automation":
        requireRegion(session, action.regionId);
        requireTrack(session, action.trackId);
        if (action.tool === "set_region_gain") range(action.gainDb, -24, 24, "gainDb");
        if (action.tool === "apply_section_automation") range(action.value, -60, 20000, "value");
        break;
      case "undo":
      case "redo":
      case "render_mix":
        break;
      default:
        assertNever(action);
    }
  }
}

function applyAction(session: MixSession, action: MixAction, forward: JsonPatch[], inverse: JsonPatch[]) {
  switch (action.tool) {
    case "create_region": {
      const region = {
        id: uuid(),
        name: action.name,
        startSample: action.startSample,
        endSample: action.endSample,
        trackIds: action.trackIds
      };
      const index = session.regions.length;
      session.regions.push(region);
      forward.push({ op: "add", path: `/regions/${index}`, value: region });
      inverse.unshift({ op: "remove", path: `/regions/${index}` });
      break;
    }
    case "set_track_gain":
      setTrackValue(session, action.trackId, "gainDb", action.gainDb, forward, inverse);
      break;
    case "adjust_track_gain": {
      const track = requireTrack(session, action.trackId);
      setTrackValue(session, action.trackId, "gainDb", clamp(track.gainDb + action.deltaDb, -24, 24), forward, inverse);
      break;
    }
    case "set_track_pan":
      setTrackValue(session, action.trackId, "pan", action.pan, forward, inverse);
      break;
    case "mute_track":
      setTrackValue(session, action.trackId, "muted", action.muted, forward, inverse);
      break;
    case "solo_track":
      setTrackValue(session, action.trackId, "solo", action.solo, forward, inverse);
      break;
    case "set_high_pass": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/chain/highPass/enabled`, true, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/highPass/frequencyHz`, action.frequencyHz, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/highPass/slopeDbOct`, action.slopeDbOct, forward, inverse);
      break;
    }
    case "set_low_pass": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/chain/lowPass/enabled`, true, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/lowPass/frequencyHz`, action.frequencyHz, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/lowPass/slopeDbOct`, action.slopeDbOct, forward, inverse);
      break;
    }
    case "set_eq_band": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/chain/eq/${action.band}/frequencyHz`, action.frequencyHz, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/eq/${action.band}/gainDb`, action.gainDb, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/eq/${action.band}/q`, action.q, forward, inverse);
      break;
    }
    case "set_compressor": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/chain/compressor/enabled`, true, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/thresholdDb`, action.thresholdDb, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/ratio`, action.ratio, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/attackMs`, action.attackMs, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/releaseMs`, action.releaseMs, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/kneeDb`, action.kneeDb, forward, inverse);
      replace(session, `/tracks/${trackIndex}/chain/compressor/makeupDb`, action.makeupDb, forward, inverse);
      break;
    }
    case "set_reverb_send": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/sends/reverbDb`, action.levelDb, forward, inverse);
      break;
    }
    case "set_delay_send": {
      const trackIndex = trackIndexById(session, action.trackId);
      replace(session, `/tracks/${trackIndex}/sends/delayDb`, action.levelDb, forward, inverse);
      break;
    }
    case "set_processor_param":
      applyProcessorParam(session, action.targetId, action.processorId, action.paramId, action.value, forward, inverse);
      break;
    case "set_region_gain":
      addAutomation(session, action.trackId, action.regionId, "gainDb", action.gainDb, forward, inverse);
      break;
    case "apply_section_automation":
      addAutomation(session, action.trackId, action.regionId, action.param, action.value, forward, inverse);
      break;
    case "undo":
    case "redo":
    case "render_mix":
      break;
  }
}

function applyProcessorParam(session: MixSession, trackId: string, processorId: string, paramId: string, value: number, forward: JsonPatch[], inverse: JsonPatch[]) {
  const i = trackIndexById(session, trackId);
  if (processorId === "track_balance" && paramId === "gainDb") return replace(session, `/tracks/${i}/gainDb`, value, forward, inverse);
  if (processorId === "track_balance" && paramId === "pan") return replace(session, `/tracks/${i}/pan`, value, forward, inverse);
  if (processorId === "sends" && paramId === "reverbDb") return replace(session, `/tracks/${i}/sends/reverbDb`, value, forward, inverse);
  if (processorId === "sends" && paramId === "delayDb") return replace(session, `/tracks/${i}/sends/delayDb`, value, forward, inverse);
  if (processorId === "filters" && paramId === "highPass.frequencyHz") {
    replace(session, `/tracks/${i}/chain/highPass/enabled`, true, forward, inverse);
    return replace(session, `/tracks/${i}/chain/highPass/frequencyHz`, value, forward, inverse);
  }
  if (processorId === "filters" && paramId === "lowPass.frequencyHz") {
    replace(session, `/tracks/${i}/chain/lowPass/enabled`, true, forward, inverse);
    return replace(session, `/tracks/${i}/chain/lowPass/frequencyHz`, value, forward, inverse);
  }
  const eqMatch = paramId.match(/^band([0-3])\.(frequencyHz|gainDb|q)$/);
  if (processorId === "eq_4band" && eqMatch) return replace(session, `/tracks/${i}/chain/eq/${eqMatch[1]}/${eqMatch[2]}`, value, forward, inverse);
  if (processorId === "compressor" && ["thresholdDb", "ratio", "attackMs", "releaseMs"].includes(paramId)) {
    replace(session, `/tracks/${i}/chain/compressor/enabled`, true, forward, inverse);
    return replace(session, `/tracks/${i}/chain/compressor/${paramId}`, value, forward, inverse);
  }
  throw new Error(`Unknown processor param ${processorId}.${paramId}`);
}

function addAutomation(session: MixSession, trackId: string, regionId: string, param: "gainDb" | string, value: number, forward: JsonPatch[], inverse: JsonPatch[]) {
  const trackIndex = trackIndexById(session, trackId);
  const track = session.tracks[trackIndex];
  const region = requireRegion(session, regionId);
  const lane = {
    id: uuid(),
    param: param as any,
    regionId,
    curve: "linear" as const,
    points: [
      { sample: region.startSample, value },
      { sample: region.endSample, value }
    ]
  };
  const laneIndex = track.automation.length;
  track.automation.push(lane);
  forward.push({ op: "add", path: `/tracks/${trackIndex}/automation/${laneIndex}`, value: lane });
  inverse.unshift({ op: "remove", path: `/tracks/${trackIndex}/automation/${laneIndex}` });
}

function setTrackValue<K extends keyof Track>(session: MixSession, trackId: string, key: K, value: Track[K], forward: JsonPatch[], inverse: JsonPatch[]) {
  const i = trackIndexById(session, trackId);
  replace(session, `/tracks/${i}/${String(key)}`, value, forward, inverse);
}

function replace(session: MixSession, path: string, value: unknown, forward: JsonPatch[], inverse: JsonPatch[]) {
  const previous = getPath(session, path);
  if (Object.is(previous, value)) return;
  setPath(session, path, value);
  forward.push({ op: "replace", path, value });
  inverse.unshift({ op: "replace", path, value: previous });
}

function applyPatch(session: MixSession, patch: JsonPatch[]) {
  for (const op of patch) {
    if (op.op === "replace" || op.op === "add") setPath(session, op.path, op.value);
    if (op.op === "remove") removePath(session, op.path);
  }
}

function getPath(root: unknown, pointer: string): unknown {
  return pointer
    .split("/")
    .slice(1)
    .reduce((acc: any, key) => acc?.[decodeSegment(key)], root as any);
}

function setPath(root: unknown, pointer: string, value: unknown) {
  const parts = pointer.split("/").slice(1).map(decodeSegment);
  const last = parts.pop();
  if (!last) return;
  const parent = parts.reduce((acc: any, key) => acc[key], root as any);
  parent[last] = value;
}

function removePath(root: unknown, pointer: string) {
  const parts = pointer.split("/").slice(1).map(decodeSegment);
  const last = parts.pop();
  if (!last) return;
  const parent = parts.reduce((acc: any, key) => acc[key], root as any);
  if (Array.isArray(parent)) parent.splice(Number(last), 1);
  else delete parent[last];
}

function decodeSegment(segment: string) {
  return segment.replace(/~1/g, "/").replace(/~0/g, "~");
}

function requireTrack(session: MixSession, trackId: string): Track {
  const track = session.tracks.find((item) => item.id === trackId);
  if (!track) throw new Error(`Unknown track ${trackId}`);
  return track;
}

function requireRegion(session: MixSession, regionId: string) {
  const region = session.regions.find((item) => item.id === regionId);
  if (!region) throw new Error(`Unknown region ${regionId}`);
  return region;
}

function trackIndexById(session: MixSession, trackId: string) {
  const index = session.tracks.findIndex((track) => track.id === trackId);
  if (index < 0) throw new Error(`Unknown track ${trackId}`);
  return index;
}

function range(value: number, min: number, max: number, label: string) {
  if (!Number.isFinite(value) || value < min || value > max) throw new Error(`${label} must be between ${min} and ${max}`);
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function assertNever(value: never): never {
  throw new Error(`Unhandled action ${(value as { tool?: string }).tool}`);
}
