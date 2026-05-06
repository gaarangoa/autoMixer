import type { MixSession, SourceFile, Track } from "../../../shared/types";
import { dbToGain } from "./utils";

type RuntimeTrack = {
  buffer: AudioBuffer;
  source?: AudioBufferSourceNode;
  gain: GainNode;
  pan: StereoPannerNode;
  hp: BiquadFilterNode;
  lp: BiquadFilterNode;
  eq: BiquadFilterNode[];
  comp: DynamicsCompressorNode;
  reverbSend: GainNode;
  delaySend: GainNode;
};

export class MixEngine {
  private context?: AudioContext;
  private master?: GainNode;
  private reverbBus?: GainNode;
  private delayBus?: GainNode;
  private delay?: DelayNode;
  private feedback?: GainNode;
  private tracks = new Map<string, RuntimeTrack>();
  private startedAt = 0;
  private pausedAt = 0;
  private playing = false;

  async ensureContext() {
    if (!this.context) {
      this.context = new AudioContext({ sampleRate: 48000 });
      this.master = this.context.createGain();
      this.master.connect(this.context.destination);

      this.reverbBus = this.context.createGain();
      this.reverbBus.gain.value = 0.25;
      this.reverbBus.connect(this.master);

      this.delayBus = this.context.createGain();
      this.delay = this.context.createDelay(2);
      this.feedback = this.context.createGain();
      this.delay.delayTime.value = 0.28;
      this.feedback.gain.value = 0.28;
      this.delayBus.connect(this.delay);
      this.delay.connect(this.feedback);
      this.feedback.connect(this.delay);
      this.delay.connect(this.master);
    }
    if (this.context.state === "suspended") await this.context.resume();
    return this.context;
  }

  async loadSession(session: MixSession) {
    const context = await this.ensureContext();
    const bySource = new Map(session.sourceFiles.map((source) => [source.id, source]));
    for (const track of session.tracks) {
      if (this.tracks.has(track.id)) {
        this.updateTrack(track, session);
        continue;
      }
      const source = bySource.get(track.sourceFileId);
      if (!source) continue;
      const buffer = await decodeSource(context, source);
      const runtime = this.createRuntimeTrack(buffer);
      this.tracks.set(track.id, runtime);
      this.updateTrack(track, session);
    }
    for (const id of Array.from(this.tracks.keys())) {
      if (!session.tracks.some((track) => track.id === id)) this.tracks.delete(id);
    }
  }

  updateSession(session: MixSession) {
    for (const track of session.tracks) this.updateTrack(track, session);
    if (this.master && session.master) this.master.gain.setTargetAtTime(dbToGain(session.master.gainDb), this.context!.currentTime, 0.01);
  }

  play(session: MixSession) {
    if (!this.context || !this.master) return;
    this.stopSources();
    const offset = this.pausedAt;
    this.startedAt = this.context.currentTime - offset;
    this.playing = true;
    const anySolo = session.tracks.some((track) => track.solo);
    for (const track of session.tracks) {
      const runtime = this.tracks.get(track.id);
      if (!runtime) continue;
      const source = this.context.createBufferSource();
      source.buffer = runtime.buffer;
      source.connect(runtime.hp);
      runtime.source = source;
      const audible = !track.muted && (!anySolo || track.solo);
      runtime.gain.gain.setValueAtTime(audible ? dbToGain(track.gainDb) : 0, this.context.currentTime);
      source.start(this.context.currentTime, Math.max(0, offset - track.startSample / session.sampleRate));
    }
  }

  pause() {
    if (!this.context || !this.playing) return;
    this.pausedAt = this.context.currentTime - this.startedAt;
    this.stopSources();
    this.playing = false;
  }

  stop() {
    this.stopSources();
    this.pausedAt = 0;
    this.playing = false;
  }

  seek(seconds: number) {
    this.pausedAt = Math.max(0, seconds);
  }

