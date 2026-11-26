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
  fxEnabled?: boolean;
  eq: boolean;
  eqBypass?: boolean;
  autoeq: number;
  eqPreamp?: number;     // tenths of dB (-120..+120)
  eqBands?: number[];    // 10 band gains in tenths of dB (-120..+120)
  tape: boolean;
  wow: number;       // 0..100
  hiss: number;      // 0..100
  sat: number;       // 0..100
  hf: number;        // 0..100
  stopStart: boolean;
  speed: number;     // 50..150 (%)
  reverb?: boolean;
  reverbSize?: number;
  reverbDamp?: number;
  reverbPredelay?: number;
  reverbMix?: number;
  delay?: boolean;
  delayTime?: number;
  delayFeedback?: number;
  delayTone?: number;
  delayMix?: number;
  chorus?: boolean;
  chorusRate?: number;
  chorusDepth?: number;
  chorusMix?: number;
  flanger?: boolean;
  flangerRate?: number;
  flangerDepth?: number;
  flangerFeedback?: number;
  flangerMix?: number;
  djMode?: number;          // 0=Off, 1=Classic, 2=DJ Vinyl, 3=DJ Master Tempo
  djLength?: number;        // 0=4 Bars, 1=8 Bars, 2=12s
  djMaxStretch?: number;    // 0=6%, 1=10%, 2=12%
  djKeepGapless?: boolean;
  djQuickCut?: boolean;
}

export interface DjWasmBridge {
  setConfig: (
    mode: number,
    length: number,
    maxStretch: number,
    keepGapless: boolean,
    quickCut: boolean,
  ) => void;
  beginTransition: (
    sampleRate: number,
    trackA: number,
    tailAS16: Int16Array,
    trackB: number,
    introBS16: Int16Array,
    isBEof: boolean,
    deckBIsNativeNsf: boolean,
    manualQuickCut: boolean,
  ) => boolean;
  feedDeckBS16: (chunkS16: Int16Array, isEof: boolean) => void;
  setTrackTiming: (deckAStartMs: number, deckADurMs: number, deckBDurMs: number) => void;
  getAudibleElapsedMs: (downstreamFrames: number) => number;
  renderS16: (frames: number) => Int16Array;
  takeMidpointCrossed: () => boolean;
  isActive: () => boolean;
  reset: () => void;
  manualBegin: (sampleRate: number) => void;
  manualLoadDeckS16: (
    deck: number,
    trackIndex: number,
    sampleRate: number,
    pcmS16: Int16Array,
    startMs: number,
    startPaused: boolean,
  ) => void;
  manualRenderS16: (frames: number, sampleRate: number) => Int16Array;
  manualRenderF32Planar: (outL: Float32Array, outR: Float32Array, sampleRate: number) => number;
}

export interface StreamingPcmSource {
  open: (sampleRate: number) => number;
  renderS16: (frames: number) => Int16Array;
  seekMs: (targetMs: number) => void;
  elapsedMs: () => number;
  close: () => void;
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

  // Studio FX Web Audio nodes
  private chorusDelay: DelayNode | null = null;
  private chorusOsc: OscillatorNode | null = null;
  private chorusDepthGain: GainNode | null = null;
  private chorusWet: GainNode | null = null;

  private flangerDelay: DelayNode | null = null;
  private flangerOsc: OscillatorNode | null = null;
  private flangerDepthGain: GainNode | null = null;
  private flangerFbGain: GainNode | null = null;
  private flangerWet: GainNode | null = null;

  private echoDelay: DelayNode | null = null;
  private echoFbGain: GainNode | null = null;
  private echoToneLp: BiquadFilterNode | null = null;
  private echoWet: GainNode | null = null;

  private reverbPredelay: DelayNode | null = null;
  private reverbConvolver: ConvolverNode | null = null;
  private reverbDampLp: BiquadFilterNode | null = null;
  private reverbWet: GainNode | null = null;
  private lastReverbSize = -1;

  private splitter: ChannelSplitterNode | null = null;
  private analyserL: AnalyserNode | null = null;
  private analyserR: AnalyserNode | null = null;
  private floatBufL = new Float32Array(1024);
  private floatBufR = new Float32Array(1024);
  private pcmOut = new Int16Array(2048);

  private sourceNode: AudioBufferSourceNode | null = null;
  private sourceGainNode: GainNode | null = null;
  private activeDecodedBuffer: AudioBuffer | null = null;
  private streamNode: ScriptProcessorNode | null = null;
  private activeStreamSource: StreamingPcmSource | null = null;
  public djBridge: DjWasmBridge | null = null;
  private djActive = false;
  private manualDjActive = false;
  private djPreppingSeq = -1;
  private djMidpointDone = false;
  private djDisplayPosSec = 0;
  private seekOsc: OscillatorNode | null = null;
  private seekLfo: OscillatorNode | null = null;
  private seekGain: GainNode | null = null;
  private builtinCache = new Map<number, AudioBuffer>();
  private builtinLoading = new Map<number, Promise<AudioBuffer>>();
  private userBuffers = new Map<number, AudioBuffer>();
  private lazyPcmLoaders = new Map<
    number,
    { durSec: number; sampleRate: number; renderS16: () => Int16Array }
  >();
  private streamingTracks = new Map<
    number,
    { initialDurSec: number; source: StreamingPcmSource }
  >();
  public onNonStreamingTrack: (() => void) | null = null;
  public onTrackAutoAdvance: ((nextIndex: number) => void) | null = null;
  public onTrackDecoded: ((trackIndex: number, buf: AudioBuffer) => void) | null = null;
  public onStreamingTrackStarted: ((trackIndex: number) => void) | null = null;
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
    djMode: 0,
    djLength: 1,
    djMaxStretch: 1,
    djKeepGapless: true,
    djQuickCut: true,
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

    // 1. Chorus send/insert
    const chDelay = ctx.createDelay(0.05);
    chDelay.delayTime.value = 0.015;
    const chOsc = ctx.createOscillator();
    chOsc.frequency.value = 0.8;
    const chDepth = ctx.createGain();
    chDepth.gain.value = 0;
    const chWet = ctx.createGain();
    chWet.gain.value = 0;
    chOsc.connect(chDepth).connect(chDelay.delayTime);
    chOsc.start();
    this.chorusDelay = chDelay;
    this.chorusOsc = chOsc;
    this.chorusDepthGain = chDepth;
    this.chorusWet = chWet;

