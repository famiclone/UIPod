package dev.famiclone.uipod;

import android.Manifest;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.SharedPreferences;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.res.AssetManager;
import android.graphics.Color;
import android.graphics.PixelFormat;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaPlayer;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.os.Process;
import android.os.SystemClock;
import android.provider.Settings;
import android.system.Os;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.RelativeLayout;

import org.libsdl.app.SDLActivity;
import org.libsdl.app.SDLSurface;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * Main Android Handheld Activity for UIPod:
 * - Unpacks bundled UIPod runtime assets to `/data/data/dev.famiclone.uipod/files/uipod`
 * - Centers SDL2's OpenGL ES 3.0 surface in an exact 4:3 frame over OLED black bars
 * - Hosts a hardware-decoded `MediaPlayer` `SurfaceView` layer underneath SDL2
 *   for Movies, TV Shows, and Jellyfin streaming
 * - Bridges physical handheld volume rockers & Android `STREAM_MUSIC` with UIPod
 * - Keeps playback & Wi-Fi active via `UIPodPlaybackService` when the screen is off
 */
public class UIPodActivity extends SDLActivity implements SurfaceHolder.Callback {
    private static final String CANONICAL_RBDIR = "/data/data/dev.famiclone.uipod/files/uipod";
    private static final String PREFS_NAME = "uipod_android_prefs";
    private static final String PREF_ASKED_MANAGE_STORAGE = "asked_manage_storage";
    private static final String PREF_MIGRATED_VOLUME_DDB_V2 = "migrated_volume_ddb_v2";
    private static final String ACTION_VOLUME_CHANGED = "android.media.VOLUME_CHANGED_ACTION";
    private static final String EXTRA_VOLUME_STREAM_TYPE = "android.media.EXTRA_VOLUME_STREAM_TYPE";

    private static volatile UIPodActivity sInstance;
    private static volatile boolean sNativeLoaded = false;
    private static volatile int sLastAppSetStreamVol = -1;
    private static volatile long sLastAppSetTimeMs = 0L;

    private SurfaceView mVideoSurface;
    private volatile SurfaceHolder mVideoHolder;
    private MediaPlayer mMediaPlayer;
    private AudioFocusRequest mAudioFocusRequest;
    private BroadcastReceiver mVolumeReceiver;
    private volatile boolean mVideoPrepared = false;
    private volatile boolean mVideoRunning = false;
    private volatile boolean mVideoStretch = false;
    private volatile int mVideoWidth = 0;
    private volatile int mVideoHeight = 0;

    public static native void nativeOnAndroidVolumeChanged(
            int cur, int min, int max, boolean isFixed);

    public static final class StreamVolumeReceiver extends BroadcastReceiver {
        @Override
        public void onReceive(Context context, Intent intent) {
            if (intent == null || !ACTION_VOLUME_CHANGED.equals(intent.getAction())) {
                return;
            }
            int streamType = intent.getIntExtra(EXTRA_VOLUME_STREAM_TYPE, -1);
            if (streamType != -1 && streamType != AudioManager.STREAM_MUSIC) {
                return;
            }
            UIPodActivity act = sInstance;
            if (act == null || !sNativeLoaded) {
                return;
            }
            AudioManager am = (AudioManager) act.getSystemService(Context.AUDIO_SERVICE);
            if (am == null) {
                return;
            }
            int cur = getSafeStreamVolume(am);
            if (cur == sLastAppSetStreamVol
                    && (SystemClock.uptimeMillis() - sLastAppSetTimeMs) < 500L) {
                return;
            }
            act.pushVolumeStateToNative(am);
        }
    }

    @Override
    protected String[] getLibraries() {
        return new String[] {
            "SDL2",
            "main"
        };
    }

