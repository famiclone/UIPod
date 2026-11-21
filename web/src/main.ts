import initWasm, {
  UIPodWasm,
  get_autoeq_presets_json,
  get_themes_json,
  get_viz_preset_shader,
  get_viz_presets_json,
} from './wasm-pkg/uipod_wasm.js';
import { UIPodAudioEngine } from './audio';
import { STRINGS, type Lang } from './i18n';

const BTN: Record<string, number> = {
  A: 0x0001,
  B: 0x0002,
  X: 0x0004,
  Y: 0x0008,
  UP: 0x0010,
  DOWN: 0x0020,
  LEFT: 0x0040,
  RIGHT: 0x0080,
  START: 0x0100,
  SELECT: 0x0200,
  L1: 0x0400,
  R1: 0x0800,
  L2: 0x1000,
  R2: 0x2000,
  MENU: 0x4000,
};

interface ThemeInfo {
  index: number;
  name: string;
  bg: string;
  fg: string;
  fg1: string;
  fg2: string;
}

interface VizInfo {
  index: number;
  id: string;
  nameEn: string;
  nameUk: string;
}

interface AppStateSnapshot {
  screen: string;
  playing: boolean;
  trackIndex: number;
  trackCount: number;
  title: string;
  artist: string;
  themeIndex: number;
  themeName: string;
  bg: string;
  fg: string;
  fg1: string;
  fg2: string;
  monoArt: boolean;
  altLayout: boolean;
  transition: number;
  uk: boolean;
  hold: boolean;
  volumeStep: number;
  volumeDb: number;
  vizPreset: number;
  vizName: string;
  vizFine: boolean;
  vizCustom: boolean;
  eq: boolean;
  autoeq: number;
  tape: boolean;
  wow: number;
  hiss: number;
  sat: number;
  hf: number;
  stopStart: boolean;
  speed: number;
  menuClick: boolean;
  djMode?: number;
  djLength?: number;
  djMaxStretch?: number;
  djKeepGapless?: boolean;
  djQuickCut?: boolean;
}

function parseHex(hex: string): number {
  return parseInt(hex.replace('#', ''), 16) & 0xffffff;
}