    // 2. Flanger send
    const flDelay = ctx.createDelay(0.02);
    flDelay.delayTime.value = 0.003;
    const flOsc = ctx.createOscillator();
    flOsc.frequency.value = 0.35;
    const flDepth = ctx.createGain();
    flDepth.gain.value = 0;
    const flFb = ctx.createGain();
    flFb.gain.value = 0;
    const flWet = ctx.createGain();
    flWet.gain.value = 0;
    flOsc.connect(flDepth).connect(flDelay.delayTime);
    flDelay.connect(flFb).connect(flDelay);
    flOsc.start();
    this.flangerDelay = flDelay;
    this.flangerOsc = flOsc;
    this.flangerDepthGain = flDepth;
    this.flangerFbGain = flFb;
    this.flangerWet = flWet;

    // 3. Stereo Delay send
    const ecDelay = ctx.createDelay(1.5);
    ecDelay.delayTime.value = 0.36;
    const ecTone = ctx.createBiquadFilter();
    ecTone.type = 'lowpass';
    ecTone.frequency.value = 5200;
    const ecFb = ctx.createGain();
    ecFb.gain.value = 0;
    const ecWet = ctx.createGain();
    ecWet.gain.value = 0;
    ecDelay.connect(ecTone);
    ecTone.connect(ecFb).connect(ecDelay);
    this.echoDelay = ecDelay;
    this.echoToneLp = ecTone;
    this.echoFbGain = ecFb;
    this.echoWet = ecWet;

    // 4. Studio Reverb send
    const rvPre = ctx.createDelay(0.2);
    rvPre.delayTime.value = 0.02;
    const rvConv = ctx.createConvolver();
    rvConv.buffer = this.makeReverbImpulse(ctx, 65);
    this.lastReverbSize = 65;
    const rvDamp = ctx.createBiquadFilter();
    rvDamp.type = 'lowpass';
    rvDamp.frequency.value = 6500;
    const rvWet = ctx.createGain();
    rvWet.gain.value = 0;
    rvPre.connect(rvConv).connect(rvDamp).connect(rvWet);
    this.reverbPredelay = rvPre;
    this.reverbConvolver = rvConv;
    this.reverbDampLp = rvDamp;
    this.reverbWet = rvWet;

    this.eqFilters[this.eqFilters.length - 1].connect(delay);
    delay.connect(shaper);
    shaper.connect(lp);

    // Dry + Chorus + Flanger + Delay + Reverb summed into preTap (so visualizers react to FX too!)
    lp.connect(preTap);
    lp.connect(chDelay);
    chDelay.connect(chWet).connect(preTap);
    lp.connect(flDelay);
    flDelay.connect(flWet).connect(preTap);
    lp.connect(ecDelay);
    ecTone.connect(ecWet).connect(preTap);
    lp.connect(rvPre);
    rvWet.connect(preTap);

    preTap.connect(splitter);
    splitter.connect(analyserL, 0);
    splitter.connect(analyserR, 1);
    preTap.connect(master);
    master.connect(ctx.destination);