    @Override
    protected SDLSurface createSDLSurface(Context context) {
        SDLSurface surface = super.createSDLSurface(context);
        try {
            surface.setZOrderMediaOverlay(true);
            surface.getHolder().setFormat(PixelFormat.TRANSLUCENT);
        } catch (Throwable ignored) {
        }
        return surface;
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        if (sNativeLoaded) {
            /* Never reuse a cached OS process whose C/Rust static globals already ran */
            Intent relaunch = getPackageManager().getLaunchIntentForPackage(getPackageName());
            if (relaunch != null) {
                relaunch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
                startActivity(relaunch);
            }
            Process.killProcess(Process.myPid());
            System.exit(0);
            return;
        }
        sInstance = this;
        prepareRuntimeEnvironment();

        super.onCreate(savedInstanceState);
        sNativeLoaded = !SDLActivity.mBrokenLibraries;

        setVolumeControlStream(AudioManager.STREAM_MUSIC);
        requestMediaAudioFocus();
        registerVolumeBroadcastReceiver();
        applyImmersiveFullscreen();
        setupFullscreenAndVideoLayer();
        requestStoragePermissionsIfNeeded();
        startPlaybackKeepAliveService();
        queryAndPushStreamVolume();
    }

    @Override
    protected void onResume() {
        super.onResume();
        sInstance = this;
        setVolumeControlStream(AudioManager.STREAM_MUSIC);
        requestMediaAudioFocus();
        applyImmersiveFullscreen();
        queryAndPushStreamVolume();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            applyImmersiveFullscreen();
        }
    }

    @Override
    protected void onDestroy() {
        unregisterVolumeBroadcastReceiver();
        abandonMediaAudioFocus();
        stopMediaPlayerInternal();
        try {
            stopService(new Intent(this, UIPodPlaybackService.class));
        } catch (Throwable ignored) {
        }
        if (sInstance == this) {
            sInstance = null;
        }
        try {
            SDLActivity.nativeSendQuit();
        } catch (Throwable ignored) {
        }
        Process.killProcess(Process.myPid());
        System.exit(0);
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        int keyCode = event.getKeyCode();
        if (keyCode == KeyEvent.KEYCODE_VOLUME_UP ||
            keyCode == KeyEvent.KEYCODE_VOLUME_DOWN ||
            keyCode == KeyEvent.KEYCODE_VOLUME_MUTE) {
            sLastAppSetStreamVol = -1;
            AudioManager am = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
            if (am != null && am.isVolumeFixed()) {
                if (event.getAction() == KeyEvent.ACTION_DOWN) {
                    SDLActivity.onNativeKeyDown(keyCode);
                } else if (event.getAction() == KeyEvent.ACTION_UP) {
                    SDLActivity.onNativeKeyUp(keyCode);
                }
                return true;
            }

            int beforeVol = (am != null) ? getSafeStreamVolume(am) : -1;

            /* Route through Activity/PhoneWindow volume handling while skipping
             * `SDLSurface.onKey` so SDL never swallows hardware volume rockers. */
            boolean handled = false;
            try {
                Window win = getWindow();
                View decor = (win != null) ? win.getDecorView() : null;
                handled = event.dispatch(
                        this,
                        decor != null ? decor.getKeyDispatcherState() : null,
                        this
                );
            } catch (Throwable ignored) {
            }

            if (am != null && event.getAction() == KeyEvent.ACTION_DOWN) {
                int afterVol = getSafeStreamVolume(am);
                if (!handled || afterVol == beforeVol) {
                    int dir = (keyCode == KeyEvent.KEYCODE_VOLUME_UP)
                            ? AudioManager.ADJUST_RAISE
                            : (keyCode == KeyEvent.KEYCODE_VOLUME_DOWN)
                            ? AudioManager.ADJUST_LOWER
                            : AudioManager.ADJUST_TOGGLE_MUTE;
                    try {
                        am.adjustStreamVolume(
                                AudioManager.STREAM_MUSIC,
                                dir,
                                AudioManager.FLAG_SHOW_UI
                        );
                    } catch (Throwable ignored) {
                    }
                }
                pushVolumeStateToNative(am);
            }
            return true;
        }
        if (isHandheldMenuKey(event)) {
            if (event.getAction() == KeyEvent.ACTION_DOWN) {
                if (event.getRepeatCount() == 0) {
                    SDLActivity.onNativeKeyDown(KeyEvent.KEYCODE_MENU);
                }
            } else if (event.getAction() == KeyEvent.ACTION_UP) {
                SDLActivity.onNativeKeyUp(KeyEvent.KEYCODE_MENU);
            }
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    private static boolean isHandheldMenuKey(KeyEvent event) {
        int keyCode = event.getKeyCode();
        switch (keyCode) {
            case KeyEvent.KEYCODE_MENU:
            case KeyEvent.KEYCODE_BUTTON_MODE:
            case KeyEvent.KEYCODE_BUTTON_SELECT:
            case KeyEvent.KEYCODE_BUTTON_C:
            case KeyEvent.KEYCODE_BUTTON_Z:
            case KeyEvent.KEYCODE_SETTINGS:
            case KeyEvent.KEYCODE_CAPTIONS:
            case KeyEvent.KEYCODE_GUIDE:
            case KeyEvent.KEYCODE_INFO:
            case KeyEvent.KEYCODE_BOOKMARK:
            case KeyEvent.KEYCODE_HELP:
            case KeyEvent.KEYCODE_ALL_APPS:
            case KeyEvent.KEYCODE_MEDIA_TOP_MENU:
                return true;
            case KeyEvent.KEYCODE_BACK:
                /* Physical handheld Back/Menu button has deviceId > 0; virtual
                 * Android swipe-back gesture has deviceId <= 0 (VIRTUAL_KEYBOARD). */
                return event.getDeviceId() > 0;
            case KeyEvent.KEYCODE_UNKNOWN:
                return event.getScanCode() > 0;
            default:
                if (keyCode >= KeyEvent.KEYCODE_BUTTON_1
                        && keyCode <= KeyEvent.KEYCODE_BUTTON_16) {
                    return true;
                }
                if (keyCode >= KeyEvent.KEYCODE_F1
                        && keyCode <= KeyEvent.KEYCODE_F12) {
                    return true;
                }
                return false;
        }
    }

    private void requestMediaAudioFocus() {
        try {
            AudioManager am = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
            if (am == null) {
                return;
            }
            if (Build.VERSION.SDK_INT >= 26) {
                if (mAudioFocusRequest == null) {
                    AudioAttributes attrs = new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build();
                    mAudioFocusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                            .setAudioAttributes(attrs)
                            .setWillPauseWhenDucked(false)
                            .setOnAudioFocusChangeListener(focusChange -> {
                                /* Keep handheld playback under user control */
                            })
                            .build();
                }
                am.requestAudioFocus(mAudioFocusRequest);
            } else {
                am.requestAudioFocus(null, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN);
            }
        } catch (Throwable ignored) {
        }
    }

    private void abandonMediaAudioFocus() {
        try {
            AudioManager am = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
            if (am != null && Build.VERSION.SDK_INT >= 26 && mAudioFocusRequest != null) {
                am.abandonAudioFocusRequest(mAudioFocusRequest);
            }
        } catch (Throwable ignored) {
        }
    }

    private void registerVolumeBroadcastReceiver() {
        if (mVolumeReceiver != null) {
            return;
        }
        mVolumeReceiver = new StreamVolumeReceiver();
        IntentFilter filter = new IntentFilter(ACTION_VOLUME_CHANGED);
        try {
            if (Build.VERSION.SDK_INT >= 33) {
                registerReceiver(mVolumeReceiver, filter, Context.RECEIVER_EXPORTED);
            } else {
                registerReceiver(mVolumeReceiver, filter);
            }
        } catch (Throwable ignored) {
        }
    }

    private void unregisterVolumeBroadcastReceiver() {
        if (mVolumeReceiver == null) {
            return;
        }
        try {
            unregisterReceiver(mVolumeReceiver);
        } catch (Throwable ignored) {
        }
        mVolumeReceiver = null;
    }

    private static int getSafeStreamVolume(AudioManager am) {
        try {
            return am.getStreamVolume(AudioManager.STREAM_MUSIC);
        } catch (Throwable t) {
            return 0;
        }
    }

    private static int getSafeStreamMinVolume(AudioManager am) {
        if (Build.VERSION.SDK_INT >= 28) {
            try {
                return am.getStreamMinVolume(AudioManager.STREAM_MUSIC);
            } catch (Throwable ignored) {
            }
        }
        return 0;
    }

    private static int getSafeStreamMaxVolume(AudioManager am) {
        try {
            return Math.max(1, am.getStreamMaxVolume(AudioManager.STREAM_MUSIC));
        } catch (Throwable t) {
            return 15;
        }
    }

    private void pushVolumeStateToNative(AudioManager am) {
        if (!sNativeLoaded || am == null) {
            return;
        }
        try {
            int cur = getSafeStreamVolume(am);
            int min = getSafeStreamMinVolume(am);
            int max = getSafeStreamMaxVolume(am);
            boolean fixed = am.isVolumeFixed();
            nativeOnAndroidVolumeChanged(cur, min, max, fixed);
        } catch (Throwable ignored) {
        }
    }

    public static void queryAndPushStreamVolume() {
        final UIPodActivity act = sInstance;
        if (act == null || !sNativeLoaded) {
            return;
        }
        try {
            AudioManager am = (AudioManager) act.getSystemService(Context.AUDIO_SERVICE);
            if (am != null) {
                act.pushVolumeStateToNative(am);
            }
        } catch (Throwable ignored) {
        }
    }

    public static void setStreamVolumePermille(final int permille) {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        try {
            AudioManager am = (AudioManager) act.getSystemService(Context.AUDIO_SERVICE);
            if (am == null || am.isVolumeFixed()) {
                return;
            }
            int min = getSafeStreamMinVolume(am);
            int max = getSafeStreamMaxVolume(am);
            int clamped = Math.max(0, Math.min(1000, permille));
            int target;
            if (clamped <= 0) {
                target = min;
            } else {
                target = min + Math.round(((max - min) * clamped) / 1000.0f);
                if (target <= min && max > min) {
                    target = min + 1;
                }
            }
            target = Math.max(min, Math.min(max, target));
            int cur = getSafeStreamVolume(am);
            if (target != cur) {
                sLastAppSetStreamVol = target;
                sLastAppSetTimeMs = SystemClock.uptimeMillis();
                am.setStreamVolume(AudioManager.STREAM_MUSIC, target, 0);
            }
        } catch (Throwable ignored) {
        }
    }

    private void applyImmersiveFullscreen() {
        Window window = getWindow();
        if (window == null) {
            return;
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN);
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams lp = window.getAttributes();
            lp.layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(lp);
        }
        View decor = window.getDecorView();
        decor.setBackgroundColor(Color.BLACK);
        decor.setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
        );
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        mVideoHolder = holder;
        if (mMediaPlayer != null) {
            try {
                mMediaPlayer.setDisplay(holder);
            } catch (Throwable ignored) {
            }
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        mVideoHolder = holder;
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        if (mVideoHolder == holder) {
            mVideoHolder = null;
        }
    }

    /**
     * Inserts `mVideoSurface` directly behind `mSurface` inside SDL2's existing `mLayout`
     * without ever detaching `SDLSurface` from the window (avoiding EGLSurface races).
     */
    private void setupFullscreenAndVideoLayer() {
        if (mLayout == null || mSurface == null) {
            return;
        }
        mLayout.setBackgroundColor(Color.BLACK);

        mVideoSurface = new SurfaceView(this);
        mVideoSurface.getHolder().addCallback(this);

        RelativeLayout.LayoutParams videoLp = new RelativeLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
        );
        videoLp.addRule(RelativeLayout.CENTER_IN_PARENT, RelativeLayout.TRUE);
        mLayout.addView(mVideoSurface, 0, videoLp);
        mLayout.addOnLayoutChangeListener(
                (v, left, top, right, bottom, oldLeft, oldTop, oldRight, oldBottom) -> {
                    if ((right - left) != (oldRight - oldLeft)
                            || (bottom - top) != (oldBottom - oldTop)) {
                        applyVideoLayout();
                    }
                }
        );
    }

    private void prepareRuntimeEnvironment() {
        File filesDir = getFilesDir();
        File cacheDir = getCacheDir();
        File rbDir = new File(filesDir, "uipod");
        rbDir.mkdirs();
        cacheDir.mkdirs();

        String sdRoot = "/storage/emulated/0";
        try {
            File ext = Environment.getExternalStorageDirectory();
            if (ext != null && ext.getAbsolutePath().length() > 0) {
                sdRoot = ext.getAbsolutePath();
            }
        } catch (Throwable ignored) {
        }

        try {
            Os.setenv("HOME", filesDir.getAbsolutePath(), true);
            Os.setenv("RBROOT", filesDir.getAbsolutePath(), true);
            Os.setenv("UIPOD_NATIVE_LIB_DIR", getApplicationInfo().nativeLibraryDir, true);
            Os.setenv("UIPOD_TMP_DIR", cacheDir.getAbsolutePath(), true);
            Os.setenv("UIPOD_SDCARD_DIR", sdRoot, true);
        } catch (Throwable ignored) {
        }

        String appVer = "0.5.0";
        long updateTime = 0L;
        try {
            PackageInfo pi = getPackageManager().getPackageInfo(getPackageName(), 0);
            if (pi.versionName != null) {
                appVer = pi.versionName;
            }
            updateTime = pi.lastUpdateTime;
        } catch (Throwable ignored) {
        }
        String stampValue = appVer + ":" + updateTime;
        File stampFile = new File(rbDir, ".uipod_asset_stamp");
        boolean freshInstall = !new File(rbDir, "config.cfg").exists()
                && !new File(filesDir, ".config/rockbox.org/config.cfg").exists();

        if (!stampValue.equals(readSmallText(stampFile))) {
            extractAssetTree(getAssets(), "uipod", rbDir, freshInstall);
            new File(rbDir, "eqs").mkdirs();
            new File(rbDir, "autoeq").mkdirs();
            new File(rbDir, "codecs").mkdirs();
            rewriteConfigPaths(rbDir, freshInstall);
            writeSmallText(stampFile, stampValue);
        }

        SharedPreferences prefs = getSharedPreferences(PREFS_NAME, MODE_PRIVATE);
        if (!prefs.getBoolean(PREF_MIGRATED_VOLUME_DDB_V2, false)) {
            migrateLegacyVolumeAttenuation(rbDir, filesDir);
            prefs.edit().putBoolean(PREF_MIGRATED_VOLUME_DDB_V2, true).apply();
        }
    }

    private void migrateLegacyVolumeAttenuation(File rbDir, File filesDir) {
        File[] targets = new File[] {
            new File(rbDir, "config.cfg"),
            new File(rbDir, ".resume.cfg"),
            new File(filesDir, ".config/rockbox.org/config.cfg"),
            new File(filesDir, ".config/rockbox.org/.resume.cfg")
        };
        for (File f : targets) {
            if (f.isFile()) {
                String s = readSmallText(f);
                if (s != null) {
                    String updated = s
                            .replace("volume_ddb: -220", "volume_ddb: 0")
                            .replace("volume_ddb: -22", "volume_ddb: 0")
                            .replace("volume: -220", "volume: 0");
                    if (!updated.equals(s)) {
                        writeSmallText(f, updated);
                    }
                }
            }
        }
    }

    private static boolean isUserEditableStateFile(String relPath) {
        return relPath.equals("config.cfg")
                || relPath.endsWith(".db")
                || relPath.equals("folders.txt")
                || relPath.equals("podcasts.txt")
                || relPath.equals("audiobooks.txt")
                || relPath.equals("movies.txt")
                || relPath.equals("television.txt")
                || relPath.equals("source.txt")
                || relPath.equals("navidrome.cfg")
                || relPath.equals("jellyfin.cfg")
                || relPath.equals("browse_last_dir.txt");
    }

    private void extractAssetTree(AssetManager am, String assetPath, File dstDir, boolean freshInstall) {
        try {
            String[] list = am.list(assetPath);
            if (list == null || list.length == 0) {
                return;
            }
            dstDir.mkdirs();
            byte[] buf = new byte[65536];
            for (String child : list) {
                String subAsset = assetPath + "/" + child;
                File subDst = new File(dstDir, child);
                String[] grand = am.list(subAsset);
                if (grand != null && grand.length > 0) {
                    extractAssetTree(am, subAsset, subDst, freshInstall);
                } else {
                    String rel = subAsset.startsWith("uipod/") ? subAsset.substring(6) : child;
                    if (!freshInstall && subDst.exists() && isUserEditableStateFile(rel)) {
                        continue;
                    }
                    try (InputStream in = am.open(subAsset);
                         FileOutputStream out = new FileOutputStream(subDst)) {
                        int n;
                        while ((n = in.read(buf)) > 0) {
                            out.write(buf, 0, n);
                        }
                    } catch (IOException ignored) {
                    }
                }
            }
        } catch (IOException ignored) {
        }
    }

    private void rewriteConfigPaths(File rbDir, boolean freshInstall) {
        File themesDir = new File(rbDir, "themes");
        File[] themes = themesDir.listFiles();
        if (themes != null) {
            for (File t : themes) {
                if (t.isFile() && t.getName().endsWith(".cfg")) {
                    String s = readSmallText(t);
                    if (s != null) {
                        s = s.replace("/.rockbox", CANONICAL_RBDIR)
                             .replace("/tmp/uipod", CANONICAL_RBDIR)
                             .replace("/tmp/trimpod", CANONICAL_RBDIR);
                        writeSmallText(t, s);
                    }
                }
            }
        }

        File[] cfgs = new File[] {
            new File(rbDir, "config.cfg"),
            new File(rbDir, ".resume.cfg"),
            new File(getFilesDir(), ".config/rockbox.org/config.cfg"),
            new File(getFilesDir(), ".config/rockbox.org/.resume.cfg")
        };
        for (File cfg : cfgs) {
            if (cfg.isFile()) {
                String s = readSmallText(cfg);
                if (s != null) {
                    s = s.replace("/.rockbox", CANONICAL_RBDIR)
                         .replace("/tmp/uipod", CANONICAL_RBDIR)
                         .replace("/tmp/trimpod", CANONICAL_RBDIR);
                    if (freshInstall) {
                        s = s.replace("volume_ddb: -220", "volume_ddb: 0")
                             .replace("volume_ddb: -22", "volume_ddb: 0")
                             .replace("volume: -220", "volume: 0");
                    }
                    writeSmallText(cfg, s);
                }
            }
        }
    }

    private static String readSmallText(File f) {
        if (!f.isFile()) {
            return null;
        }
        try (FileInputStream in = new FileInputStream(f);
             ByteArrayOutputStream bos = new ByteArrayOutputStream()) {
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) > 0) {
                bos.write(buf, 0, n);
            }
            return new String(bos.toByteArray(), StandardCharsets.UTF_8);
        } catch (IOException e) {
            return null;
        }
    }

    private static void writeSmallText(File f, String content) {
        try {
            File parent = f.getParentFile();
            if (parent != null) {
                parent.mkdirs();
            }
            try (FileOutputStream out = new FileOutputStream(f)) {
                out.write(content.getBytes(StandardCharsets.UTF_8));
            }
        } catch (IOException ignored) {
        }
    }

    private void requestStoragePermissionsIfNeeded() {
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                if (!Environment.isExternalStorageManager()) {
                    SharedPreferences prefs = getSharedPreferences(PREFS_NAME, MODE_PRIVATE);
                    if (!prefs.getBoolean(PREF_ASKED_MANAGE_STORAGE, false)) {
                        prefs.edit().putBoolean(PREF_ASKED_MANAGE_STORAGE, true).apply();
                        try {
                            Intent intent = new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION);
                            intent.setData(Uri.parse("package:" + getPackageName()));
                            startActivity(intent);
                        } catch (Throwable fallback) {
                            Intent intent = new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION);
                            startActivity(intent);
                        }
                    }
                }
                if (Build.VERSION.SDK_INT >= 33) {
                    if (checkSelfPermission(Manifest.permission.READ_MEDIA_AUDIO)
                            != PackageManager.PERMISSION_GRANTED) {
                        requestPermissions(new String[] {
                            Manifest.permission.READ_MEDIA_AUDIO,
                            Manifest.permission.READ_MEDIA_VIDEO,
                            Manifest.permission.POST_NOTIFICATIONS
                        }, 101);
                    }
                }
            } else {
                if (checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE)
                        != PackageManager.PERMISSION_GRANTED) {
                    requestPermissions(new String[] {
                        Manifest.permission.READ_EXTERNAL_STORAGE,
                        Manifest.permission.WRITE_EXTERNAL_STORAGE
                    }, 100);
                }
            }
        } catch (Throwable ignored) {
        }
    }

    private void startPlaybackKeepAliveService() {
        try {
            Intent svc = new Intent(this, UIPodPlaybackService.class);
            startForegroundService(svc);
        } catch (Throwable ignored) {
        }
    }

    /* ---- JNI Callbacks Invoked from C (`backlight-target.c` & `video-target.c`) ---- */

    public static void setAppBrightness(final int step) {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        act.runOnUiThread(() -> {
            Window w = act.getWindow();
            if (w == null) {
                return;
            }
            WindowManager.LayoutParams lp = w.getAttributes();
            if (step <= 0) {
                lp.screenBrightness = 0.01f;
            } else {
                float norm = Math.min(1.0f, Math.max(0.05f, step / 17.0f));
                lp.screenBrightness = norm;
            }
            w.setAttributes(lp);
        });
    }

    public static boolean videoStart(final String path, final int startMs) {
        final UIPodActivity act = sInstance;
        if (act == null || path == null || path.isEmpty()) {
            return false;
        }
        final CountDownLatch latch = new CountDownLatch(1);
        final boolean[] ok = new boolean[] { false };
        act.runOnUiThread(() -> {
            act.stopMediaPlayerInternal();
            try {
                MediaPlayer mp = new MediaPlayer();
                mp.setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                        .build());
                if (act.mVideoHolder != null) {
                    mp.setDisplay(act.mVideoHolder);
                }
                if (path.startsWith("http://") || path.startsWith("https://")) {
                    mp.setDataSource(act, Uri.parse(path));
                } else {
                    mp.setDataSource(path);
                }
                mp.setOnVideoSizeChangedListener((player, width, height) -> {
                    act.mVideoWidth = width;
                    act.mVideoHeight = height;
                    act.applyVideoLayout();
                });
                mp.setOnCompletionListener(player -> act.mVideoRunning = false);
                mp.setOnErrorListener((player, what, extra) -> {
                    act.mVideoRunning = false;
                    return true;
                });
                mp.setOnPreparedListener(player -> {
                    act.mVideoPrepared = true;
                    act.mVideoRunning = true;
                    act.mVideoWidth = player.getVideoWidth();
                    act.mVideoHeight = player.getVideoHeight();
                    act.applyVideoLayout();
                    if (startMs > 0) {
                        player.seekTo(startMs);
                    }
                    player.start();
                });
                act.mMediaPlayer = mp;
                act.mVideoPrepared = false;
                act.mVideoRunning = true;
                mp.prepareAsync();
                ok[0] = true;
            } catch (Throwable t) {
                act.stopMediaPlayerInternal();
                ok[0] = false;
            } finally {
                latch.countDown();
            }
        });
        try {
            latch.await(2, TimeUnit.SECONDS);
        } catch (InterruptedException ignored) {
        }
        return ok[0];
    }

    public static void videoStop() {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        act.runOnUiThread(act::stopMediaPlayerInternal);
    }

    public static boolean videoIsRunning() {
        final UIPodActivity act = sInstance;
        return act != null && act.mVideoRunning;
    }

    public static int[] videoGetState() {
        final UIPodActivity act = sInstance;
        int[] out = new int[] { 0, 0, 0, 0 };
        if (act == null || !act.mVideoPrepared) {
            return out;
        }
        MediaPlayer mp = act.mMediaPlayer;
        if (mp == null) {
            return out;
        }
        try {
            out[0] = 1;
            out[1] = Math.max(0, mp.getCurrentPosition());
            out[2] = Math.max(0, mp.getDuration());
            out[3] = mp.isPlaying() ? 1 : 0;
        } catch (Throwable ignored) {
            out[0] = 0;
        }
        return out;
    }

    public static void videoPause(final boolean paused) {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        act.runOnUiThread(() -> {
            if (act.mMediaPlayer != null && act.mVideoPrepared) {
                try {
                    if (paused && act.mMediaPlayer.isPlaying()) {
                        act.mMediaPlayer.pause();
                    } else if (!paused && !act.mMediaPlayer.isPlaying()) {
                        act.mMediaPlayer.start();
                    }
                } catch (Throwable ignored) {
                }
            }
        });
    }

    public static void videoSeek(final int positionMs) {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        act.runOnUiThread(() -> {
            if (act.mMediaPlayer != null && act.mVideoPrepared) {
                try {
                    act.mMediaPlayer.seekTo(Math.max(0, positionMs));
                } catch (Throwable ignored) {
                }
            }
        });
    }

    public static void videoSetStretch(final boolean stretch) {
        final UIPodActivity act = sInstance;
        if (act == null) {
            return;
        }
        act.mVideoStretch = stretch;
        act.runOnUiThread(act::applyVideoLayout);
    }

    private void applyVideoLayout() {
        if (mVideoSurface == null) {
            return;
        }
        ViewGroup parent = (ViewGroup) mVideoSurface.getParent();
        if (parent == null) {
            return;
        }
        int pw = parent.getWidth();
        int ph = parent.getHeight();
        if (pw <= 0 || ph <= 0) {
            return;
        }
        int vw = pw;
        int vh = ph;
        if (!mVideoStretch && mVideoWidth > 0 && mVideoHeight > 0) {
            if (mVideoWidth * ph > mVideoHeight * pw) {
                vw = pw;
                vh = (pw * mVideoHeight) / mVideoWidth;
            } else {
                vh = ph;
                vw = (ph * mVideoWidth) / mVideoHeight;
            }
        }
        RelativeLayout.LayoutParams lp = new RelativeLayout.LayoutParams(vw, vh);
        lp.addRule(RelativeLayout.CENTER_IN_PARENT, RelativeLayout.TRUE);
        mVideoSurface.setLayoutParams(lp);
    }

    private void stopMediaPlayerInternal() {
        mVideoPrepared = false;
        mVideoRunning = false;
        if (mMediaPlayer != null) {
            try {
                mMediaPlayer.stop();
            } catch (Throwable ignored) {
            }
            try {
                mMediaPlayer.release();
            } catch (Throwable ignored) {
            }
            mMediaPlayer = null;
        }
    }
}