async function boot(): Promise<void> {
  const canvas = document.getElementById('uipod-canvas') as HTMLCanvasElement;
  const loader = document.getElementById('screen-loader') as HTMLElement;
  const fpsBadge = document.getElementById('fps-badge') as HTMLElement;

  try {
    await initWasm();
  } catch (err) {
    loader.innerHTML = `<span style="color:#fca5a5;padding:16px;text-align:center">Failed to load UIPod WASM:<br/>${String(err)}</span>`;
    return;
  }

  let uipod: UIPodWasm;
  try {
    uipod = new UIPodWasm(canvas);
  } catch (err) {
    loader.innerHTML = `<span style="color:#fca5a5;padding:16px;text-align:center">WebGL2 Initialization Error:<br/>${String(err)}</span>`;
    return;
  }

  const audio = new UIPodAudioEngine();
  audio.djBridge = {
    setConfig: (mode, length, maxStretch, keepGapless, quickCut) => {
      uipod.set_dj_config(mode, length, maxStretch, keepGapless, quickCut);
    },
    beginTransition: (
      sampleRate,
      trackA,
      tailAS16,
      trackB,
      introBS16,
      isBEof,
      deckBIsNativeNsf,
      manualQuickCut,
    ) =>
      uipod.dj_begin_transition(
        sampleRate,
        trackA,
        tailAS16,
        trackB,
        introBS16,
        isBEof,
        deckBIsNativeNsf,
        manualQuickCut,
      ),
    feedDeckBS16: (chunkS16, isEof) => uipod.dj_feed_deck_b_s16(chunkS16, isEof),
    renderS16: (frames) => uipod.dj_render_s16(frames),
    takeMidpointCrossed: () => uipod.dj_take_midpoint_crossed(),
    isActive: () => uipod.dj_is_active(),
    reset: () => uipod.dj_reset(),
  };

  try {
    const savedDj = localStorage.getItem('uipod_dj_cfg_v1');
    if (savedDj) {
      const parsed = JSON.parse(savedDj) as {
        mode?: number;
        length?: number;
        maxStretch?: number;
        keepGapless?: boolean;
        quickCut?: boolean;
      };
      const djMode = Number(parsed.mode ?? 0);
      const djLength = Number(parsed.length ?? 1);
      const djMaxStretch = Number(parsed.maxStretch ?? 1);
      const djKeepGapless = parsed.keepGapless !== false;
      const djQuickCut = parsed.quickCut !== false;
      uipod.set_dj_config(djMode, djLength, djMaxStretch, djKeepGapless, djQuickCut);
      audio.applyDsp({
        ...audio.dsp,
        djMode,
        djLength,
        djMaxStretch,
        djKeepGapless,
        djQuickCut,
      });
    }
  } catch {
    // Ignore storage access errors
  }
  const themes: ThemeInfo[] = JSON.parse(get_themes_json());
  const vizPresets: VizInfo[] = JSON.parse(get_viz_presets_json());
  const autoeqPresets: string[] = JSON.parse(get_autoeq_presets_json());

  let uiLang: Lang = 'en';
  let lastStateJson = '';
  let lastState: AppStateSnapshot | null = null;
  let editedPresetIdx = -1;

  // ---- DOM References ----
  const deviceStage = document.getElementById('device-stage') as HTMLElement;
  const btnFullscreen = document.getElementById('btn-fullscreen') as HTMLButtonElement;
  const btnFsExit = document.getElementById('btn-fs-exit') as HTMLButtonElement;
  const btnFsHold = document.getElementById('btn-fs-hold') as HTMLButtonElement;
  const holdBtn = document.getElementById('btn-hold-switch') as HTMLButtonElement;

  const hudTitle = document.getElementById('hud-track-title') as HTMLElement;
  const hudArtist = document.getElementById('hud-track-artist') as HTMLElement;
  const hudTheme = document.getElementById('hud-theme-name') as HTMLElement;
  const studioPlayBtn = document.getElementById('studio-play-btn') as HTMLButtonElement;
  const iconPlay = document.getElementById('icon-play') as unknown as SVGElement;
  const iconPause = document.getElementById('icon-pause') as unknown as SVGElement;

  const dotBg = document.querySelector('#hud-palette-dots [data-shade="bg"]') as HTMLElement;
  const dotFg1 = document.querySelector('#hud-palette-dots [data-shade="fg1"]') as HTMLElement;
  const dotFg2 = document.querySelector('#hud-palette-dots [data-shade="fg2"]') as HTMLElement;
  const dotFg = document.querySelector('#hud-palette-dots [data-shade="fg"]') as HTMLElement;

  // Theme Studio elements
  const themesGrid = document.getElementById('theme-presets-grid') as HTMLElement;
  const pickBg = document.getElementById('pick-bg') as HTMLInputElement;
  const pickFg1 = document.getElementById('pick-fg1') as HTMLInputElement;
  const pickFg2 = document.getElementById('pick-fg2') as HTMLInputElement;
  const pickFg = document.getElementById('pick-fg') as HTMLInputElement;
  const hexBg = document.getElementById('hex-bg') as HTMLElement;
  const hexFg1 = document.getElementById('hex-fg1') as HTMLElement;
  const hexFg2 = document.getElementById('hex-fg2') as HTMLElement;
  const hexFg = document.getElementById('hex-fg') as HTMLElement;
  const chkAutoDerive = document.getElementById('chk-auto-derive') as HTMLInputElement;
  const chkMonoArt = document.getElementById('chk-mono-art') as HTMLInputElement;
  const selTransition = document.getElementById('sel-transition') as HTMLSelectElement;
  const btnCopyCfg = document.getElementById('btn-copy-cfg') as HTMLButtonElement;

  // Visualizer Lab elements
  const vizGrid = document.getElementById('viz-presets-grid') as HTMLElement;
  const btnLaunchViz = document.getElementById('btn-launch-viz') as HTMLButtonElement;
  const chkVizFine = document.getElementById('chk-viz-fine') as HTMLInputElement;
  const lblVizGrain = document.getElementById('lbl-viz-grain') as HTMLElement;
  const glslEditor = document.getElementById('glsl-editor') as HTMLTextAreaElement;
  const btnCompileShader = document.getElementById('btn-compile-shader') as HTMLButtonElement;
  const btnResetShader = document.getElementById('btn-reset-shader') as HTMLButtonElement;
  const shaderStatus = document.getElementById('shader-status') as HTMLElement;

  // Sound & Tape elements
  const chkEq = document.getElementById('chk-eq') as HTMLInputElement;
  const chkClick = document.getElementById('chk-click') as HTMLInputElement;
  const selAutoeq = document.getElementById('sel-autoeq') as HTMLSelectElement;
  const chkTape = document.getElementById('chk-tape') as HTMLInputElement;
  const chkMotor = document.getElementById('chk-motor') as HTMLInputElement;
  const rngWow = document.getElementById('rng-wow') as HTMLInputElement;
  const rngHiss = document.getElementById('rng-hiss') as HTMLInputElement;
  const rngSat = document.getElementById('rng-sat') as HTMLInputElement;
  const rngHf = document.getElementById('rng-hf') as HTMLInputElement;
  const rngSpeed = document.getElementById('rng-speed') as HTMLInputElement;
  const valWow = document.getElementById('val-wow') as HTMLElement;
  const valHiss = document.getElementById('val-hiss') as HTMLElement;
  const valSat = document.getElementById('val-sat') as HTMLElement;
  const valHf = document.getElementById('val-hf') as HTMLElement;
  const valSpeed = document.getElementById('val-speed') as HTMLElement;

  // ---- Build Theme Chips ----
  const themeButtons: HTMLButtonElement[] = [];
  themes.forEach((t) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'theme-chip';
    btn.innerHTML = `
      <span class="theme-swatch-4">
        <span style="background:${t.bg}"></span>
        <span style="background:${t.fg1}"></span>
        <span style="background:${t.fg2}"></span>
        <span style="background:${t.fg}"></span>
      </span>
      <span class="theme-chip-name">${t.name}</span>
    `;
    btn.addEventListener('click', () => {
      uipod.set_theme_preset(t.index);
    });
    themesGrid.appendChild(btn);
    themeButtons.push(btn);
  });

  // ---- Build Visualizer Preset Buttons ----
  const vizButtons: HTMLButtonElement[] = [];
  vizPresets.forEach((v) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'viz-btn';
    btn.innerHTML = `<span class="viz-btn-name">${v.nameEn}</span><code>#${String(v.index).padStart(2, '0')}</code>`;
    btn.addEventListener('click', () => {
      uipod.set_viz_preset(v.index);
      glslEditor.value = get_viz_preset_shader(v.index).trim();
      editedPresetIdx = v.index;
      if (!audio.playing) {
        audio.setPlaying(true, lastState?.trackIndex ?? 0);
      }
      uipod.jump_to_screen('visualizer');
    });
    vizGrid.appendChild(btn);
    vizButtons.push(btn);
  });

  // ---- Build AutoEQ Options ----
  autoeqPresets.forEach((name, idx) => {
    const opt = document.createElement('option');
    opt.value = String(idx);
    opt.textContent = name;
    selAutoeq.appendChild(opt);
  });

  // ---- Bilingual UI Switcher ----
  function applyPageLang(lang: Lang, syncWasm: boolean): void {
    uiLang = lang;
    const dict = STRINGS[lang];
    document.querySelectorAll<HTMLElement>('[data-i18n]').forEach((el) => {
      const key = el.dataset.i18n;
      if (key && dict[key]) {
        el.textContent = dict[key];
      }
    });
    document.querySelectorAll<HTMLButtonElement>('.lang-btn').forEach((b) => {
      b.classList.toggle('active', b.dataset.lang === lang);
    });
    vizPresets.forEach((v, i) => {
      const span = vizButtons[i]?.querySelector('.viz-btn-name');
      if (span) span.textContent = lang === 'uk' ? v.nameUk : v.nameEn;
    });
    if (syncWasm) {
      uipod.set_language(lang === 'uk');
    }
  }

  document.querySelectorAll<HTMLButtonElement>('.lang-btn').forEach((b) => {
    b.addEventListener('click', () => {
      applyPageLang((b.dataset.lang as Lang) || 'en', true);
    });
  });

  // ---- Studio Tabs ----
  document.querySelectorAll<HTMLButtonElement>('.studio-tab').forEach((tab) => {
    tab.addEventListener('click', () => {
      const target = tab.dataset.tab;
      document.querySelectorAll('.studio-tab').forEach((t) => t.classList.toggle('active', t === tab));
      document.querySelectorAll('.tab-pane').forEach((p) => {
        p.classList.toggle('active', p.id === `pane-${target}`);
      });
    });
  });

  // ---- Quick Jump & Plugin Cards ----
  document.querySelectorAll<HTMLButtonElement>('[data-jump]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const dest = btn.dataset.jump;
      if (!dest) return;
      if (dest === 'visualizer' && !audio.playing) {
        audio.setPlaying(true, lastState?.trackIndex ?? 0);
      }
      uipod.jump_to_screen(dest);
    });
  });

  studioPlayBtn.addEventListener('click', () => {
    const nextPlay = !audio.playing;
    audio.setPlaying(nextPlay, lastState?.trackIndex ?? 0);
    uipod.sync_audio_state(audio.playing, audio.getPositionMs(), audio.getDurationMs());
  });

  // ---- Theme Studio Pickers ----
  function pushCustomTheme(): void {
    const bg = parseHex(pickBg.value);
    const fg = parseHex(pickFg.value);
    const fg1 = parseHex(pickFg1.value);
    const fg2 = parseHex(pickFg2.value);
    uipod.set_custom_theme(bg, fg, fg1, fg2, chkAutoDerive.checked);
  }

  pickBg.addEventListener('input', pushCustomTheme);
  pickFg.addEventListener('input', pushCustomTheme);
  pickFg1.addEventListener('input', () => {
    chkAutoDerive.checked = false;
    pushCustomTheme();
  });
  pickFg2.addEventListener('input', () => {
    chkAutoDerive.checked = false;
    pushCustomTheme();
  });
  chkAutoDerive.addEventListener('change', () => {
    if (chkAutoDerive.checked) pushCustomTheme();
  });
  chkMonoArt.addEventListener('change', () => uipod.set_mono_art(chkMonoArt.checked));
  selTransition.addEventListener('change', () => uipod.set_transition(Number(selTransition.value)));

  btnCopyCfg.addEventListener('click', async () => {
    const s = lastState;
    if (!s) return;
    const snippet = [
      '# UIPod 4-Shade Theme (paste into uipod/config.cfg)',
      `background color: ${s.bg.slice(1)}`,
      `foreground color: ${s.fg.slice(1)}`,
      `fg1 color: ${s.fg1.slice(1)}`,
      `fg2 color: ${s.fg2.slice(1)}`,
    ].join('\n');
    try {
      await navigator.clipboard.writeText(snippet);
      btnCopyCfg.textContent = STRINGS[uiLang].theme_copied;
      setTimeout(() => {
        btnCopyCfg.textContent = STRINGS[uiLang].theme_copy;
      }, 1500);
    } catch {
      window.prompt('Copy UIPod config.cfg theme snippet:', snippet);
    }
  });

  // ---- Visualizer Lab Controls ----
  btnLaunchViz.addEventListener('click', () => {
    if (!audio.playing) {
      audio.setPlaying(true, lastState?.trackIndex ?? 0);
    }
    uipod.jump_to_screen('visualizer');
  });

  chkVizFine.addEventListener('change', () => {
    uipod.set_viz_fine_grain(chkVizFine.checked);
    lblVizGrain.textContent = chkVizFine.checked ? '2×2 Fine' : '4×4 LCD';
  });

  btnCompileShader.addEventListener('click', () => {
    try {
      uipod.compile_custom_viz_shader(glslEditor.value);
      shaderStatus.className = 'shader-status ok';
      shaderStatus.textContent = STRINGS[uiLang].viz_ok;
      if (!audio.playing) audio.setPlaying(true, lastState?.trackIndex ?? 0);
      uipod.jump_to_screen('visualizer');
    } catch (err) {
      shaderStatus.className = 'shader-status err';
      shaderStatus.textContent = String(err);
    }
  });

  btnResetShader.addEventListener('click', () => {
    const idx = lastState?.vizPreset ?? 9;
    glslEditor.value = get_viz_preset_shader(idx).trim();
    uipod.set_viz_preset(idx);
    shaderStatus.className = 'shader-status';
    shaderStatus.textContent = `Loaded preset #${idx}: ${vizPresets[idx]?.nameEn ?? ''}`;
  });

  // ---- Sound & Tape Controls ----
  function pushDspFromControls(): void {
    const eq = chkEq.checked;
    const autoeq = Number(selAutoeq.value);
    const tape = chkTape.checked;
    const wow = Number(rngWow.value);
    const hiss = Number(rngHiss.value);
    const sat = Number(rngSat.value);
    const hf = Number(rngHf.value);
    const stopStart = chkMotor.checked;
    const speed = Number(rngSpeed.value);
    const click = chkClick.checked;

    valWow.textContent = `${wow}%`;
    valHiss.textContent = `${hiss}%`;
    valSat.textContent = `${sat}%`;
    valHf.textContent = `${hf}%`;
    valSpeed.textContent = `${(speed / 100).toFixed(2)}×`;

    uipod.set_dsp_config(eq, autoeq, tape, wow, hiss, sat, hf, stopStart, speed, click);
    audio.applyDsp({ eq, autoeq, tape, wow, hiss, sat, hf, stopStart, speed });
  }

  [chkEq, chkClick, selAutoeq, chkTape, chkMotor, rngWow, rngHiss, rngSat, rngHf, rngSpeed].forEach(
    (el) => el.addEventListener('input', pushDspFromControls),
  );

  // ---- Drag & Drop Audio File Importer ----
  const dropzone = document.getElementById('audio-dropzone') as HTMLElement;
  const fileInput = document.getElementById('audio-file-input') as HTMLInputElement;
  const btnPickAudio = document.getElementById('btn-pick-audio') as HTMLButtonElement;
  const uriToTrackIndex = new Map<string, number>();

  audio.onNonStreamingTrack = () => {
    uipod.reset_nsf_session();
  };
  audio.onTrackAutoAdvance = (nextIdx: number) => {
    uipod.select_and_play_track(nextIdx);
  };

  async function importAudioFiles(files: FileList | File[]): Promise<void> {
    const list = Array.from(files);

    // 1. Register any `.gsflib` sample/ROM libraries or `.sf2` SoundFonts first.
    const gsfLibFiles = list.filter((f) => /\.gsflib$/i.test(f.name));
    for (const libFile of gsfLibFiles) {
      const libBytes = new Uint8Array(await libFile.arrayBuffer());
      uipod.register_gsflib(libFile.name, libBytes);
    }
    const sf2Files = list.filter((f) => /\.sf2$/i.test(f.name));
    for (const sf2File of sf2Files) {
      const sf2Bytes = new Uint8Array(await sf2File.arrayBuffer());
      uipod.register_soundfont(sf2File.name, sf2Bytes);
    }

    const m3uFiles = list.filter((f) => /\.(m3u|m3u8)$/i.test(f.name));
    const audioFiles = list
      .filter(
        (f) =>
          !/\.(m3u|m3u8|gsflib|sf2)$/i.test(f.name) &&
          (f.type.startsWith('audio/') ||
            /\.(mp3|flac|wav|ogg|opus|m4a|aac|nsf|nsfe|gsf|minigsf|mid|midi|kar|rmi)$/i.test(
              f.name,
            )),
      )
      .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));

    const coverFile = list.find((f) => f.type.startsWith('image/'));
    const coverBytes = coverFile ? new Uint8Array(await coverFile.arrayBuffer()) : undefined;

    let firstAddedIdx: number | null = null;

    for (const file of audioFiles) {
      if (/\.(nsf|nsfe)$/i.test(file.name)) {
        try {
          const nsfBytes = new Uint8Array(await file.arrayBuffer());
          const meta = JSON.parse(uipod.register_nsf_container(file.name, nsfBytes)) as {
            ok?: boolean;
            gameTitle?: string;
            artist?: string;
            chipFlags?: number;
            startingSubtrack?: number;
            subtracks?: Array<{
              index: number;
              title: string;
              kind: 'music' | 'sfx';
              durationMs: number;
              loopPointMs: number | null;
            }>;
          };
          if (!meta.ok || !Array.isArray(meta.subtracks) || meta.subtracks.length === 0) {
            continue;
          }

          for (const sub of meta.subtracks) {
            const uri = `${file.name}#${sub.index}`;
            const nextIdx = audio.trackCount;
            const durMs = Math.max(400, Number(sub.durationMs ?? 124000));
            audio.registerStreamingPcmTrack(nextIdx, durMs, {
              open: (sr) => uipod.open_nsf_stream(uri, Math.round(sr)),
              renderS16: (frames) => uipod.render_nsf_stream_s16(frames),
              seekMs: (ms) => uipod.seek_nsf_stream_ms(ms),
              elapsedMs: () => uipod.nsf_stream_elapsed_ms(),
              close: () => uipod.close_nsf_stream(),
            });
            const idx = uipod.add_nsf_subtrack(
              uri,
              sub.index,
              sub.kind === 'sfx',
              meta.chipFlags ?? 0,
              sub.loopPointMs ?? -1,
              sub.title,
              meta.artist || 'Famicom / NES',
              meta.gameTitle || file.name.replace(/\.[^.]+$/, ''),
              durMs,
              coverBytes,
            );
            uriToTrackIndex.set(uri.toLowerCase(), idx);
            if (firstAddedIdx === null && sub.kind === 'music') {
              firstAddedIdx = idx;
            }
          }
          if (firstAddedIdx === null && meta.subtracks.length > 0) {
            const startSub =
              meta.subtracks[Math.min(meta.startingSubtrack ?? 0, meta.subtracks.length - 1)];
            const startIdx = uriToTrackIndex.get(`${file.name}#${startSub.index}`.toLowerCase());
            if (startIdx !== undefined) firstAddedIdx = startIdx;
          }
        } catch (err) {
          console.warn('Could not import NSF/NSFe container:', file.name, err);
        }
        continue;
      }
      if (/\.(gsf|minigsf)$/i.test(file.name)) {
        try {
          const gsfBytes = new Uint8Array(await file.arrayBuffer());
          const meta = JSON.parse(uipod.inspect_gsf_json(file.name, gsfBytes)) as {
            ok?: boolean;
            title?: string;
            artist?: string;
            album?: string;
            durationMs?: number;
          };
          if (!meta.ok) continue;
          const nextIdx = audio.trackCount;
          const durMs = Number(meta.durationMs ?? 155000);
          audio.registerLazyPcmTrack(nextIdx, durMs, 44100, () =>
            uipod.decode_gsf_s16(file.name, gsfBytes),
          );
          const idx = uipod.add_user_track(
            meta.title || file.name.replace(/\.[^.]+$/, ''),
            meta.artist || 'Game Boy Advance',
            meta.album || 'GSF Rip',
            'GSF',
            '16bit',
            '44.1kHz',
            durMs,
            coverBytes,
          );
          if (firstAddedIdx === null) firstAddedIdx = idx;
        } catch (err) {
          console.warn('Could not import GSF track:', file.name, err);
        }
        continue;
      }

      if (/\.(mid|midi|kar|rmi)$/i.test(file.name)) {
        try {
          const midiBytes = new Uint8Array(await file.arrayBuffer());
          const meta = JSON.parse(uipod.inspect_midi_json(file.name, midiBytes)) as {
            ok?: boolean;
            title?: string;
            artist?: string;
            album?: string;
            durationMs?: number;
          };
          if (!meta.ok) continue;
          const nextIdx = audio.trackCount;
          const durMs = Number(meta.durationMs ?? 120000);
          audio.registerLazyPcmTrack(nextIdx, durMs, 44100, () =>
            uipod.decode_midi_s16(midiBytes),
          );
          const idx = uipod.add_user_track(
            meta.title || file.name.replace(/\.[^.]+$/, ''),
            meta.artist || 'MIDI Synthesizer',
            meta.album || 'SoundFont MIDI',
            'MIDI',
            '16bit',
            '44.1kHz',
            durMs,
            coverBytes,
          );
          if (firstAddedIdx === null) firstAddedIdx = idx;
        } catch (err) {
          console.warn('Could not import MIDI track:', file.name, err);
        }
        continue;
      }

      const cleanName = file.name.replace(/\.[^.]+$/, '');
      const parts = cleanName.split(/\s+-\s+/);
      const artist = parts.length >= 2 ? parts[0].trim() : 'Local Upload';
      const title = parts.length >= 2 ? parts.slice(1).join(' - ').trim() : cleanName;
      const ext = (file.name.split('.').pop() || 'AUDIO').toUpperCase();
      const nextIdx = audio.trackCount;
      try {
        const durMs = await audio.decodeUserAudio(file, nextIdx);
        const srKHz = `${(audio.getSampleRate() / 1000).toFixed(1).replace('.0', '')}kHz`;
        const idx = uipod.add_user_track(
          title,
          artist,
          'Browser Drop',
          ext,
          ext === 'FLAC' || ext === 'WAV' ? '24bit' : '',
          srKHz,
          durMs,
          coverBytes,
        );
        if (firstAddedIdx === null) firstAddedIdx = idx;
      } catch (err) {
        console.warn('Could not decode dropped audio file:', file.name, err);
      }
    }

    // Resolve `.m3u` / `.m3u8` playlist entries (including `Castlevania.nsf#1` subtrack URIs)
    for (const plFile of m3uFiles) {
      try {
        const text = await plFile.text();
        for (const rawLine of text.split(/\r?\n/)) {
          const line = rawLine.trim();
          if (!line || line.startsWith('#')) continue;
          const baseRef = line.split(/[\\/]/).pop()?.toLowerCase() ?? line.toLowerCase();
          const matched = uriToTrackIndex.get(baseRef);
          if (typeof matched === 'number' && firstAddedIdx === null) {
            firstAddedIdx = matched;
          }
        }
      } catch (err) {
        console.warn('Could not parse playlist file:', plFile.name, err);
      }
    }

    if (firstAddedIdx !== null) {
      audio.playTrack(firstAddedIdx, 0);
      uipod.select_and_play_track(firstAddedIdx);
      uipod.jump_to_screen('now_playing');
    }
  }

  btnPickAudio.addEventListener('click', () => fileInput.click());
  fileInput.addEventListener('change', () => {
    if (fileInput.files && fileInput.files.length > 0) {
      void importAudioFiles(fileInput.files);
      fileInput.value = '';
    }
  });

  ['dragenter', 'dragover'].forEach((ev) => {
    dropzone.addEventListener(ev, (e) => {
      e.preventDefault();
      dropzone.classList.add('dragover');
    });
  });
  ['dragleave', 'drop'].forEach((ev) => {
    dropzone.addEventListener(ev, (e) => {
      e.preventDefault();
      dropzone.classList.remove('dragover');
    });
  });
  dropzone.addEventListener('drop', (e) => {
    if (e.dataTransfer?.files?.length) {
      void importAudioFiles(e.dataTransfer.files);
    }
  });

  // ---- Tactile TrimUI Brick Hardware Buttons (Click + Touch + Firmware 650ms Hold) ----
  function pressHw(mask: number, el?: Element | null): void {
    el?.classList.add('pressed');
    uipod.button_down(mask);
  }

  function releaseHw(mask: number, el?: Element | null): void {
    el?.classList.remove('pressed');
    uipod.button_up(mask);
  }

  document.querySelectorAll<HTMLButtonElement>('[data-btn]').forEach((btnEl) => {
    const name = btnEl.dataset.btn || '';
    const mask = BTN[name];
    if (!mask) return;

    btnEl.addEventListener(
      'touchstart',
      (e) => {
        e.preventDefault();
      },
      { passive: false },
    );
    btnEl.addEventListener(
      'touchend',
      (e) => {
        e.preventDefault();
      },
      { passive: false },
    );
    btnEl.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      try {
        btnEl.setPointerCapture(e.pointerId);
      } catch {
        // Ignore on synthetic touch events
      }
      pressHw(mask, btnEl);
    });
    const end = () => releaseHw(mask, btnEl);
    btnEl.addEventListener('pointerup', end);
    btnEl.addEventListener('pointercancel', end);
  });

  // Prevent iOS Safari text selection, callout menu, pinch-zoom & double-tap zoom inside the handheld
  deviceStage.addEventListener('contextmenu', (e) => e.preventDefault());
  deviceStage.addEventListener('selectstart', (e) => e.preventDefault());
  deviceStage.addEventListener('dblclick', (e) => e.preventDefault());
  deviceStage.addEventListener('gesturestart', (e) => e.preventDefault(), { passive: false });
  deviceStage.addEventListener('gesturechange', (e) => e.preventDefault(), { passive: false });
  let lastTouchEnd = 0;
  deviceStage.addEventListener(
    'touchend',
    (e) => {
      const now = Date.now();
      if (now - lastTouchEnd <= 320 && deviceStage.classList.contains('is-fullscreen')) {
        e.preventDefault();
      }
      lastTouchEnd = now;
    },
    { passive: false },
  );

  const bindFastTap = (el: HTMLElement | null, fn: () => void) => {
    if (!el) return;
    el.addEventListener(
      'touchend',
      (e) => {
        e.preventDefault();
        fn();
      },
      { passive: false },
    );
    el.addEventListener('click', (e) => {
      if ((e as PointerEvent).pointerType === 'touch') return;
      fn();
    });
  };

  bindFastTap(document.getElementById('btn-vol-up'), () => uipod.volume_step(2));
  bindFastTap(document.getElementById('btn-vol-down'), () => uipod.volume_step(-2));
  bindFastTap(document.getElementById('btn-fs-vol-up'), () => uipod.volume_step(2));
  bindFastTap(document.getElementById('btn-fs-vol-down'), () => uipod.volume_step(-2));

  function toggleHold(): void {
    const nextHold = !holdBtn.classList.contains('active');
    holdBtn.classList.toggle('active', nextHold);
    holdBtn.setAttribute('aria-pressed', String(nextHold));
    btnFsHold.classList.toggle('active', nextHold);
    uipod.set_hold(nextHold);
  }
  bindFastTap(holdBtn, toggleHold);
  bindFastTap(btnFsHold, toggleHold);

  // ---- Fullscreen Handheld Mode (Phone & Desktop) ----
  function setFullscreenMode(enable: boolean): void {
    deviceStage.classList.toggle('is-fullscreen', enable);
    document.body.classList.toggle('uipod-fullscreen', enable);
    if (enable) {
      if (!document.fullscreenElement && document.documentElement.requestFullscreen) {
        document.documentElement.requestFullscreen().catch(() => {
          // iOS Safari falls back cleanly to CSS 100dvh fixed fullscreen overlay
        });
      }
    } else if (document.fullscreenElement && document.exitFullscreen) {
      document.exitFullscreen().catch(() => {});
    }
  }

  btnFullscreen.addEventListener('click', () => setFullscreenMode(true));
  bindFastTap(btnFsExit, () => setFullscreenMode(false));
  document.addEventListener('fullscreenchange', () => {
    if (!document.fullscreenElement && deviceStage.classList.contains('is-fullscreen')) {
      deviceStage.classList.remove('is-fullscreen');
      document.body.classList.remove('uipod-fullscreen');
    }
  });

  // ---- Keyboard Controls ----
  const KEY_MAP: Record<string, string> = {
    ArrowUp: 'UP',
    KeyW: 'UP',
    ArrowDown: 'DOWN',
    KeyS: 'DOWN',
    ArrowLeft: 'LEFT',
    KeyA: 'LEFT',
    ArrowRight: 'RIGHT',
    KeyD: 'RIGHT',
    Enter: 'A',
    KeyZ: 'A',
    Escape: 'B',
    Backspace: 'B',
    KeyX: 'B',
    KeyC: 'X',
    KeyV: 'Y',
    Space: 'START',
    KeyM: 'MENU',
    KeyQ: 'L1',
    KeyE: 'R1',
    Digit1: 'L2',
    Digit2: 'R2',
  };

  window.addEventListener('keydown', (e) => {
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === 'TEXTAREA' || tag === 'INPUT' || tag === 'SELECT') return;

    if (e.code === 'Equal' || e.code === 'NumpadAdd') {
      e.preventDefault();
      uipod.volume_step(2);
      return;
    }
    if (e.code === 'Minus' || e.code === 'NumpadSubtract') {
      e.preventDefault();
      uipod.volume_step(-2);
      return;
    }
    if (e.code === 'KeyH') {
      e.preventDefault();
      holdBtn.click();
      return;
    }

    const name = KEY_MAP[e.code];
    if (!name) return;
    e.preventDefault();
    if (e.repeat) return;
    const el = document.querySelector(`[data-btn="${name}"]`);
    pressHw(BTN[name], el);
  });

  window.addEventListener('keyup', (e) => {
    const name = KEY_MAP[e.code];
    if (!name) return;
    const el = document.querySelector(`[data-btn="${name}"]`);
    releaseHw(BTN[name], el);
  });

  // ---- USB / Bluetooth Gamepad Polling ----
  let prevGpMask = 0;
  function pollGamepad(): void {
    const pads = navigator.getGamepads?.();
    if (!pads) return;
    const gp = pads.find((p) => p && p.connected);
    if (!gp) return;

    let mask = 0;
    if (gp.buttons[0]?.pressed) mask |= BTN.A;
    if (gp.buttons[1]?.pressed) mask |= BTN.B;
    if (gp.buttons[2]?.pressed) mask |= BTN.X;
    if (gp.buttons[3]?.pressed) mask |= BTN.Y;
    if (gp.buttons[4]?.pressed) mask |= BTN.L1;
    if (gp.buttons[5]?.pressed) mask |= BTN.R1;
    if (gp.buttons[6]?.pressed) mask |= BTN.L2;
    if (gp.buttons[7]?.pressed) mask |= BTN.R2;
    if (gp.buttons[8]?.pressed) mask |= BTN.SELECT;
    if (gp.buttons[9]?.pressed) mask |= BTN.START;
    if (gp.buttons[12]?.pressed || (gp.axes[1] ?? 0) < -0.5) mask |= BTN.UP;
    if (gp.buttons[13]?.pressed || (gp.axes[1] ?? 0) > 0.5) mask |= BTN.DOWN;
    if (gp.buttons[14]?.pressed || (gp.axes[0] ?? 0) < -0.5) mask |= BTN.LEFT;
    if (gp.buttons[15]?.pressed || (gp.axes[0] ?? 0) > 0.5) mask |= BTN.RIGHT;
    if (gp.buttons[16]?.pressed) mask |= BTN.MENU;

    const down = mask & ~prevGpMask;
    const up = prevGpMask & ~mask;
    if (down) uipod.button_down(down);
    if (up) uipod.button_up(up);
    prevGpMask = mask;
  }

  // ---- Process Outbound Commands from WASM UIPod -> Web Audio ----
  function processWasmCommands(): void {
    const raw = uipod.take_commands();
    if (raw === '[]') return;
    const cmds = JSON.parse(raw) as Array<Record<string, unknown>>;
    for (const cmd of cmds) {
      switch (cmd.type) {
        case 'click':
          audio.playMenuClick();
          break;
        case 'playTrack':
          audio.playTrack(Number(cmd.index ?? 0), 0);
          break;
        case 'quickCutNext':
          audio.quickCutToTrack(Number(cmd.index ?? 0));
          break;
        case 'playTrackAt':
          audio.playTrack(Number(cmd.index ?? 0), Number(cmd.posMs ?? 0) / 1000);
          break;
        case 'setPlaying':
          audio.setPlaying(Boolean(cmd.playing), Number(cmd.trackIndex ?? 0));
          break;
        case 'seek':
          audio.seekMs(Number(cmd.posMs ?? 0));
          break;
        case 'seekMotor':
          audio.setSeekMotor(Number(cmd.dir ?? 0), Boolean(cmd.enabled));
          break;
        case 'volume':
          audio.setVolume(Number(cmd.norm ?? 0.8));
          break;
        case 'dsp':
          audio.applyDsp({
            fxEnabled: cmd.fxEnabled !== false,
            eq: Boolean(cmd.eq),
            eqBypass: Boolean(cmd.eqBypass),
            autoeq: Number(cmd.autoeq ?? 0),
            eqPreamp: typeof cmd.eqPreamp === 'number' ? cmd.eqPreamp : 0,
            eqBands: Array.isArray(cmd.eqBands) ? (cmd.eqBands as number[]) : undefined,
            tape: Boolean(cmd.tape),
            wow: Number(cmd.wow ?? 35),
            hiss: Number(cmd.hiss ?? 22),
            sat: Number(cmd.sat ?? 30),
            hf: Number(cmd.hf ?? 40),
            stopStart: Boolean(cmd.stopStart),
            speed: Number(cmd.speed ?? 100),
            reverb: Boolean(cmd.reverb),
            reverbSize: Number(cmd.reverbSize ?? 65),
            reverbDamp: Number(cmd.reverbDamp ?? 45),
            reverbPredelay: Number(cmd.reverbPredelay ?? 20),
            reverbMix: Number(cmd.reverbMix ?? 30),
            delay: Boolean(cmd.delay),
            delayTime: Number(cmd.delayTime ?? 360),
            delayFeedback: Number(cmd.delayFeedback ?? 45),
            delayTone: Number(cmd.delayTone ?? 60),
            delayMix: Number(cmd.delayMix ?? 35),
            chorus: Boolean(cmd.chorus),
            chorusRate: Number(cmd.chorusRate ?? 35),
            chorusDepth: Number(cmd.chorusDepth ?? 50),
            chorusMix: Number(cmd.chorusMix ?? 40),
            flanger: Boolean(cmd.flanger),
            flangerRate: Number(cmd.flangerRate ?? 25),
            flangerDepth: Number(cmd.flangerDepth ?? 60),
            flangerFeedback: Number(cmd.flangerFeedback ?? 60),
            flangerMix: Number(cmd.flangerMix ?? 45),
            djMode: typeof cmd.djMode === 'number' ? cmd.djMode : audio.dsp.djMode,
            djLength: typeof cmd.djLength === 'number' ? cmd.djLength : audio.dsp.djLength,
            djMaxStretch:
              typeof cmd.djMaxStretch === 'number' ? cmd.djMaxStretch : audio.dsp.djMaxStretch,
            djKeepGapless:
              typeof cmd.djKeepGapless === 'boolean'
                ? cmd.djKeepGapless
                : audio.dsp.djKeepGapless,
            djQuickCut:
              typeof cmd.djQuickCut === 'boolean' ? cmd.djQuickCut : audio.dsp.djQuickCut,
          });
          if (typeof cmd.djMode === 'number') {
            try {
              localStorage.setItem(
                'uipod_dj_cfg_v1',
                JSON.stringify({
                  mode: cmd.djMode,
                  length: cmd.djLength ?? 1,
                  maxStretch: cmd.djMaxStretch ?? 1,
                  keepGapless: cmd.djKeepGapless !== false,
                  quickCut: cmd.djQuickCut !== false,
                }),
              );
            } catch {
              // Ignore storage access errors
            }
          }
          break;
      }
    }
  }

  // ---- Sync Live WASM State -> Landing Page Theme Halo & Studio Controls ----
  function syncStudioHud(): void {
    const raw = uipod.get_state_json();
    if (raw === lastStateJson) return;
    lastStateJson = raw;
    const s: AppStateSnapshot = JSON.parse(raw);
    lastState = s;

    // Live CSS variables drive the ambient halo & tactile LEDs!
    const root = document.documentElement.style;
    root.setProperty('--lcd-bg', s.bg);
    root.setProperty('--lcd-fg', s.fg);
    root.setProperty('--lcd-fg1', s.fg1);
    root.setProperty('--lcd-fg2', s.fg2);

    dotBg.style.background = s.bg;
    dotFg1.style.background = s.fg1;
    dotFg2.style.background = s.fg2;
    dotFg.style.background = s.fg;

    if (s.screen === 'music_quiz') {
      hudTitle.textContent = '???';
      hudArtist.textContent = uiLang === 'uk' ? 'Вгадай мелодію' : 'Music Quiz';
    } else {
      hudTitle.textContent = s.title;
      hudArtist.textContent = s.artist;
    }
    hudTheme.textContent = s.themeName;

    iconPlay.style.display = s.playing ? 'none' : 'block';
    iconPause.style.display = s.playing ? 'block' : 'none';

    holdBtn.classList.toggle('active', s.hold);
    holdBtn.setAttribute('aria-pressed', String(s.hold));
    btnFsHold.classList.toggle('active', s.hold);

    // Sync Theme Studio inputs
    pickBg.value = s.bg;
    pickFg1.value = s.fg1;
    pickFg2.value = s.fg2;
    pickFg.value = s.fg;
    hexBg.textContent = s.bg;
    hexFg1.textContent = s.fg1;
    hexFg2.textContent = s.fg2;
    hexFg.textContent = s.fg;
    chkMonoArt.checked = s.monoArt;
    selTransition.value = String(s.transition);
    themeButtons.forEach((b, i) => b.classList.toggle('active', i === s.themeIndex));

    // Sync Visualizer Lab
    vizButtons.forEach((b, i) => b.classList.toggle('active', !s.vizCustom && i === s.vizPreset));
    chkVizFine.checked = s.vizFine;
    lblVizGrain.textContent = s.vizFine ? '2×2 Fine' : '4×4 LCD';
    if (editedPresetIdx !== s.vizPreset && !s.vizCustom) {
      editedPresetIdx = s.vizPreset;
      glslEditor.value = get_viz_preset_shader(s.vizPreset).trim();
    }

    // Sync Sound Rack
    chkEq.checked = s.eq;
    selAutoeq.value = String(s.autoeq);
    chkTape.checked = s.tape;
    chkMotor.checked = s.stopStart;
    chkClick.checked = s.menuClick;
    rngWow.value = String(s.wow);
    rngHiss.value = String(s.hiss);
    rngSat.value = String(s.sat);
    rngHf.value = String(s.hf);
    rngSpeed.value = String(s.speed);
    valWow.textContent = `${s.wow}%`;
    valHiss.textContent = `${s.hiss}%`;
    valSat.textContent = `${s.sat}%`;
    valHf.textContent = `${s.hf}%`;
    valSpeed.textContent = `${(s.speed / 100).toFixed(2)}×`;
  }

  // Initial shader editor population
  glslEditor.value = get_viz_preset_shader(9).trim();
  editedPresetIdx = 9;

  // Optional URL query params for shareable deep-links (?screen=visualizer&viz=9&theme=23&lang=uk)
  const params = new URLSearchParams(window.location.search);
  const qLang = params.get('lang');
  if (qLang === 'uk' || qLang === 'en') {
    applyPageLang(qLang, true);
  }
  const qTheme = params.get('theme');
  if (qTheme !== null && !Number.isNaN(Number(qTheme))) {
    uipod.set_theme_preset(Number(qTheme));
  }
  const qViz = params.get('viz');
  if (qViz !== null && !Number.isNaN(Number(qViz))) {
    const vIdx = Number(qViz);
    uipod.set_viz_preset(vIdx);
    glslEditor.value = get_viz_preset_shader(vIdx).trim();
    editedPresetIdx = vIdx;
  }
  const qTab = params.get('tab');
  if (qTab && ['themes', 'viz', 'sound', 'plugins'].includes(qTab)) {
    document.querySelectorAll<HTMLButtonElement>('.studio-tab').forEach((t) => {
      t.classList.toggle('active', t.dataset.tab === qTab);
    });
    document.querySelectorAll('.tab-pane').forEach((p) => {
      p.classList.toggle('active', p.id === `pane-${qTab}`);
    });
  }
  const qScreen = params.get('screen');
  if (qScreen) {
    uipod.jump_to_screen(qScreen);
  }
  if (params.get('fullscreen') === '1') {
    setFullscreenMode(true);
  }

  // Hide boot spinner right after first frame!
  let firstFrameDone = false;
  let lastTs = performance.now();
  let fpsAccum = 0;
  let fpsFrames = 0;

  function frame(ts: number): void {
    const dt = Math.min(0.1, Math.max(0.001, (ts - lastTs) / 1000));
    lastTs = ts;

    pollGamepad();
    audio.tickLookahead();
    uipod.sync_audio_state(audio.playing, audio.getPositionMs(), audio.getDurationMs());
    const pcm = audio.readStereoPcmS16();
    uipod.render(dt, pcm, audio.getSampleRate(), canvas.width, canvas.height);

    processWasmCommands();
    syncStudioHud();

    if (!firstFrameDone) {
      firstFrameDone = true;
      loader.classList.add('hidden');
    }

    fpsAccum += dt;
    fpsFrames += 1;
    if (fpsAccum >= 0.6) {
      const fps = Math.round(fpsFrames / fpsAccum);
      fpsBadge.textContent = `${fps} FPS · 1024×768`;
      fpsAccum = 0;
      fpsFrames = 0;
    }

    requestAnimationFrame(frame);
  }

  requestAnimationFrame(frame);
}

void boot();
