import type { TimeSignature } from "../../shared/types";

export type SnapResolution = "bar" | "beat" | "1/2" | "1/4" | "1/8" | "1/16";

export type MusicalPosition = {
  bar: number;
  beat: number;
  sixteenth: number;
};

export function normalizeTimeSignature(value?: TimeSignature): TimeSignature {
  const numerator = Number.isFinite(value?.numerator)
    ? Math.max(1, Math.min(32, Math.round(value!.numerator)))
    : 4;
  const rawDenominator = Number.isFinite(value?.denominator) ? Math.round(value!.denominator) : 4;
  const denominator = [1, 2, 4, 8, 16, 32].includes(rawDenominator) ? rawDenominator : 4;
  return { numerator, denominator };
}

export function beatSeconds(bpm: number, signature?: TimeSignature): number {
  const safeBpm = Math.max(1, Number.isFinite(bpm) ? bpm : 120);
  const { denominator } = normalizeTimeSignature(signature);
  return (60 / safeBpm) * (4 / denominator);
}

export function barSeconds(bpm: number, signature?: TimeSignature): number {
  const { numerator } = normalizeTimeSignature(signature);
  return beatSeconds(bpm, signature) * numerator;
}

export function gridStepSeconds(
  bpm: number,
  signature: TimeSignature | undefined,
  resolution: SnapResolution,
): number {
  if (resolution === "bar") return barSeconds(bpm, signature);
  if (resolution === "beat") return beatSeconds(bpm, signature);
  const denominator = Number(resolution.slice(2));
  return (240 / Math.max(1, bpm)) / denominator;
}

export function snapSecondsToGrid(
  seconds: number,
  bpm: number,
  signature: TimeSignature | undefined,
  resolution: SnapResolution,
): number {
  const step = gridStepSeconds(bpm, signature, resolution);
  return Math.max(0, Math.round(Math.max(0, seconds) / step) * step);
}

export function musicalPosition(
  seconds: number,
  bpm: number,
  signature?: TimeSignature,
  projectStartBar = 1,
): MusicalPosition {
  const normalized = normalizeTimeSignature(signature);
  const beatLength = beatSeconds(bpm, normalized);
  const absoluteBeats = Math.max(0, seconds) / beatLength;
  const barOffset = Math.floor(absoluteBeats / normalized.numerator);
  const beatInBar = Math.floor(absoluteBeats % normalized.numerator);
  const fractionOfBeat = absoluteBeats - Math.floor(absoluteBeats);
  const sixteenthsPerBeat = Math.max(1, 16 / normalized.denominator);
  return {
    bar: projectStartBar + barOffset,
    beat: beatInBar + 1,
    sixteenth: Math.floor(fractionOfBeat * sixteenthsPerBeat) + 1,
  };
}

export function formatMusicalPosition(
  seconds: number,
  bpm: number,
  signature?: TimeSignature,
  projectStartBar = 1,
): string {
  const position = musicalPosition(seconds, bpm, signature, projectStartBar);
  return `${position.bar}.${position.beat}.${position.sixteenth}`;
}

export function timeToPixels(seconds: number, pixelsPerSecond: number): number {
  return Math.max(0, seconds) * Math.max(0.01, pixelsPerSecond);
}

export function pixelsToTime(pixels: number, pixelsPerSecond: number): number {
  return Math.max(0, pixels) / Math.max(0.01, pixelsPerSecond);
}

export function zoomScrollLeft(
  previousScrollLeft: number,
  anchorViewportX: number,
  previousPixelsPerSecond: number,
  nextPixelsPerSecond: number,
): number {
  const anchorSeconds = (Math.max(0, previousScrollLeft) + Math.max(0, anchorViewportX))
    / Math.max(0.01, previousPixelsPerSecond);
  return Math.max(0, anchorSeconds * Math.max(0.01, nextPixelsPerSecond) - Math.max(0, anchorViewportX));
}