  getPlayhead() {
    if (!this.context) return this.pausedAt;
    return this.playing ? this.context.currentTime - this.startedAt : this.pausedAt;
  }

  getBuffer(trackId: string) {
    return this.tracks.get(trackId)?.buffer;
  }

  private createRuntimeTrack(buffer: AudioBuffer): RuntimeTrack {
    const context = this.context!;
    const hp = context.createBiquadFilter();
    hp.type = "highpass";
    const lp = context.createBiquadFilter();
    lp.type = "lowpass";
    const eq = [context.createBiquadFilter(), context.createBiquadFilter(), context.createBiquadFilter(), context.createBiquadFilter()];
    eq[0].type = "lowshelf";
    eq[1].type = "peaking";
    eq[2].type = "peaking";
    eq[3].type = "highshelf";
    const comp = context.createDynamicsCompressor();
    const pan = context.createStereoPanner();
    const gain = context.createGain();
    const reverbSend = context.createGain();
    const delaySend = context.createGain();

    hp.connect(lp);
    lp.connect(eq[0]);
    eq[0].connect(eq[1]);
    eq[1].connect(eq[2]);
    eq[2].connect(eq[3]);
    eq[3].connect(comp);
    comp.connect(pan);
    pan.connect(gain);
    gain.connect(this.master!);
    gain.connect(reverbSend);
    gain.connect(delaySend);
    reverbSend.connect(this.reverbBus!);
    delaySend.connect(this.delayBus!);

    return { buffer, hp, lp, eq, comp, pan, gain, reverbSend, delaySend };
  }

  private updateTrack(track: Track, session: MixSession) {
    if (!this.context) return;
    const runtime = this.tracks.get(track.id);
    if (!runtime) return;
    const now = this.context.currentTime;
    const anySolo = session.tracks.some((item) => item.solo);
    const audible = !track.muted && (!anySolo || track.solo);
    runtime.gain.gain.setTargetAtTime(audible ? dbToGain(track.gainDb) : 0, now, 0.01);
    runtime.pan.pan.setTargetAtTime(track.pan, now, 0.01);

    runtime.hp.frequency.setTargetAtTime(track.chain.highPass.enabled ? track.chain.highPass.frequencyHz : 20, now, 0.01);
    runtime.lp.frequency.setTargetAtTime(track.chain.lowPass.enabled ? track.chain.lowPass.frequencyHz : 20000, now, 0.01);
    track.chain.eq.forEach((band, index) => {
      runtime.eq[index].frequency.setTargetAtTime(band.frequencyHz, now, 0.01);
      runtime.eq[index].gain.setTargetAtTime(band.gainDb, now, 0.01);
      runtime.eq[index].Q.setTargetAtTime(band.q, now, 0.01);
    });

    const comp = track.chain.compressor;
    runtime.comp.threshold.setTargetAtTime(comp.enabled ? comp.thresholdDb : 0, now, 0.01);
    runtime.comp.ratio.setTargetAtTime(comp.enabled ? comp.ratio : 1, now, 0.01);
    runtime.comp.attack.setTargetAtTime(comp.attackMs / 1000, now, 0.01);
    runtime.comp.release.setTargetAtTime(comp.releaseMs / 1000, now, 0.01);
    runtime.comp.knee.setTargetAtTime(comp.kneeDb, now, 0.01);
    runtime.reverbSend.gain.setTargetAtTime(dbToGain(track.sends.reverbDb), now, 0.01);
    runtime.delaySend.gain.setTargetAtTime(dbToGain(track.sends.delayDb), now, 0.01);
  }

  private stopSources() {
    for (const runtime of this.tracks.values()) {
      try {
        runtime.source?.stop();
      } catch {
        // source may already be stopped
      }
      runtime.source = undefined;
    }
  }
}

async function decodeSource(context: AudioContext, source: SourceFile) {
  const response = await fetch(`/api/files/${source.storedName}`);
  const arrayBuffer = await response.arrayBuffer();
  return context.decodeAudioData(arrayBuffer.slice(0));
}