    this.applyDsp(this.dsp);
    return ctx;
  }

  private makeReverbImpulse(ctx: AudioContext, sizePct: number): AudioBuffer {
    const dur = 0.6 + (Math.max(0, Math.min(100, sizePct)) / 100) * 3.2;
    const len = Math.max(1024, Math.floor(ctx.sampleRate * dur));
    const buf = ctx.createBuffer(2, len, ctx.sampleRate);
    for (let ch = 0; ch < 2; ch++) {
      const d = buf.getChannelData(ch);
      for (let i = 0; i < len; i++) {
        const env = Math.pow(1 - i / len, 2.6);
        d[i] = (Math.random() * 2 - 1) * env * 0.42;
      }
    }
    return buf;
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
    const merged: DspParams = { ...this.dsp, ...dsp };
    this.dsp = merged;
    if (typeof dsp.djMode === 'number' && this.djBridge) {
      this.djBridge.setConfig(
        merged.djMode ?? 0,
        merged.djLength ?? 1,
        merged.djMaxStretch ?? 1,
        merged.djKeepGapless !== false,
        merged.djQuickCut !== false,
      );
      if ((merged.djMode ?? 0) === 0 && this.djActive) {
        this.djActive = false;
      }
    }
    if (this.sourceNode) {
      this.sourceNode.loop = this.trackCount < 2;
    }
    if (!this.ctx) return;
    const now = this.ctx.currentTime;

    const eqActive = dsp.eq && !dsp.eqBypass;
    const curve = eqActive
      ? dsp.eqBands && dsp.eqBands.length === 10
        ? dsp.eqBands.map((t) => t / 10)
        : AUTOEQ_CURVES[dsp.autoeq % AUTOEQ_CURVES.length]
      : AUTOEQ_CURVES[0];
    this.eqFilters.forEach((f, i) => {
      f.gain.setTargetAtTime(curve[i] ?? 0, now, 0.02);
    });
    const preampDb = eqActive && typeof dsp.eqPreamp === 'number' ? dsp.eqPreamp / 10 : 0;
    this.preTapGain?.gain.setTargetAtTime(Math.pow(10, preampDb / 20), now, 0.02);

    const fxOn = dsp.fxEnabled !== false;

    if (fxOn && dsp.tape) {
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

    // Chorus
    if (fxOn && dsp.chorus) {
      const r = (dsp.chorusRate ?? 35) / 100;
      const d = (dsp.chorusDepth ?? 50) / 100;
      const m = (dsp.chorusMix ?? 40) / 100;
      this.chorusOsc?.frequency.setTargetAtTime(0.15 + 3.0 * r * r, now, 0.04);
      this.chorusDepthGain?.gain.setTargetAtTime(d * 0.006, now, 0.04);
      this.chorusWet?.gain.setTargetAtTime(m * 0.85, now, 0.04);
    } else {
      this.chorusWet?.gain.setTargetAtTime(0, now, 0.03);
    }

    // Flanger
    if (fxOn && dsp.flanger) {
      const r = (dsp.flangerRate ?? 25) / 100;
      const d = (dsp.flangerDepth ?? 60) / 100;
      const fb = Math.max(-0.88, Math.min(0.88, ((dsp.flangerFeedback ?? 60) / 100) * 0.92));
      const m = (dsp.flangerMix ?? 45) / 100;
      this.flangerOsc?.frequency.setTargetAtTime(0.08 + 2.4 * r * r, now, 0.04);
      this.flangerDepthGain?.gain.setTargetAtTime(d * 0.0028, now, 0.04);
      this.flangerFbGain?.gain.setTargetAtTime(fb, now, 0.04);
      this.flangerWet?.gain.setTargetAtTime(m * 0.85, now, 0.04);
    } else {
      this.flangerFbGain?.gain.setTargetAtTime(0, now, 0.03);
      this.flangerWet?.gain.setTargetAtTime(0, now, 0.03);
    }

    // Delay
    if (fxOn && dsp.delay) {
      const tSec = Math.max(0.04, Math.min(1.2, (dsp.delayTime ?? 360) / 1000));
      const fb = Math.max(0, Math.min(0.9, (dsp.delayFeedback ?? 45) / 100));
      const tone = (dsp.delayTone ?? 60) / 100;
      const m = (dsp.delayMix ?? 35) / 100;
      this.echoDelay?.delayTime.setTargetAtTime(tSec, now, 0.05);
      this.echoFbGain?.gain.setTargetAtTime(fb, now, 0.04);
      this.echoToneLp?.frequency.setTargetAtTime(1100 * Math.pow(12.5, tone), now, 0.04);
      this.echoWet?.gain.setTargetAtTime(m * 0.9, now, 0.04);
    } else {
      this.echoFbGain?.gain.setTargetAtTime(0, now, 0.03);
      this.echoWet?.gain.setTargetAtTime(0, now, 0.03);
    }

    // Reverb
    if (fxOn && dsp.reverb) {
      const sz = Math.round((dsp.reverbSize ?? 65) / 5) * 5;
      if (this.reverbConvolver && Math.abs(sz - this.lastReverbSize) >= 5) {
        this.reverbConvolver.buffer = this.makeReverbImpulse(this.ctx, sz);
        this.lastReverbSize = sz;
      }
      const damp = (dsp.reverbDamp ?? 45) / 100;
      const pre = Math.max(0, Math.min(0.15, (dsp.reverbPredelay ?? 20) / 1000));
      const m = (dsp.reverbMix ?? 30) / 100;
      this.reverbPredelay?.delayTime.setTargetAtTime(pre, now, 0.03);
      this.reverbDampLp?.frequency.setTargetAtTime(14000 * Math.pow(0.12, damp), now, 0.04);
      this.reverbWet?.gain.setTargetAtTime(m * 0.95, now, 0.04);
    } else {
      this.reverbWet?.gain.setTargetAtTime(0, now, 0.03);
    }

    if (this.sourceNode && this.playing) {
      const rate = fxOn ? Math.max(0.5, Math.min(1.5, dsp.speed / 100)) : 1.0;
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

  public registerLazyPcmTrack(
    trackIndex: number,
    durationMs: number,
    sampleRate: number,
    renderS16: () => Int16Array,
  ): void {
    this.lazyPcmLoaders.set(trackIndex, {
      durSec: Math.max(0.1, durationMs / 1000),
      sampleRate,
      renderS16,
    });
    this.trackCount = Math.max(this.trackCount, trackIndex + 1);
  }

  public registerStreamingPcmTrack(
    trackIndex: number,
    durationMs: number,
    source: StreamingPcmSource,
  ): void {
    this.streamingTracks.set(trackIndex, {
      initialDurSec: Math.max(0.4, durationMs / 1000),
      source,
    });
    this.trackCount = Math.max(this.trackCount, trackIndex + 1);
  }

  private async loadTrackBuffer(ctx: AudioContext, trackIndex: number): Promise<AudioBuffer> {
    const userBuf = this.userBuffers.get(trackIndex);
    if (userBuf) return userBuf;

    const lazy = this.lazyPcmLoaders.get(trackIndex);
    if (lazy) {
      const s16 = lazy.renderS16();
      const frames = Math.floor(s16.length / 2);
      if (frames <= 0) {
        throw new Error('GSF track produced no audio samples (missing .gsflib?)');
      }
      const buf = ctx.createBuffer(2, frames, lazy.sampleRate);
      const chL = buf.getChannelData(0);
      const chR = buf.getChannelData(1);
      const inv = 1.0 / 32768.0;
      for (let i = 0; i < frames; i++) {
        chL[i] = s16[i * 2] * inv;
        chR[i] = s16[i * 2 + 1] * inv;
      }
      this.userBuffers.set(trackIndex, buf);
      return buf;
    }

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

  private extractBufferS16AtRate(
    buf: AudioBuffer,
    targetRate: number,
    startSec: number,
    maxSec: number,
  ): { s16: Int16Array; consumedSec: number; reachedEnd: boolean } {
    const srcRate = buf.sampleRate;
    const srcStart = Math.max(0, Math.min(buf.length, Math.floor(startSec * srcRate)));
    const srcAvail = Math.max(0, buf.length - srcStart);
    const availSec = srcAvail / srcRate;
    const takeSec = Math.max(0, Math.min(availSec, maxSec));
    const outFrames = Math.max(0, Math.floor(takeSec * targetRate));
    const out = new Int16Array(outFrames * 2);
    if (outFrames === 0 || buf.numberOfChannels === 0) {
      return { s16: out, consumedSec: 0, reachedEnd: true };
    }

    const chL = buf.getChannelData(0);
    const chR = buf.numberOfChannels > 1 ? buf.getChannelData(1) : chL;
    if (srcRate === targetRate) {
      for (let i = 0; i < outFrames; i++) {
        const idx = Math.min(buf.length - 1, srcStart + i);
        const l = Math.max(-1, Math.min(1, chL[idx]));
        const r = Math.max(-1, Math.min(1, chR[idx]));
        out[i * 2] = (l * 32767) | 0;
        out[i * 2 + 1] = (r * 32767) | 0;
      }
    } else {
      const step = srcRate / targetRate;
      for (let i = 0; i < outFrames; i++) {
        const pos = srcStart + i * step;
        const idx0 = Math.min(buf.length - 1, Math.floor(pos));
        const idx1 = Math.min(buf.length - 1, idx0 + 1);
        const frac = pos - idx0;
        const l = Math.max(-1, Math.min(1, chL[idx0] * (1 - frac) + chL[idx1] * frac));
        const r = Math.max(-1, Math.min(1, chR[idx0] * (1 - frac) + chR[idx1] * frac));
        out[i * 2] = (l * 32767) | 0;
        out[i * 2 + 1] = (r * 32767) | 0;
      }
    }

    const consumedSrc = Math.ceil(takeSec * srcRate);
    return {
      s16: out,
      consumedSec: outFrames / targetRate,
      reachedEnd: srcStart + consumedSrc >= buf.length - 2,
    };
  }

  public tickLookahead(): void {
    if (
      !this.playing ||
      this.djActive ||
      this.manualDjActive ||
      !this.djBridge ||
      (this.dsp.djMode ?? 0) === 0 ||
      this.trackCount < 2 ||
      !this.ctx
    ) {
      return;
    }
    const posSec = this.getPositionSec();
    const remSec = this.currentDurSec - posSec;
    if (
      this.currentDurSec >= 8.0 &&
      remSec > 0.35 &&
      remSec <= 17.5 &&
      this.djPreppingSeq !== this.playSeq
    ) {
      this.djPreppingSeq = this.playSeq;
      const nextIdx = (this.currentTrack + 1) % Math.max(1, this.trackCount);
      void this.beginDjTransitionInternal(nextIdx, false);
    }
  }

  public quickCutToTrack(nextIdx: number): void {
    if (
      !this.playing ||
      !this.djBridge ||
      (this.dsp.djMode ?? 0) === 0 ||
      this.djActive ||
      this.manualDjActive ||
      !this.ctx
    ) {
      this.playTrack(nextIdx, 0);
      this.onTrackAutoAdvance?.(nextIdx);
      return;
    }
    this.djPreppingSeq = this.playSeq;
    void this.beginDjTransitionInternal(nextIdx, true);
  }

  private async beginDjTransitionInternal(
    nextIdx: number,
    manualQuickCut: boolean,
  ): Promise<void> {
    const ctx = this.ensureContext();
    const seq = this.playSeq;
    const trackA = this.currentTrack;
    const djBridge = this.djBridge;
    if (!djBridge) return;

    const streamEntryB = this.streamingTracks.get(nextIdx) ?? null;
    let bufB: AudioBuffer | null = null;
    if (!streamEntryB) {
      try {
        bufB = await this.loadTrackBuffer(ctx, nextIdx);
      } catch (err) {
        console.warn('DJ Deck B preload error:', err);
        if (manualQuickCut && seq === this.playSeq) {
          this.playTrack(nextIdx, 0);
          this.onTrackAutoAdvance?.(nextIdx);
        }
        return;
      }
    }

    if (seq !== this.playSeq || !this.playing || this.djActive) {
      return;
    }

    const posA = this.getPositionSec();
    const maxTailSec = manualQuickCut ? 2.4 : 17.5;
    let tailAS16: Int16Array;

    if (this.activeDecodedBuffer && this.sourceNode) {
      tailAS16 = this.extractBufferS16AtRate(
        this.activeDecodedBuffer,
        ctx.sampleRate,
        posA,
        maxTailSec,
      ).s16;
    } else if (this.activeStreamSource) {
      const remSec = Math.max(0.5, Math.min(maxTailSec, this.currentDurSec - posA));
      const framesA = Math.floor(remSec * ctx.sampleRate);
      tailAS16 = this.activeStreamSource.renderS16(framesA);
      this.activeStreamSource.close();
      this.activeStreamSource = null;
    } else {
      if (manualQuickCut) {
        this.playTrack(nextIdx, 0);
        this.onTrackAutoAdvance?.(nextIdx);
      }
      return;
    }

    let introBS16: Int16Array;
    let isBEof = true;
    let deckBIsNativeNsf = false;
    let handoffSecB = 0;
    let durSecB = 23.0;
    let liveStreamB: StreamingPcmSource | null = null;

    if (streamEntryB) {
      const durMsB = streamEntryB.source.open(ctx.sampleRate);
      durSecB = durMsB > 0 ? durMsB / 1000 : streamEntryB.initialDurSec;
      const introSec = manualQuickCut ? 3.5 : 13.5;
      introBS16 = streamEntryB.source.renderS16(Math.floor(introSec * ctx.sampleRate));
      const gotSec = introBS16.length / 2 / ctx.sampleRate;
      isBEof = gotSec + 0.05 >= durSecB;
      deckBIsNativeNsf = true;
      liveStreamB = streamEntryB.source;
      handoffSecB = gotSec;
    } else if (bufB) {
      this.onNonStreamingTrack?.();
      durSecB = bufB.duration;
      const introWindowSec = manualQuickCut
        ? Math.min(8.0, bufB.duration)
        : Math.min(20.0, bufB.duration);
      const extractedB = this.extractBufferS16AtRate(bufB, ctx.sampleRate, 0, introWindowSec);
      introBS16 = extractedB.s16;
      handoffSecB = extractedB.consumedSec;
      isBEof = true;
    } else {
      return;
    }

    const durAMs = Math.round(this.currentDurSec * 1000);
    const startAMs = Math.round(posA * 1000);
    const durBMs = Math.round(durSecB * 1000);

    djBridge.setTrackTiming(startAMs, durAMs, durBMs);
    const armed = djBridge.beginTransition(
      ctx.sampleRate,
      trackA,
      tailAS16,
      nextIdx,
      introBS16,
      isBEof,
      deckBIsNativeNsf,
      manualQuickCut,
    );
    if (!armed) {
      liveStreamB?.close();
      if (manualQuickCut) {
        this.playTrack(nextIdx, 0);
        this.onTrackAutoAdvance?.(nextIdx);
      }
      return;
    }
    djBridge.setTrackTiming(startAMs, durAMs, durBMs);

    this.stopSource(false, false);
    this.djActive = true;
    this.djMidpointDone = false;
    this.djDisplayPosSec = posA;

    const proc = ctx.createScriptProcessor(2048, 0, 2);
    let streamBEof = isBEof;
    let handoffQueued = false;

    proc.onaudioprocess = (ev: AudioProcessingEvent) => {
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      const frames = outL.length;
      if (!this.playing || this.streamNode !== proc || !this.djBridge) {
        outL.fill(0);
        outR.fill(0);
        return;
      }

      if (liveStreamB && !streamBEof) {
        const chunkB = liveStreamB.renderS16(frames * 2);
        if (chunkB.length === 0) {
          streamBEof = true;
          this.djBridge.feedDeckBS16(chunkB, true);
        } else {
          this.djBridge.feedDeckBS16(chunkB, false);
        }
      }

      const s16 = this.djBridge.renderS16(frames);
      const gotFrames = Math.floor(s16.length / 2);
      const inv = 1.0 / 32768.0;

      for (let i = 0; i < Math.min(frames, gotFrames); i++) {
        outL[i] = s16[i * 2] * inv;
        outR[i] = s16[i * 2 + 1] * inv;
      }

      if (!this.djMidpointDone && this.djBridge.takeMidpointCrossed()) {
        this.djMidpointDone = true;
        this.currentTrack = nextIdx;
        this.currentDurSec = durSecB;
        setTimeout(() => {
          if (this.playing) {
            this.onTrackAutoAdvance?.(nextIdx);
          }
        }, 0);
      }

      this.djDisplayPosSec = this.djBridge.getAudibleElapsedMs(frames) / 1000;

      if (gotFrames < frames || !this.djBridge.isActive()) {
        const remOut = frames - gotFrames;
        if (remOut > 0 && bufB && handoffSecB < bufB.duration) {
          const tailFill = this.extractBufferS16AtRate(
            bufB,
            ctx.sampleRate,
            handoffSecB,
            remOut / ctx.sampleRate,
          );
          const fillN = Math.min(remOut, Math.floor(tailFill.s16.length / 2));
          for (let j = 0; j < fillN; j++) {
            outL[gotFrames + j] = tailFill.s16[j * 2] * inv;
            outR[gotFrames + j] = tailFill.s16[j * 2 + 1] * inv;
          }
          for (let j = gotFrames + fillN; j < frames; j++) {
            outL[j] = 0;
            outR[j] = 0;
          }
          handoffSecB += tailFill.consumedSec;
        } else {
          for (let j = gotFrames; j < frames; j++) {
            outL[j] = 0;
            outR[j] = 0;
          }
        }

        if (!handoffQueued) {
          handoffQueued = true;
          setTimeout(() => {
            if (this.streamNode !== proc || !this.playing) return;
            this.djActive = false;
            if (!this.djMidpointDone) {
              this.djMidpointDone = true;
              this.currentTrack = nextIdx;
              this.onTrackAutoAdvance?.(nextIdx);
            }
            if (liveStreamB && streamEntryB) {
              if (streamBEof) {
                const afterB = (nextIdx + 1) % Math.max(1, this.trackCount);
                this.playTrack(afterB, 0);
                this.onTrackAutoAdvance?.(afterB);
              } else {
                this.continueOpenStreamingTrack(ctx, nextIdx, streamEntryB, liveStreamB, durSecB);
              }
            } else if (bufB) {
              if (handoffSecB + 0.15 >= bufB.duration) {
                const afterB = (nextIdx + 1) % Math.max(1, this.trackCount);
                this.playTrack(afterB, 0);
                this.onTrackAutoAdvance?.(afterB);
              } else {
                this.startDecodedBuffer(ctx, bufB, nextIdx, handoffSecB, true);
              }
            }
          }, 0);
        }
      }
    };

    proc.connect(this.eqFilters[0]);
    this.streamNode = proc;
    this.activeStreamSource = liveStreamB;
    this.playing = true;
  }

  private continueOpenStreamingTrack(
    ctx: AudioContext,
    trackIndex: number,
    entry: { initialDurSec: number; source: StreamingPcmSource },
    openSource: StreamingPcmSource,
    durSec: number,
  ): void {
    if (this.streamNode) {
      const oldProc = this.streamNode;
      this.streamNode = null;
      try {
        oldProc.onaudioprocess = null;
        oldProc.disconnect();
      } catch {
        // ignore
      }
    }
    this.currentTrack = trackIndex;
    this.currentDurSec = durSec > 0 ? durSec : entry.initialDurSec;
    this.offsetSec = openSource.elapsedMs() / 1000;

    const proc = ctx.createScriptProcessor(2048, 0, 2);
    let endedQueued = false;
    proc.onaudioprocess = (ev: AudioProcessingEvent) => {
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      const frames = outL.length;
      if (!this.playing || this.streamNode !== proc) {
        outL.fill(0);
        outR.fill(0);
        return;
      }
      const fxOn = this.dsp.fxEnabled !== false;
      const rate = fxOn ? Math.max(0.5, Math.min(1.5, this.dsp.speed / 100)) : 1.0;
      const pullFrames = rate === 1.0 ? frames : Math.max(1, Math.round(frames * rate));
      const s16 = openSource.renderS16(pullFrames);
      const gotFrames = Math.floor(s16.length / 2);
      if (gotFrames <= 0) {
        outL.fill(0);
        outR.fill(0);
        if (!endedQueued && this.playing && this.streamNode === proc) {
          endedQueued = true;
          setTimeout(() => {
            if (this.streamNode === proc && this.playing) {
              const next = (this.currentTrack + 1) % Math.max(1, this.trackCount);
              this.playTrack(next, 0);
              this.onTrackAutoAdvance?.(next);
            }
          }, 0);
        }
        return;
      }
      const inv = 1.0 / 32768.0;
      if (gotFrames === frames) {
        for (let i = 0; i < frames; i++) {
          outL[i] = s16[i * 2] * inv;
          outR[i] = s16[i * 2 + 1] * inv;
        }
      } else {
        const scale = (gotFrames - 1) / Math.max(1, frames - 1);
        for (let i = 0; i < frames; i++) {
          const srcIdx = Math.min(gotFrames - 1, Math.floor(i * scale));
          outL[i] = s16[srcIdx * 2] * inv;
          outR[i] = s16[srcIdx * 2 + 1] * inv;
        }
      }
    };
    proc.connect(this.eqFilters[0]);
    this.streamNode = proc;
    this.activeStreamSource = openSource;
    this.startCtxTime = ctx.currentTime;
    this.playing = true;
    this.applyDsp(this.dsp);
  }

  private preloadNeighborTracks(ctx: AudioContext, trackIndex: number): void {
    if (this.trackCount < 2) return;
    const neighbors = [
      (trackIndex + 1) % this.trackCount,
      (trackIndex - 1 + this.trackCount) % this.trackCount,
    ];
    for (const nIdx of neighbors) {
      if (this.streamingTracks.has(nIdx) || this.userBuffers.has(nIdx)) continue;
      if (this.lazyPcmLoaders.has(nIdx)) continue;
      const bIdx = nIdx % BUILTIN_TRACK_URLS.length;
      if (!this.builtinCache.has(bIdx) && !this.builtinLoading.has(bIdx)) {
        void this.loadTrackBuffer(ctx, nIdx).catch(() => {});
      }
    }
  }

  private startDecodedBuffer(
    ctx: AudioContext,
    buf: AudioBuffer,
    trackIndex: number,
    startSec: number,
    skipMotorRamp = false,
  ): void {
    this.stopSource(false);
    this.activeDecodedBuffer = buf;
    this.currentTrack = trackIndex;
    this.currentDurSec = buf.duration;
    this.offsetSec = Math.max(0, Math.min(startSec, Math.max(0, buf.duration - 0.05)));

    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.loop = this.trackCount < 2;

    const voiceGain = ctx.createGain();
    const now = ctx.currentTime;
    if (skipMotorRamp) {
      voiceGain.gain.setValueAtTime(1.0, now);
    } else {
      // 5 ms anti-click fade-in on manual / cold track start
      voiceGain.gain.setValueAtTime(0.0, now);
      voiceGain.gain.linearRampToValueAtTime(1.0, now + 0.005);
    }
    src.connect(voiceGain).connect(this.eqFilters[0]);

    const fxOn = this.dsp.fxEnabled !== false;
    const targetRate = fxOn ? Math.max(0.5, Math.min(1.5, this.dsp.speed / 100)) : 1.0;
    if (!skipMotorRamp && fxOn && this.dsp.tape && this.dsp.stopStart) {
      src.playbackRate.setValueAtTime(0.32, now);
      src.playbackRate.exponentialRampToValueAtTime(targetRate, now + 0.24);
    } else {
      src.playbackRate.setValueAtTime(targetRate, now);
    }

    const bufOffset = this.offsetSec % Math.max(0.05, buf.duration);
    src.start(now, bufOffset);
    src.onended = () => {
      if (this.sourceNode === src && this.trackCount >= 2 && this.playing && !this.djActive) {
        const next = (this.currentTrack + 1) % Math.max(1, this.trackCount);
        this.playTrack(next, 0, true);
        this.onTrackAutoAdvance?.(next);
      }
    };

    this.sourceNode = src;
    this.sourceGainNode = voiceGain;
    this.startCtxTime = now;
    this.playing = true;
    this.applyDsp(this.dsp);
    this.onTrackDecoded?.(trackIndex, buf);
    this.preloadNeighborTracks(ctx, trackIndex);
  }

  private startStreamingTrack(
    ctx: AudioContext,
    trackIndex: number,
    entry: { initialDurSec: number; source: StreamingPcmSource },
    startSec: number,
  ): void {
    this.stopSource(false);
    const durMs = entry.source.open(ctx.sampleRate);
    this.currentTrack = trackIndex;
    this.currentDurSec = durMs > 0 ? durMs / 1000 : entry.initialDurSec;
    this.offsetSec = Math.max(0, Math.min(startSec, Math.max(0, this.currentDurSec - 0.1)));
    if (this.offsetSec > 0.005) {
      entry.source.seekMs(Math.round(this.offsetSec * 1000));
    }

    const proc = ctx.createScriptProcessor(2048, 0, 2);
    let endedQueued = false;
    proc.onaudioprocess = (ev: AudioProcessingEvent) => {
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      const frames = outL.length;
      if (!this.playing || this.streamNode !== proc) {
        outL.fill(0);
        outR.fill(0);
        return;
      }

      const fxOn = this.dsp.fxEnabled !== false;
      const rate = fxOn ? Math.max(0.5, Math.min(1.5, this.dsp.speed / 100)) : 1.0;
      const pullFrames = rate === 1.0 ? frames : Math.max(1, Math.round(frames * rate));
      const s16 = entry.source.renderS16(pullFrames);
      const gotFrames = Math.floor(s16.length / 2);

      if (gotFrames <= 0) {
        outL.fill(0);
        outR.fill(0);
        if (!endedQueued && this.playing && this.streamNode === proc) {
          endedQueued = true;
          setTimeout(() => {
            if (this.streamNode === proc && this.playing) {
              const next = (this.currentTrack + 1) % Math.max(1, this.trackCount);
              this.playTrack(next, 0);
              this.onTrackAutoAdvance?.(next);
            }
          }, 0);
        }
        return;
      }

      const inv = 1.0 / 32768.0;
      if (gotFrames === frames) {
        for (let i = 0; i < frames; i++) {
          outL[i] = s16[i * 2] * inv;
          outR[i] = s16[i * 2 + 1] * inv;
        }
      } else {
        const scale = (gotFrames - 1) / Math.max(1, frames - 1);
        for (let i = 0; i < frames; i++) {
          const srcIdx = Math.min(gotFrames - 1, Math.floor(i * scale));
          outL[i] = s16[srcIdx * 2] * inv;
          outR[i] = s16[srcIdx * 2 + 1] * inv;
        }
      }
    };

    proc.connect(this.eqFilters[0]);
    this.streamNode = proc;
    this.activeStreamSource = entry.source;
    this.startCtxTime = ctx.currentTime;
    this.playing = true;
    this.applyDsp(this.dsp);
    this.onStreamingTrackStarted?.(trackIndex);
  }

  public playTrack(trackIndex: number, startSec = 0, seamlessGapless = false): void {
    const ctx = this.ensureContext();
    const seq = ++this.playSeq;
    this.currentTrack = trackIndex;
    this.offsetSec = startSec;
    this.playing = true;

    const streaming = this.streamingTracks.get(trackIndex);
    if (streaming) {
      this.startStreamingTrack(ctx, trackIndex, streaming, startSec);
      this.preloadNeighborTracks(ctx, trackIndex);
      return;
    }

    // Switching away from an NSF track to a standard/non-NSF track: reset NSF runtime mask!
    this.onNonStreamingTrack?.();

    const userBuf = this.userBuffers.get(trackIndex);
    if (userBuf) {
      this.startDecodedBuffer(ctx, userBuf, trackIndex, startSec, seamlessGapless);
      return;
    }

    const lazy = this.lazyPcmLoaders.get(trackIndex);
    if (lazy) {
      this.currentDurSec = lazy.durSec;
      this.stopSource(false);
      void this.loadTrackBuffer(ctx, trackIndex)
        .then((buf) => {
          if (seq !== this.playSeq || !this.playing) return;
          this.startDecodedBuffer(ctx, buf, trackIndex, startSec, seamlessGapless);
        })
        .catch((err) => {
          console.warn('GSF track decode error:', err);
          if (seq === this.playSeq) {
            this.playing = false;
          }
        });
      return;
    }

    const idx = trackIndex % BUILTIN_TRACK_URLS.length;
    this.currentDurSec = BUILTIN_TRACK_DURATIONS_SEC[idx] ?? 23.0;
    const cached = this.builtinCache.get(idx);
    if (cached) {
      this.startDecodedBuffer(ctx, cached, trackIndex, startSec, seamlessGapless);
      return;
    }

    // Keep outgoing source fading cleanly; do not kill AudioContext while buffer decodes
    this.stopSource(false);
    void this.loadTrackBuffer(ctx, trackIndex)
      .then((buf) => {
        if (seq !== this.playSeq || !this.playing) return;
        this.startDecodedBuffer(ctx, buf, trackIndex, startSec, seamlessGapless);
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
      if (!this.playing || (!this.sourceNode && !this.streamNode)) {
        this.playTrack(this.currentTrack, this.offsetSec);
      }
    } else if (this.playing) {
      this.playSeq++;
      this.offsetSec = this.getPositionSec();
      this.stopSource(this.dsp.fxEnabled !== false && this.dsp.tape && this.dsp.stopStart);
      this.playing = false;
      this.applyDsp(this.dsp);
    }
  }

  public seekMs(posMs: number): void {
    const clampedMs = Math.max(0, Math.min(posMs, this.currentDurSec * 1000));
    this.offsetSec = clampedMs / 1000;
    if (this.djActive) {
      this.djBridge?.reset();
      this.djActive = false;
      this.djPreppingSeq = -1;
      if (this.playing) {
        this.playTrack(this.currentTrack, this.offsetSec);
      }
      return;
    }
    if (this.playing && this.streamNode && this.activeStreamSource) {
      this.activeStreamSource.seekMs(Math.round(clampedMs));
      return;
    }
    if (this.playing) {
      this.playTrack(this.currentTrack, this.offsetSec);
    }
  }

  public setSeekMotor(dir: number, enabled: boolean): void {
    if (dir === 0 || !enabled) {
      if (this.seekGain && this.ctx) {
        const now = this.ctx.currentTime;
        this.seekGain.gain.setTargetAtTime(0.0001, now, 0.02);
      }
      if (this.seekOsc) {
        try {
          this.seekOsc.stop((this.ctx?.currentTime ?? 0) + 0.07);
        } catch {
          // ignore
        }
        this.seekOsc = null;
      }
      if (this.seekLfo) {
        try {
          this.seekLfo.stop((this.ctx?.currentTime ?? 0) + 0.07);
        } catch {
          // ignore
        }
        this.seekLfo = null;
      }
      this.seekGain = null;
      return;
    }

    const ctx = this.ensureContext();
    if (this.seekOsc) return;
    const now = ctx.currentTime;
    const osc = ctx.createOscillator();
    const lfo = ctx.createOscillator();
    const lfoGain = ctx.createGain();
    const bp = ctx.createBiquadFilter();
    const gain = ctx.createGain();

    osc.type = 'sawtooth';
    osc.frequency.setValueAtTime(dir > 0 ? 410 : 340, now);
    osc.frequency.exponentialRampToValueAtTime(dir > 0 ? 640 : 520, now + 0.9);

    lfo.type = 'sine';
    lfo.frequency.setValueAtTime(22, now);
    lfoGain.gain.setValueAtTime(38, now);

    bp.type = 'bandpass';
    bp.frequency.setValueAtTime(1150, now);
    bp.Q.setValueAtTime(2.2, now);

    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.linearRampToValueAtTime(0.032 * 0.40 * this.volumeNorm, now + 0.022);

    lfo.connect(lfoGain).connect(osc.frequency);
    osc.connect(bp).connect(gain).connect(ctx.destination);
    osc.start(now);
    lfo.start(now);

    this.seekOsc = osc;
    this.seekLfo = lfo;
    this.seekGain = gain;
  }

  private stopSource(withTapeSpinDown: boolean, resetDj = true): void {
    if (resetDj) {
      this.djActive = false;
      this.djPreppingSeq = -1;
      this.djBridge?.reset();
    }
    this.activeDecodedBuffer = null;
    if (this.streamNode) {
      const proc = this.streamNode;
      this.streamNode = null;
      try {
        proc.onaudioprocess = null;
        proc.disconnect();
      } catch {
        // Already disconnected
      }
    }
    if (this.activeStreamSource) {
      const st = this.activeStreamSource;
      this.activeStreamSource = null;
      st.close();
    }

    const src = this.sourceNode;
    const vGain = this.sourceGainNode;
    this.sourceNode = null;
    this.sourceGainNode = null;
    if (!src || !this.ctx) return;
    const now = this.ctx.currentTime;
    try {
      src.onended = null;
      if (withTapeSpinDown) {
        src.playbackRate.cancelScheduledValues(now);
        src.playbackRate.setValueAtTime(src.playbackRate.value, now);
        src.playbackRate.exponentialRampToValueAtTime(0.18, now + 0.26);
        src.stop(now + 0.28);
        setTimeout(() => {
          try {
            vGain?.disconnect();
          } catch {
            // ignore
          }
        }, 320);
      } else if (vGain) {
        // 6 ms anti-click fade-out on manual skip / DJ handoff
        vGain.gain.cancelScheduledValues(now);
        vGain.gain.setValueAtTime(vGain.gain.value, now);
        vGain.gain.linearRampToValueAtTime(0.0, now + 0.006);
        src.stop(now + 0.007);
        setTimeout(() => {
          try {
            vGain.disconnect();
          } catch {
            // ignore
          }
        }, 30);
      } else {
        src.stop();
      }
    } catch {
      // Already stopped
    }
  }

  public getPositionSec(): number {
    if (this.playing && this.djActive) {
      return Math.max(0, Math.min(this.djDisplayPosSec, this.currentDurSec || this.djDisplayPosSec));
    }
    if (this.playing && this.streamNode && this.activeStreamSource) {
      const sec = this.activeStreamSource.elapsedMs() / 1000;
      return Math.max(0, Math.min(sec, this.currentDurSec || sec));
    }
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

  private async decodeTrackFullS16(ctx: AudioContext, trackIndex: number): Promise<Int16Array> {
    const streamEntry = this.streamingTracks.get(trackIndex);
    if (streamEntry) {
      const durMs = streamEntry.source.open(ctx.sampleRate);
      const durSec = Math.min(240, Math.max(5, durMs > 0 ? durMs / 1000 : streamEntry.initialDurSec));
      const pcm = streamEntry.source.renderS16(Math.floor(durSec * ctx.sampleRate));
      streamEntry.source.close();
      return pcm;
    }
    const buf = await this.loadTrackBuffer(ctx, trackIndex);
    return this.extractBufferS16AtRate(buf, ctx.sampleRate, 0, Math.min(360, buf.duration)).s16;
  }

  public ensureManualDjAudioActive(): void {
    const ctx = this.ensureContext();
    if (ctx.state === 'suspended') {
      void ctx.resume();
    }
    if (this.manualDjActive && this.streamNode) return;
    const djBridge = this.djBridge;
    if (!djBridge) return;

    this.stopSource(false, true);
    this.manualDjActive = true;
    this.playing = true;

    const proc = ctx.createScriptProcessor(2048, 0, 2);
    proc.onaudioprocess = (ev: AudioProcessingEvent) => {
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      const frames = outL.length;
      if (!this.manualDjActive || this.streamNode !== proc || !this.djBridge) {
        outL.fill(0);
        outR.fill(0);
        return;
      }
      const n = this.djBridge.manualRenderF32Planar(outL, outR, ctx.sampleRate);
      for (let i = n; i < frames; i++) {
        outL[i] = 0;
        outR[i] = 0;
      }
    };
    proc.connect(this.eqFilters[0]);
    this.streamNode = proc;
  }

  private manualDjSessionSeq = 0;

  public async startManualDjSession(
    trackA: number,
    posAMs: number,
    playingA: boolean,
    trackB: number,
  ): Promise<void> {
    const ctx = this.ensureContext();
    const djBridge = this.djBridge;
    if (!djBridge) return;

    const sessionSeq = ++this.manualDjSessionSeq;
    const wasPlayingBefore = this.playing;
    this.manualDjActive = true;

    // Decode Deck A BEFORE stopping `sourceNode` so current playback never drops out during decode!
    let s16A: Int16Array | null = null;
    try {
      s16A = await this.decodeTrackFullS16(ctx, trackA);
    } catch (err) {
      console.warn('Manual DJ Deck A load error:', err);
    }
    if (!this.manualDjActive || this.manualDjSessionSeq !== sessionSeq) return;

    const handoffPosMs = wasPlayingBefore ? this.getPositionMs() : posAMs;
    djBridge.manualBegin(ctx.sampleRate);
    if (s16A && s16A.length > 0) {
      djBridge.manualLoadDeckS16(0, trackA, ctx.sampleRate, s16A, handoffPosMs, !playingA);
    }
    this.ensureManualDjAudioActive();

    if (trackB < 0) return;
    // Yield one macrotask so the newly connected audio node fills its first buffer cleanly
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    if (!this.manualDjActive || this.manualDjSessionSeq !== sessionSeq) return;

    try {
      const s16B = await this.decodeTrackFullS16(ctx, trackB);
      if (!this.manualDjActive || this.manualDjSessionSeq !== sessionSeq) return;
      djBridge.manualLoadDeckS16(1, trackB, ctx.sampleRate, s16B, 0, true);
    } catch (err) {
      console.warn('Manual DJ Deck B load error:', err);
    }
  }

  public async loadManualDjDeck(deck: number, trackIndex: number): Promise<void> {
    const ctx = this.ensureContext();
    const djBridge = this.djBridge;
    if (!djBridge || !this.manualDjActive) return;
    const sessionSeq = this.manualDjSessionSeq;
    try {
      const s16 = await this.decodeTrackFullS16(ctx, trackIndex);
      if (!this.manualDjActive || this.manualDjSessionSeq !== sessionSeq) return;
      djBridge.manualLoadDeckS16(deck, trackIndex, ctx.sampleRate, s16, 0, true);
    } catch (err) {
      console.warn('Manual DJ deck reload error:', err);
    }
  }

  public releaseManualDjSession(trackIndex: number, posMs: number, playing: boolean): void {
    this.manualDjSessionSeq++;
    this.manualDjActive = false;
    this.stopSource(false, false);
    this.currentTrack = trackIndex;
    this.offsetSec = Math.max(0, posMs / 1000);
    if (playing) {
      this.playTrack(trackIndex, this.offsetSec);
    } else {
      this.playing = false;
    }
  }
}
