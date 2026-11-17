package com.demarc.c64;

import android.app.NativeActivity;
import android.content.Intent;
import android.content.res.Configuration;
import android.graphics.Insets;
import android.os.Build;
import android.os.Bundle;
import android.view.RoundedCorner;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

/** NativeActivity in immersive mode: the theme alone cannot hide the navigation bar. */
public class MiniActivity extends NativeActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        if (Build.VERSION.SDK_INT >= 28) {
            getWindow().getAttributes().layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }
        hideSystemBars();
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

    /** The display cutout, grown so no corner of the rect is cut by a rounded screen corner. */
    private static Insets safeInsets(WindowInsets insets) {
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
        stopService(new Intent(this, PlaybackService.class));
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
