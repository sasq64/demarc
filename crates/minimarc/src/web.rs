//! A release's web page, shown by `WebActivity` on Android.

#[cfg(target_os = "android")]
mod android {
    use std::sync::OnceLock;

    use jni::JavaVM;
    use jni::objects::JObject;
    use winit::platform::android::activity::AndroidApp;

    static APP: OnceLock<AndroidApp> = OnceLock::new();

    pub fn set_app(app: AndroidApp) {
        let _ = APP.set(app);
    }

    /// `MiniActivity.openWeb(url, css)`; `css` names an APK asset.
    pub fn open(url: &str, css: &str) {
        let Some(app) = APP.get() else {
            return;
        };
        let result = (|| -> jni::errors::Result<()> {
            // SAFETY: both pointers stay valid for as long as the activity.
            let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
            let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
            let mut env = vm.attach_current_thread()?;
            let url = env.new_string(url)?;
            let css = env.new_string(css)?;
            let call = env.call_method(
                &activity,
                "openWeb",
                "(Ljava/lang/String;Ljava/lang/String;)V",
                &[(&url).into(), (&css).into()],
            );
            if call.is_err() {
                let _ = env.exception_clear();
            }
            call.map(|_| ())
        })();
        if let Err(e) = result {
            tracing::warn!("Can't open {url}: {e}");
        }
    }
}

#[cfg(target_os = "android")]
pub use android::{open, set_app};

#[cfg(not(target_os = "android"))]
pub fn open(_url: &str, _css: &str) {}
