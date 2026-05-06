import type { MixSession, SourceFile, Track } from "../../../shared/types";
import { dbToGain } from "./utils";

export async function exportMix(session: MixSession) {
  const decoded = new Map<string, AudioBuffer>();
  const probe = new AudioContext({ sampleRate: session.sampleRate });
  for (const source of session.sourceFiles) {
    const response = await fetch(`/api/files/${source.storedName}`);
    const data = await response.arrayBuffer();
    decoded.set(source.id, await probe.decodeAudioData(data.slice(0)));
  }
  await probe.close();

  const duration = Math.max(1, ...session.tracks.map((track) => {
    const buffer = decoded.get(track.sourceFileId);
    return (track.startSample / session.sampleRate) + (buffer?.duration ?? 0);
  }));
  const offline = new OfflineAudioContext(2, Math.ceil(duration * session.sampleRate), session.sampleRate);
  const master = offline.createGain();
  master.gain.value = dbToGain(session.master.gainDb);
  master.connect(offline.destination);

  const anySolo = session.tracks.some((track) => track.solo);
  for (const track of session.tracks) {
    if (track.muted || (anySolo && !track.solo)) continue;
    const buffer = decoded.get(track.sourceFileId);
    if (!buffer) continue;
    connectTrack(offline, master, track, buffer);
  }

  const rendered = await offline.startRendering();
  const wav = encodeWav(rendered);
  const url = URL.createObjectURL(new Blob([wav], { type: "audio/wav" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}.wav`;
  anchor.click();
  URL.revokeObjectURL(url);
}

function connectTrack(context: OfflineAudioContext, master: AudioNode, track: Track, buffer: AudioBuffer) {
  const source = context.createBufferSource();
  source.buffer = buffer;
  const hp = context.createBiquadFilter();
  hp.type = "highpass";
  hp.frequency.value = track.chain.highPass.enabled ? track.chain.highPass.frequencyHz : 20;
  const lp = context.createBiquadFilter();
  lp.type = "lowpass";
  lp.frequency.value = track.chain.lowPass.enabled ? track.chain.lowPass.frequencyHz : 20000;
  const eq = track.chain.eq.map((band) => {
    const node = context.createBiquadFilter();
    node.type = band.type === "low_shelf" ? "lowshelf" : band.type === "high_shelf" ? "highshelf" : "peaking";
    node.frequency.value = band.frequencyHz;
    node.gain.value = band.gainDb;
    node.Q.value = band.q;
    return node;
  });
  const comp = context.createDynamicsCompressor();
  const state = track.chain.compressor;
  comp.threshold.value = state.enabled ? state.thresholdDb : 0;
  comp.ratio.value = state.enabled ? state.ratio : 1;
  comp.attack.value = state.attackMs / 1000;
  comp.release.value = state.releaseMs / 1000;
  comp.knee.value = state.kneeDb;
  const pan = context.createStereoPanner();
  pan.pan.value = track.pan;
  const gain = context.createGain();
  gain.gain.value = dbToGain(track.gainDb);

  source.connect(hp);
  hp.connect(lp);
  lp.connect(eq[0]);
  eq[0].connect(eq[1]);
  eq[1].connect(eq[2]);
  eq[2].connect(eq[3]);
  eq[3].connect(comp);
  comp.connect(pan);
  pan.connect(gain);
  gain.connect(master);
  source.start(track.startSample / context.sampleRate);
}

function encodeWav(buffer: AudioBuffer) {
  const channels = [buffer.getChannelData(0), buffer.numberOfChannels > 1 ? buffer.getChannelData(1) : buffer.getChannelData(0)];
  const length = channels[0].length;
  const bytes = new ArrayBuffer(44 + length * 4);
  const view = new DataView(bytes);
  writeString(view, 0, "RIFF");
  view.setUint32(4, 36 + length * 4, true);
  writeString(view, 8, "WAVE");
  writeString(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 2, true);
  view.setUint32(24, buffer.sampleRate, true);
  view.setUint32(28, buffer.sampleRate * 4, true);
  view.setUint16(32, 4, true);
  view.setUint16(34, 16, true);
  writeString(view, 36, "data");
  view.setUint32(40, length * 4, true);
  let offset = 44;
  for (let i = 0; i < length; i++) {
    view.setInt16(offset, sampleToInt16(channels[0][i]), true);
    view.setInt16(offset + 2, sampleToInt16(channels[1][i]), true);
    offset += 4;
  }
  return bytes;
}

function sampleToInt16(sample: number) {
  const clipped = Math.max(-1, Math.min(1, sample));
  return clipped < 0 ? clipped * 0x8000 : clipped * 0x7fff;
}

function writeString(view: DataView, offset: number, value: string) {
  for (let i = 0; i < value.length; i++) view.setUint8(offset + i, value.charCodeAt(i));
}
