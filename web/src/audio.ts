/**
 * UIPod Web Audio Engine:
 * - Plays bundled Y2K MP3 demo tracks (`./audio/aquabios-2000.mp3`, `./audio/chrome-breakbeat.mp3`)
 * - Decodes & plays any user-dropped audio file (MP3, FLAC, WAV, OGG, M4A)
 * - Real 10-band Parametric EQ + AutoEQ headphone target curves
 * - Real Tape Mode DSP (Wow & Flutter LFO delay, Tape Hiss, Analog Saturation, HF Roll-Off, Stop/Start motor ramp)
 * - Authentic iPod Click Wheel menu click transient synth
 * - Live stereo PCM S16 tap (1024 frames @ sampleRate) for the real Rust `VizDsp` + WebGL2 shaders!
 */

export interface DspParams {
  eq: boolean;
  autoeq: number;
  tape: boolean;
  wow: number;       // 0..100
  hiss: number;      // 0..100
  sat: number;       // 0..100
  hf: number;        // 0..100
  stopStart: boolean;
  speed: number;     // 50..150 (%)
}

const BUILTIN_TRACK_URLS: string[] = [
  './audio/aquabios-2000.mp3',
  './audio/chrome-breakbeat.mp3',
  './audio/possibility-2000.mp3',
  './audio/distortedspaceship.mp3',
];

const BUILTIN_TRACK_DURATIONS_SEC: number[] = [23.22, 23.325, 52.723, 65.907];

const EQ_FREQS = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

const AUTOEQ_CURVES: number[][] = [
  [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],                           // 0: Off / Flat
  [-4.5, -5.2, -4.0, -1.8, 0.4, 1.5, 2.2, 3.4, 1.8, 1.0],   // 1: Sony WH-1000XM4
  [1.2, 0.8, -0.5, -1.0, 0.2, 0.8, 1.5, -1.2, 0.6, 1.5],    // 2: Moondrop Aria
  [4.8, 3.2, 0.5, -1.4, -0.6, 0.0, 1.2, 1.8, 2.2, 1.6],     // 3: Sennheiser HD 650
  [2.5, 0.5, -3.5, -3.0, -0.8, 0.6, 2.0, 3.8, 4.2, 3.0],    // 4: Koss Porta Pro
  [1.0, 0.6, -0.8, -0.4, 0.0, 0.4, 1.0, -0.6, 1.4, 2.0],    // 5: AirPods Pro 2
  [2.8, 2.2, 1.2, 0.4, -0.4, 0.0, 0.6, 1.2, -0.8, -1.4],    // 6: Studio Warm
];

export class UIPodAudioEngine {
  private ctx: AudioContext | null = null;
  private masterGain: GainNode | null = null;
  private preTapGain: GainNode | null = null;
  private eqFilters: BiquadFilterNode[] = [];
  private tapeShaper: WaveShaperNode | null = null;
  private tapeLowpass: BiquadFilterNode | null = null;
  private tapeDelay: DelayNode | null = null;
  private wowOsc1: OscillatorNode | null = null;
  private wowGain1: GainNode | null = null;
  private wowOsc2: OscillatorNode | null = null;
  private wowGain2: GainNode | null = null;
  private hissNode: AudioBufferSourceNode | null = null;
  private hissGain: GainNode | null = null;

  private splitter: ChannelSplitterNode | null = null;
  private analyserL: AnalyserNode | null = null;
  private analyserR: AnalyserNode | null = null;
  private floatBufL = new Float32Array(1024);
  private floatBufR = new Float32Array(1024);
  private pcmOut = new Int16Array(2048);

  private sourceNode: AudioBufferSourceNode | null = null;
  private builtinCache = new Map<number, AudioBuffer>();
  private builtinLoading = new Map<number, Promise<AudioBuffer>>();
  private userBuffers = new Map<number, AudioBuffer>();
  private playSeq = 0;

  public playing = false;
  public currentTrack = 0;
  public trackCount = BUILTIN_TRACK_URLS.length;
  private offsetSec = 0.0;
  private startCtxTime = 0;
  private currentDurSec = BUILTIN_TRACK_DURATIONS_SEC[0];
  private volumeNorm = 0.8;

  public dsp: DspParams = {
    eq: true,
    autoeq: 0,
    tape: false,
    wow: 35,
    hiss: 22,
    sat: 30,
    hf: 40,
    stopStart: true,
    speed: 100,
  };

