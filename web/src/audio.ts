/**
 * UIPod Web Audio Engine:
 * - Plays 5 built-in procedurally synthesized stereo tracks (zero external copyright assets!)
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

export interface TrackEntry {
  index: number;
  title: string;
  artist: string;
  album: string;
  codec: string;
  durationMs: number;
  isUserFile: boolean;
  buffer?: AudioBuffer;
}

const EQ_FREQS = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

// Realistic 10-band AutoEQ compensation curves (dB) matching UIPod's headphone database spirit
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
  private demoCache = new Map<number, AudioBuffer>();
  private userBuffers = new Map<number, AudioBuffer>();

  public playing = false;
  public currentTrack = 0;
  public trackCount = 5;
  private offsetSec = 42.0;
  private startCtxTime = 0;
  private currentDurSec = 192.0;
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

    // Signal Chain:
    // Source -> 10-band EQ -> Tape Delay (Wow/Flutter) -> Tape Shaper (Sat) -> Tape Lowpass (HF Roll-Off)
    //        -> (+ Tape Hiss) -> PreTapGain -> (Analyser L/R) -> MasterGain -> Destination
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

    // Dual-rate analog Wow (0.65 Hz capstan) + Flutter (6.2 Hz pinch roller) LFOs
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

    // Tape Hiss noise loop
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

    // Stereo Analyser Tap (pre-volume, matching UIPod's ALSA pre-volume PCM tap!)
    const splitter = ctx.createChannelSplitter(2);
    const analyserL = ctx.createAnalyser();
    const analyserR = ctx.createAnalyser();
    analyserL.fftSize = 2048;
    analyserR.fftSize = 2048;
    this.splitter = splitter;
    this.analyserL = analyserL;
    this.analyserR = analyserR;

    const master = ctx.createGain();
    master.gain.value = this.volumeNorm * this.volumeNorm;
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
      // Perceptual loudness curve matching UIPod's 40-step volume curve
      const gain = this.volumeNorm === 0 ? 0 : Math.pow(this.volumeNorm, 2.1);
      this.masterGain.gain.setTargetAtTime(gain, this.ctx.currentTime, 0.02);
    }
  }

  /** Authentic tactile iPod Click Wheel piezo click impulse. */
  public playMenuClick(): void {
    if (!this.ctx) return; // Don't force AudioContext before first user gesture
    if (this.ctx.state !== 'running') return;
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

  private getOrSynthTrackBuffer(ctx: AudioContext, trackIndex: number): AudioBuffer {
    const userBuf = this.userBuffers.get(trackIndex);
    if (userBuf) return userBuf;
    const cached = this.demoCache.get(trackIndex);
    if (cached) return cached;

    // Synthesize a lush 16-second stereo musical loop tailored to each built-in track!
    const sr = ctx.sampleRate;
    const loopSec = 16;
    const len = sr * loopSec;
    const buf = ctx.createBuffer(2, len, sr);
    const L = buf.getChannelData(0);
    const R = buf.getChannelData(1);

    const style = trackIndex % 5;
    const bpm = [86, 118, 76, 128, 92][style];
    const beatSec = 60 / bpm;

    // Musical chord progressions (Hz)
    const chords: number[][][] = [
      // 0: Midnight Tape Loop (Warm Lo-Fi Rhodes Dm9 -> G13 -> Cmaj9 -> A7b9)
      [[146.83, 220.0, 261.63, 329.63], [196.0, 246.94, 329.63, 392.0], [130.81, 196.0, 246.94, 293.66], [110.0, 164.81, 220.0, 277.18]],
      // 1: Phosphor Horizon (Synthwave Am -> F -> C -> G)
      [[110.0, 220.0, 261.63, 329.63], [87.31, 174.61, 220.0, 261.63], [130.81, 196.0, 261.63, 329.63], [98.0, 196.0, 246.94, 293.66]],
      // 2: 4-Shade Rain (Ambient Dub Cm9 -> Abmaj7 -> Fm9 -> Gsus4)
      [[130.81, 155.56, 196.0, 293.66], [103.83, 155.56, 207.65, 246.94], [87.31, 130.81, 174.61, 207.65], [98.0, 130.81, 196.0, 261.63]],
      // 3: Pixel Velocity (Chiptune Electro Em -> C -> G -> D)
      [[164.81, 246.94, 329.63, 392.0], [130.81, 196.0, 261.63, 329.63], [196.0, 246.94, 293.66, 392.0], [146.83, 220.0, 293.66, 369.99]],
      // 4: Velvet Cassette (Chillhop Fmaj7 -> Em7 -> Dm7 -> Cmaj7)
      [[174.61, 220.0, 261.63, 329.63], [164.81, 196.0, 246.94, 293.66], [146.83, 174.61, 220.0, 261.63], [130.81, 164.81, 196.0, 246.94]],
    ];
    const prog = chords[style];
    const barSec = beatSec * 4;

    let rng = (trackIndex + 1) * 133791;
    const rand = () => {
      rng = (rng * 1664525 + 1013904223) >>> 0;
      return (rng / 0xffffffff) * 2 - 1;
    };

    for (let i = 0; i < len; i++) {
      const t = i / sr;
      const barIdx = Math.floor(t / barSec) % 4;
      const beatInBar = (t % barSec) / beatSec;
      const beatFrac = beatInBar - Math.floor(beatInBar);
      const stepIdx = Math.floor(beatInBar * 4);
      const stepFrac = beatInBar * 4 - stepIdx;
      const chord = prog[barIdx];

      // 1. Punchy Analog Kick on beats 0 & 2 (plus syncopation on style 1 & 3)
      let kick = 0;
      const kickHit =
        Math.floor(beatInBar) === 0 ||
        Math.floor(beatInBar) === 2 ||
        (style === 1 && stepIdx === 10) ||
        (style === 3 && stepIdx === 14);
      if (kickHit) {
        const kt = stepIdx % 4 === 0 ? beatFrac * beatSec : stepFrac * (beatSec * 0.25);
        if (kt < 0.28) {
          const pitch = 46 + 115 * Math.exp(-kt * 28);
          const env = Math.exp(-kt * 11.5);
          kick = Math.sin(2 * Math.PI * pitch * kt) * env * 0.52;
        }
      }

      // 2. Crisp Snare / Clap on beats 1 & 3
      let snare = 0;
      if (Math.floor(beatInBar) === 1 || Math.floor(beatInBar) === 3) {
        const st = beatFrac * beatSec;
        if (st < 0.18) {
          const nEnv = Math.exp(-st * 24);
          const body = Math.sin(2 * Math.PI * 195 * st) * Math.exp(-st * 32) * 0.24;
          snare = (rand() * 0.28 * nEnv + body) * (style === 2 ? 0.55 : 0.9);
        }
      }

      // 3. Stereo Hi-Hat groove
      let hatL = 0;
      let hatR = 0;
      const ht = stepFrac * (beatSec * 0.25);
      if (ht < 0.045 && (stepIdx % 2 === 0 || style === 3)) {
        const hEnv = Math.exp(-ht * 85) * (stepIdx % 4 === 2 ? 0.12 : 0.065);
        const n = rand();
        hatL = n * hEnv * (stepIdx % 2 === 0 ? 1.15 : 0.7);
        hatR = -n * hEnv * (stepIdx % 2 === 0 ? 0.7 : 1.15);
      }

      // 4. Warm Sub-Bass line following root notes
      const rootFreq = chord[0] * 0.5;
      const bassPulse = 0.65 + 0.35 * Math.exp(-beatFrac * 4.0);
      const bass =
        (Math.sin(2 * Math.PI * rootFreq * t) * 0.75 +
          Math.sin(2 * Math.PI * rootFreq * 2 * t) * 0.25) *
        0.24 *
        bassPulse;

      // 5. Lush Stereo Chords / Pads with slow chorus detune
      let padL = 0;
      let padR = 0;
      const sidechain = 0.65 + 0.35 * Math.min(1, beatFrac * 2.5);
      for (let n = 0; n < chord.length; n++) {
        const f = chord[n];
        const det = 1 + (n % 2 === 0 ? 0.0028 : -0.0028);
        const w1 = Math.sin(2 * Math.PI * f * t);
        const w2 = Math.sin(2 * Math.PI * f * det * t + 0.7);
        const harm = style === 1 || style === 3 ? Math.sin(2 * Math.PI * f * 2 * t) * 0.25 : 0;
        padL += (w1 + harm) * 0.085;
        padR += (w2 + harm) * 0.085;
      }
      padL *= sidechain;
      padR *= sidechain;

      // 6. Stereo Arpeggio / Lead Melody
      const arpNote = chord[(stepIdx + barIdx) % chord.length] * (style === 3 ? 2.0 : 1.0);
      const arpEnv = Math.exp(-stepFrac * (style === 2 ? 2.2 : 5.0));
      let arp = 0;
      if (style === 3) {
        // Crisp chiptune square-ish lead
        const s = Math.sin(2 * Math.PI * arpNote * t);
        arp = Math.tanh(s * 3.2) * 0.11 * arpEnv;
      } else {
        arp =
          (Math.sin(2 * Math.PI * arpNote * t) +
            0.3 * Math.sin(2 * Math.PI * arpNote * 2 * t)) *
          0.11 *
          arpEnv;
      }
      const pan = Math.sin(t * 0.9) * 0.45;
      const arpL = arp * (0.5 - pan * 0.5);
      const arpR = arp * (0.5 + pan * 0.5);

      // Smooth fade at loop seam (12ms)
      let seam = 1;
      const fadeSamples = Math.floor(sr * 0.012);
      if (i < fadeSamples) seam = i / fadeSamples;
      else if (i > len - fadeSamples) seam = (len - i) / fadeSamples;

      L[i] = Math.tanh((kick + snare + hatL + bass + padL + arpL) * 0.92) * seam;
      R[i] = Math.tanh((kick + snare + hatR + bass + padR + arpR) * 0.92) * seam;
    }

    this.demoCache.set(trackIndex, buf);
    return buf;
  }

  public playTrack(trackIndex: number, startSec = 0): void {
    const ctx = this.ensureContext();
    this.stopSource(false);

    this.currentTrack = trackIndex;
    const buf = this.getOrSynthTrackBuffer(ctx, trackIndex);
    const isUser = this.userBuffers.has(trackIndex);
    this.currentDurSec = isUser ? buf.duration : [192, 214, 238, 176, 205][trackIndex % 5];
    this.offsetSec = Math.max(0, Math.min(startSec, this.currentDurSec - 0.5));

    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.loop = !isUser;
    src.connect(this.eqFilters[0]);

    const targetRate = Math.max(0.5, Math.min(1.5, this.dsp.speed / 100));
    const now = ctx.currentTime;
    if (this.dsp.tape && this.dsp.stopStart) {
      // Authentic cassette motor spin-up!
      src.playbackRate.setValueAtTime(0.32, now);
      src.playbackRate.exponentialRampToValueAtTime(targetRate, now + 0.24);
    } else {
      src.playbackRate.setValueAtTime(targetRate, now);
    }

    const bufOffset = isUser ? this.offsetSec : this.offsetSec % buf.duration;
    src.start(now, bufOffset);
    src.onended = () => {
      if (this.sourceNode === src && isUser && this.playing) {
        // Advance to next track when user file completes
        const next = (this.currentTrack + 1) % Math.max(1, this.trackCount);
        this.playTrack(next, 0);
      }
    };

    this.sourceNode = src;
    this.startCtxTime = now;
    this.playing = true;
    this.applyDsp(this.dsp);
  }

  public setPlaying(shouldPlay: boolean, trackIndex?: number): void {
    if (typeof trackIndex === 'number' && trackIndex !== this.currentTrack) {
      this.currentTrack = trackIndex;
      this.offsetSec = 0;
    }
    if (shouldPlay) {
      if (!this.playing) {
        this.playTrack(this.currentTrack, this.offsetSec);
      }
    } else if (this.playing) {
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
        // Authentic Walkman cassette motor spin-down!
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
    if (!this.playing || !this.ctx) return this.offsetSec;
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
    return this.ctx?.sampleRate ?? 48000;
  }

  /**
   * Reads 1024 interleaved stereo S16 PCM frames (`2048` i16 values) from the live Web Audio tap
   * so UIPod's real Rust `VizDsp` (FFT, beat detector, VU needles, oscilloscope) reacts in real time.
   */
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
