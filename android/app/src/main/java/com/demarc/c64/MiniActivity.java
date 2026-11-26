package com.demarc.c64;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.content.res.Configuration;
import android.graphics.Insets;
import android.database.Cursor;
import android.media.session.MediaSession;
import android.net.Uri;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.Display;
import android.view.KeyEvent;
import android.view.RoundedCorner;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/** NativeActivity in immersive mode: the theme alone cannot hide the navigation bar. */
public class MiniActivity extends NativeActivity {
    private static final int PICK_KICKSTARTS = 1;
    private static final long MAX_PICKED_SIZE = 16 << 20;

    private MediaSession session;
    private String kickstartDir;

    /** Hands an Android keycode to minimarc's media handling; false if it is not a media key. */
    private static native boolean nativeMediaKey(int keyCode);

    /** Tells minimarc the picked Kickstarts are copied and can be imported. */
    private static native void nativeKickstartsPicked();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // NativeActivity's own load does not register the library for JNI lookups.
        System.loadLibrary("minimarc");
        super.onCreate(savedInstanceState);
        startMediaSession();
        if (Build.VERSION.SDK_INT >= 28) {
            getWindow().getAttributes().layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }
        hideSystemBars();
        preferFastestMode();
        // The native side reads the safe area off the content rect.
        if (Build.VERSION.SDK_INT >= 30) {
            findViewById(android.R.id.content).setOnApplyWindowInsetsListener((v, insets) -> {
                Insets safe = safeInsets(insets);
                int bottom = safe.bottom;
                if (getResources().getConfiguration().orientation
                        == Configuration.ORIENTATION_PORTRAIT) {
                    bottom = Math.max(bottom, insets.getInsets(WindowInsets.Type.ime()).bottom);
                }
                v.setPadding(safe.left, safe.top, safe.right, bottom);
                return insets;
            });
        }
    }

    /** 50 Hz frames pace far more evenly at 120 Hz than at 60. */
    @SuppressWarnings("deprecation")
    private void preferFastestMode() {
        Display display = getWindowManager().getDefaultDisplay();
        Display.Mode current = display.getMode();
        Display.Mode best = current;
        for (Display.Mode m : display.getSupportedModes()) {
            if (m.getPhysicalWidth() == current.getPhysicalWidth()
                    && m.getPhysicalHeight() == current.getPhysicalHeight()
                    && m.getRefreshRate() > best.getRefreshRate()) {
                best = m;
            }
        }
        WindowManager.LayoutParams lp = getWindow().getAttributes();
        lp.preferredDisplayModeId = best.getModeId();
        getWindow().setAttributes(lp);
    }

    /** The display cutout, grown so no corner of the rect is cut by a rounded screen corner. */
    static Insets safeInsets(WindowInsets insets) {
        Insets cut = insets.getInsets(WindowInsets.Type.displayCutout());
        int left = cut.left, top = cut.top, right = cut.right, bottom = cut.bottom;
        if (Build.VERSION.SDK_INT >= 31) {
            int tl = corner(insets, RoundedCorner.POSITION_TOP_LEFT);
            int tr = corner(insets, RoundedCorner.POSITION_TOP_RIGHT);
            int bl = corner(insets, RoundedCorner.POSITION_BOTTOM_LEFT);
            int br = corner(insets, RoundedCorner.POSITION_BOTTOM_RIGHT);
            left = Math.max(left, Math.max(tl, bl));
            top = Math.max(top, Math.max(tl, tr));
            right = Math.max(right, Math.max(tr, br));
            bottom = Math.max(bottom, Math.max(bl, br));
        }
        return Insets.of(left, top, right, bottom);
    }

    /** How far in from both edges a rect corner has to be to clear a corner of radius r. */
    private static int corner(WindowInsets insets, int position) {
        RoundedCorner c = insets.getRoundedCorner(position);
        return c == null ? 0 : (int) Math.ceil(c.getRadius() * (1 - Math.sqrt(0.5)));
    }

    /** Headset buttons in the background; in the foreground they come in as keys. */
    private void startMediaSession() {
        session = new MediaSession(this, "minimarc");
        session.setCallback(new MediaSession.Callback() {
            @Override
            @SuppressWarnings("deprecation")
            public boolean onMediaButtonEvent(Intent intent) {
                KeyEvent e = intent.getParcelableExtra(Intent.EXTRA_KEY_EVENT);
                if (e == null) {
                    return false;
                }
                if (e.getAction() != KeyEvent.ACTION_DOWN || e.getRepeatCount() != 0) {
                    return true;
                }
                return nativeMediaKey(e.getKeyCode());
            }
        });
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_SKIP_TO_NEXT
                        | PlaybackState.ACTION_SKIP_TO_PREVIOUS)
                .setState(PlaybackState.STATE_PLAYING, 0, 1)
                .build());
        session.setActive(true);
    }

    /** Called from native code: the APK of an installed package, or null. */
    public String packagePath(String pkg) {
        try {
            return getPackageManager().getApplicationInfo(pkg, 0).sourceDir;
        } catch (Exception e) {
            return null;
        }
    }

    /** Called from native code: let the user pick Kickstarts, or zips of them, to copy into dest. */
    public void pickKickstarts(String dest) {
        runOnUiThread(() -> {
            kickstartDir = dest;
            startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT)
                    .addCategory(Intent.CATEGORY_OPENABLE)
                    .setType("*/*")
                    .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true), PICK_KICKSTARTS);
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != PICK_KICKSTARTS || resultCode != RESULT_OK || data == null
                || kickstartDir == null) {
            return;
        }
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                copyPicked(clip.getItemAt(i).getUri());
            }
        } else if (data.getData() != null) {
            copyPicked(data.getData());
        }
        nativeKickstartsPicked();
    }

    private void copyPicked(Uri uri) {
        String name = "picked";
        long size = -1;
        try (Cursor c = getContentResolver().query(uri, null, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                int n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                int s = c.getColumnIndex(OpenableColumns.SIZE);
                if (n >= 0 && c.getString(n) != null) {
                    name = new File(c.getString(n)).getName();
                }
                if (s >= 0 && !c.isNull(s)) {
                    size = c.getLong(s);
                }
            }
        }
        if (size > MAX_PICKED_SIZE) {
            return;
        }
        File dir = new File(kickstartDir);
        dir.mkdirs();
        try (InputStream in = getContentResolver().openInputStream(uri);
             OutputStream out = new FileOutputStream(new File(dir, name))) {
            byte[] buf = new byte[65536];
            for (int r; (r = in.read(buf)) > 0; ) {
                out.write(buf, 0, r);
            }
        } catch (Exception e) {
            Log.w("minimarc", "Can't copy " + uri, e);
        }
    }

    /** Called from native code: show a release's page, styled by the asset `css`. */
    public void openWeb(String url, String css) {
        startActivity(new Intent(this, WebActivity.class)
                .putExtra(WebActivity.URL, url)
                .putExtra(WebActivity.CSS, css));
    }

    // Started while hidden so the process is not frozen and the demo keeps playing.
    @Override
    protected void onPause() {
        super.onPause();
        startForegroundService(new Intent(this, PlaybackService.class));
    }

    @Override
    protected void onResume() {
        super.onResume();
        stopService(new Intent(this, PlaybackService.class));
    }

    @Override
    protected void onDestroy() {
        session.release();
        stopService(new Intent(this, PlaybackService.class));
        // super.onDestroy() waits for android_main, which winit never lets return.
        android.os.Process.killProcess(android.os.Process.myPid());
        super.onDestroy();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            hideSystemBars();
        }
    }

    @SuppressWarnings("deprecation")
    private void hideSystemBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            getWindow().setDecorFitsSystemWindows(false);
            WindowInsetsController c = getWindow().getInsetsController();
            if (c != null) {
                c.hide(WindowInsets.Type.systemBars());
                c.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        } else {
            getWindow().getDecorView().setSystemUiVisibility(
                    View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                            | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                            | View.SYSTEM_UI_FLAG_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                            | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                            | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN);
        }
    }
}