  private ensureContext(): AudioContext {
    if (this.ctx) {
      if (this.ctx.state === 'suspended') {
        void this.ctx.resume();
      }
      return this.ctx;
    }

    const ctx = new AudioContext();
    this.ctx = ctx;

    let prev: AudioNode | null = null;
    this.eqFilters = EQ_FREQS.map((freq, idx) => {
      const f = ctx.createBiquadFilter();
      f.type = idx === 0 ? 'lowshelf' : idx === EQ_FREQS.length - 1 ? 'highshelf' : 'peaking';
      f.frequency.value = freq;
      f.Q.value = 1.4;
      f.gain.value = 0;
      if (prev) prev.connect(f);
      prev = f;
      return f;
    });

    const delay = ctx.createDelay(0.15);
    delay.delayTime.value = 0.012;
    this.tapeDelay = delay;

    const wowOsc1 = ctx.createOscillator();
    wowOsc1.frequency.value = 0.65;
    const wowGain1 = ctx.createGain();
    wowGain1.gain.value = 0;
    wowOsc1.connect(wowGain1).connect(delay.delayTime);
    wowOsc1.start();
    this.wowOsc1 = wowOsc1;
    this.wowGain1 = wowGain1;

    const wowOsc2 = ctx.createOscillator();
    wowOsc2.frequency.value = 6.2;
    const wowGain2 = ctx.createGain();
    wowGain2.gain.value = 0;
    wowOsc2.connect(wowGain2).connect(delay.delayTime);
    wowOsc2.start();
    this.wowOsc2 = wowOsc2;
    this.wowGain2 = wowGain2;

    const shaper = ctx.createWaveShaper();
    shaper.oversample = '2x';
    shaper.curve = this.makeSaturationCurve(0);
    this.tapeShaper = shaper;

    const lp = ctx.createBiquadFilter();
    lp.type = 'lowpass';
    lp.frequency.value = 20000;
    lp.Q.value = 0.7;
    this.tapeLowpass = lp;

    const preTap = ctx.createGain();
    preTap.gain.value = 1.0;
    this.preTapGain = preTap;

    const noiseLen = ctx.sampleRate * 2;
    const noiseBuf = ctx.createBuffer(2, noiseLen, ctx.sampleRate);
    for (let ch = 0; ch < 2; ch++) {
      const d = noiseBuf.getChannelData(ch);
      let b0 = 0, b1 = 0;
      for (let i = 0; i < noiseLen; i++) {
        const w = Math.random() * 2 - 1;
        b0 = 0.92 * b0 + w * 0.08;
        b1 = 0.65 * b1 + w * 0.35;
        d[i] = (w * 0.45 + b1 * 0.4 - b0 * 0.25) * 0.14;
      }
    }
    const hissSrc = ctx.createBufferSource();
    hissSrc.buffer = noiseBuf;
    hissSrc.loop = true;
    const hissFilter = ctx.createBiquadFilter();
    hissFilter.type = 'bandpass';
    hissFilter.frequency.value = 4200;
    hissFilter.Q.value = 0.55;
    const hissGain = ctx.createGain();
    hissGain.gain.value = 0;
    hissSrc.connect(hissFilter).connect(hissGain).connect(preTap);
    hissSrc.start();
    this.hissNode = hissSrc;
    this.hissGain = hissGain;

    const splitter = ctx.createChannelSplitter(2);
    const analyserL = ctx.createAnalyser();
    const analyserR = ctx.createAnalyser();
    analyserL.fftSize = 2048;
    analyserR.fftSize = 2048;
    this.splitter = splitter;
    this.analyserL = analyserL;
    this.analyserR = analyserR;

    const master = ctx.createGain();
    master.gain.value = Math.pow(this.volumeNorm, 2.1);
    this.masterGain = master;

    this.eqFilters[this.eqFilters.length - 1].connect(delay);
    delay.connect(shaper);
    shaper.connect(lp);
    lp.connect(preTap);
    preTap.connect(splitter);
    splitter.connect(analyserL, 0);
    splitter.connect(analyserR, 1);
    preTap.connect(master);
    master.connect(ctx.destination);

    this.applyDsp(this.dsp);
    return ctx;
  }

  private makeSaturationCurve(amount: number): Float32Array<ArrayBuffer> {
    const n = 1024;
    const buf = new ArrayBuffer(n * 4);
    const curve = new Float32Array(buf);
    if (amount <= 0.001) {
      for (let i = 0; i < n; i++) {
        curve[i] = (i * 2) / (n - 1) - 1;
      }
      return curve;
    }
    const drive = 1 + amount * 4.5;
    const norm = Math.tanh(drive);
    for (let i = 0; i < n; i++) {
      const x = (i * 2) / (n - 1) - 1;
      curve[i] = Math.tanh(x * drive) / norm;
    }
    return curve;
  }

