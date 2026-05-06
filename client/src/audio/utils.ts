export function dbToGain(db: number) {
  if (db <= -60) return 0;
  return Math.pow(10, db / 20);
}

export function gainToDb(gain: number) {
  return 20 * Math.log10(Math.max(0.000001, gain));
}

export function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}
