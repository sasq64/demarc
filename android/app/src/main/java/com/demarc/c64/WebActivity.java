package com.demarc.c64;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.util.Base64;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.util.Objects;

/** A release's web page, restyled for the phone. Back finishes it. */
public class WebActivity extends Activity {
    static final String URL = "url";
    static final String CSS = "css";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        String url = getIntent().getStringExtra(URL);
        String host = Uri.parse(url).getHost();
        String script = styleScript(readAsset(getIntent().getStringExtra(CSS)));

        WebView web = new WebView(this);
        web.getSettings().setJavaScriptEnabled(true);
        web.setBackgroundColor(0xff000000);
        // Hidden until the css is in, so the unstyled page never shows.
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageStarted(WebView view, String url, Bitmap favicon) {
                if (Objects.equals(Uri.parse(url).getHost(), host)) {
                    view.setAlpha(0);
                }
            }

            @Override
            public void onPageCommitVisible(WebView view, String url) {
                style(view, url);
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                style(view, url);
            }

            private void style(WebView view, String url) {
                if (Objects.equals(Uri.parse(url).getHost(), host)) {
                    view.evaluateJavascript(script, r -> view.setAlpha(1));
                } else {
                    view.setAlpha(1);
                }
            }
        });

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(0xff000000);
        root.addView(web);
        if (Build.VERSION.SDK_INT >= 28) {
            getWindow().getAttributes().layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }
        if (Build.VERSION.SDK_INT >= 30) {
            getWindow().setDecorFitsSystemWindows(false);
            root.setOnApplyWindowInsetsListener((v, insets) -> {
                Insets safe = MiniActivity.safeInsets(insets);
                v.setPadding(0, safe.top, 0, 0);
                return insets;
            });
        }
        setContentView(root);
        web.loadUrl(url);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus && Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController c = getWindow().getInsetsController();
            if (c != null) {
                c.hide(WindowInsets.Type.systemBars());
                c.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        }
    }

    /** Adds a phone viewport and `css` (passed as base64, so no quoting) to the page, once. */
    private static String styleScript(String css) {
        String b64 = Base64.encodeToString(css.getBytes(), Base64.NO_WRAP);
        return "(function(){"
                + "if(document.getElementById('minimarc-css'))return;"
                + "var p=document.head||document.documentElement;"
                + "var m=document.createElement('meta');m.name='viewport';"
                + "m.content='width=device-width,initial-scale=1';p.appendChild(m);"
                + "var s=document.createElement('style');s.id='minimarc-css';"
                + "s.textContent=atob('" + b64 + "');p.appendChild(s);})()";
    }

    private String readAsset(String name) {
        if (name == null || name.isEmpty()) {
            return "";
        }
        try (InputStream in = getAssets().open(name)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            for (int n; (n = in.read(buf)) > 0; ) {
                out.write(buf, 0, n);
            }
            return out.toString("UTF-8");
        } catch (Exception e) {
            return "";
        }
    }
}