  public applyDsp(dsp: DspParams): void {
    this.dsp = { ...dsp };
    if (!this.ctx) return;
    const now = this.ctx.currentTime;

    const curve = dsp.eq
      ? AUTOEQ_CURVES[dsp.autoeq % AUTOEQ_CURVES.length]
      : AUTOEQ_CURVES[0];
    this.eqFilters.forEach((f, i) => {
      f.gain.setTargetAtTime(curve[i] ?? 0, now, 0.03);
    });

    if (dsp.tape) {
      const wowAmt = (dsp.wow / 100) * 0.0032;
      const flutAmt = (dsp.wow / 100) * 0.00055;
      this.wowGain1?.gain.setTargetAtTime(wowAmt, now, 0.05);
      this.wowGain2?.gain.setTargetAtTime(flutAmt, now, 0.05);
      if (this.tapeShaper) {
        this.tapeShaper.curve = this.makeSaturationCurve(dsp.sat / 100);
      }
      const cutoff = 18000 - (dsp.hf / 100) * 14500;
      this.tapeLowpass?.frequency.setTargetAtTime(cutoff, now, 0.04);
      const hissLevel = this.playing ? (dsp.hiss / 100) * 0.24 : 0;
      this.hissGain?.gain.setTargetAtTime(hissLevel, now, 0.08);
    } else {
      this.wowGain1?.gain.setTargetAtTime(0, now, 0.04);
      this.wowGain2?.gain.setTargetAtTime(0, now, 0.04);
      if (this.tapeShaper) {
        this.tapeShaper.curve = this.makeSaturationCurve(0);
      }
      this.tapeLowpass?.frequency.setTargetAtTime(20000, now, 0.04);
      this.hissGain?.gain.setTargetAtTime(0, now, 0.05);
    }

    if (this.sourceNode && this.playing) {
      const rate = Math.max(0.5, Math.min(1.5, dsp.speed / 100));
      this.sourceNode.playbackRate.setTargetAtTime(rate, now, 0.04);
    }
  }

  public setVolume(norm: number): void {
    this.volumeNorm = Math.max(0, Math.min(1, norm));
    if (this.masterGain && this.ctx) {
      const gain = this.volumeNorm === 0 ? 0 : Math.pow(this.volumeNorm, 2.1);
      this.masterGain.gain.setTargetAtTime(gain, this.ctx.currentTime, 0.02);
    }
  }

  public playMenuClick(): void {
    if (!this.ctx || this.ctx.state !== 'running') return;
    const now = this.ctx.currentTime;
    const osc = this.ctx.createOscillator();
    const gain = this.ctx.createGain();
    osc.type = 'triangle';
    osc.frequency.setValueAtTime(1750, now);
    osc.frequency.exponentialRampToValueAtTime(520, now + 0.011);
    gain.gain.setValueAtTime(0.09, now);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.014);
    osc.connect(gain).connect(this.ctx.destination);
    osc.start(now);
    osc.stop(now + 0.016);
  }

  public async decodeUserAudio(file: File, trackIndex: number): Promise<number> {
    const ctx = this.ensureContext();
    const arr = await file.arrayBuffer();
    const buf = await ctx.decodeAudioData(arr);
    this.userBuffers.set(trackIndex, buf);
    this.trackCount = Math.max(this.trackCount, trackIndex + 1);
    return Math.round(buf.duration * 1000);
  }

  private async loadTrackBuffer(ctx: AudioContext, trackIndex: number): Promise<AudioBuffer> {
    const userBuf = this.userBuffers.get(trackIndex);
    if (userBuf) return userBuf;

    const idx = trackIndex % BUILTIN_TRACK_URLS.length;
    const cached = this.builtinCache.get(idx);
    if (cached) return cached;

    const inflight = this.builtinLoading.get(idx);
    if (inflight) return inflight;

    const promise = (async () => {
      const resp = await fetch(BUILTIN_TRACK_URLS[idx]);
      if (!resp.ok) {
        throw new Error(`Failed to fetch ${BUILTIN_TRACK_URLS[idx]} (${resp.status})`);
      }
      const arr = await resp.arrayBuffer();
      const buf = await ctx.decodeAudioData(arr);
      this.builtinCache.set(idx, buf);
      this.builtinLoading.delete(idx);
      return buf;
    })();
    this.builtinLoading.set(idx, promise);
    return promise;
  }

  private startDecodedBuffer(
    ctx: AudioContext,
    buf: AudioBuffer,
    trackIndex: number,
    startSec: number,
  ): void {
    this.stopSource(false);
    const isUser = this.userBuffers.has(trackIndex);
    this.currentTrack = trackIndex;
    this.currentDurSec = buf.duration;
    this.offsetSec = Math.max(0, Math.min(startSec, Math.max(0, buf.duration - 0.25)));

    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.loop = !isUser;
    src.connect(this.eqFilters[0]);

    const targetRate = Math.max(0.5, Math.min(1.5, this.dsp.speed / 100));
    const now = ctx.currentTime;
    if (this.dsp.tape && this.dsp.stopStart) {
      src.playbackRate.setValueAtTime(0.32, now);
      src.playbackRate.exponentialRampToValueAtTime(targetRate, now + 0.24);
    } else {
      src.playbackRate.setValueAtTime(targetRate, now);
    }

    const bufOffset = this.offsetSec % Math.max(0.1, buf.duration);
    src.start(now, bufOffset);
    src.onended = () => {
      if (this.sourceNode === src && isUser && this.playing) {
        const next = (this.currentTrack + 1) % Math.max(1, this.trackCount);
        this.playTrack(next, 0);
      }
    };

    this.sourceNode = src;
    this.startCtxTime = now;
    this.playing = true;
    this.applyDsp(this.dsp);
  }

  public playTrack(trackIndex: number, startSec = 0): void {
    const ctx = this.ensureContext();
    const seq = ++this.playSeq;
    this.currentTrack = trackIndex;
    this.offsetSec = startSec;
    this.playing = true;

    const userBuf = this.userBuffers.get(trackIndex);
    if (userBuf) {
      this.startDecodedBuffer(ctx, userBuf, trackIndex, startSec);
      return;
    }

    const idx = trackIndex % BUILTIN_TRACK_URLS.length;
    this.currentDurSec = BUILTIN_TRACK_DURATIONS_SEC[idx] ?? 23.0;
    const cached = this.builtinCache.get(idx);
    if (cached) {
      this.startDecodedBuffer(ctx, cached, trackIndex, startSec);
      return;
    }

    this.stopSource(false);
    void this.loadTrackBuffer(ctx, trackIndex)
      .then((buf) => {
        if (seq !== this.playSeq || !this.playing) return;
        this.startDecodedBuffer(ctx, buf, trackIndex, startSec);
      })
      .catch((err) => {
        console.warn('Audio track load error:', err);
        if (seq === this.playSeq) {
          this.playing = false;
        }
      });
  }

  public setPlaying(shouldPlay: boolean, trackIndex?: number): void {
    if (typeof trackIndex === 'number' && trackIndex !== this.currentTrack) {
      this.currentTrack = trackIndex;
      this.offsetSec = 0;
    }
    if (shouldPlay) {
      if (!this.playing || !this.sourceNode) {
        this.playTrack(this.currentTrack, this.offsetSec);
      }
    } else if (this.playing) {
      this.playSeq++;
      this.offsetSec = this.getPositionSec();
      this.stopSource(this.dsp.tape && this.dsp.stopStart);
      this.playing = false;
      this.applyDsp(this.dsp);
    }
  }

  public seekMs(posMs: number): void {
    this.offsetSec = Math.max(0, Math.min(posMs / 1000, this.currentDurSec));
    if (this.playing) {
      this.playTrack(this.currentTrack, this.offsetSec);
    }
  }

  private stopSource(withTapeSpinDown: boolean): void {
    const src = this.sourceNode;
    this.sourceNode = null;
    if (!src || !this.ctx) return;
    const now = this.ctx.currentTime;
    try {
      if (withTapeSpinDown) {
        src.playbackRate.cancelScheduledValues(now);
        src.playbackRate.setValueAtTime(src.playbackRate.value, now);
        src.playbackRate.exponentialRampToValueAtTime(0.18, now + 0.26);
        src.stop(now + 0.28);
      } else {
        src.stop();
      }
    } catch {
      // Already stopped
    }
  }

  public getPositionSec(): number {
    if (!this.playing || !this.ctx || !this.sourceNode) return this.offsetSec;
    const rate = Math.max(0.5, Math.min(1.5, this.dsp.speed / 100));
    const elapsed = (this.ctx.currentTime - this.startCtxTime) * rate;
    const pos = this.offsetSec + elapsed;
    if (this.currentDurSec <= 0) return 0;
    return pos % this.currentDurSec;
  }

  public getPositionMs(): number {
    return Math.floor(this.getPositionSec() * 1000);
  }

  public getDurationMs(): number {
    return Math.floor(this.currentDurSec * 1000);
  }

  public getSampleRate(): number {
    return this.ctx?.sampleRate ?? 44100;
  }

  public readStereoPcmS16(): Int16Array {
    if (!this.playing || !this.analyserL || !this.analyserR) {
      this.pcmOut.fill(0);
      return this.pcmOut;
    }
    this.analyserL.getFloatTimeDomainData(this.floatBufL);
    this.analyserR.getFloatTimeDomainData(this.floatBufR);
    for (let i = 0; i < 1024; i++) {
      const l = Math.max(-1, Math.min(1, this.floatBufL[i]));
      const r = Math.max(-1, Math.min(1, this.floatBufR[i]));
      this.pcmOut[i * 2] = (l * 32767) | 0;
      this.pcmOut[i * 2 + 1] = (r * 32767) | 0;
    }
    return this.pcmOut;
  }
}
