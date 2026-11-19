//! A release's web page, shown by `WebActivity` on Android.

use std::sync::Mutex;

use database::emu_file::EmuFile;

static DB: Mutex<Option<(String, &'static [EmuFile])>> = Mutex::new(None);
static PENDING: Mutex<Option<Vec<String>>> = Mutex::new(None);

pub fn set_db(name: &str, files: &'static [EmuFile]) {
    *DB.lock().unwrap() = Some((name.to_owned(), files));
}

/// The picker path for a CSDb page of `kind` ("event" or "group") named
/// `name`, if the db has releases there.
fn path_in(kind: &str, name: &str, files: &[EmuFile]) -> Option<Vec<String>> {
    let found = match kind {
        "event" => files.iter().any(|f| f.get_party() == name),
        "group" => files
            .iter()
            .any(|f| f.get_meta("author").split(" & ").any(|a| a == name)),
        _ => false,
    };
    let root = if kind == "event" { "Parties" } else { "Authors" };
    found.then(|| vec![root.to_owned(), name.to_owned()])
}

fn path(kind: &str, name: &str) -> Option<Vec<String>> {
    let db = DB.lock().unwrap();
    let (db, files) = db.as_ref()?;
    let mut path = path_in(kind, name, files)?;
    path.insert(0, db.clone());
    Some(path)
}

/// The path the page's Open button asked for.
pub fn take() -> Option<Vec<String>> {
    PENDING.lock().unwrap().take()
}

#[cfg(target_os = "android")]
mod android {
    use std::sync::OnceLock;

    use jni::JNIEnv;
    use jni::JavaVM;
    use jni::objects::{JClass, JObject, JString};
    use jni::sys::jboolean;
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

    fn page_path(env: &mut JNIEnv, kind: &JString, name: &JString) -> Option<Vec<String>> {
        let kind: String = env.get_string(kind).ok()?.into();
        let name: String = env.get_string(name).ok()?.into();
        super::path(&kind, &name)
    }

    /// `WebActivity.nativeHasPath(kind, name)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_demarc_c64_WebActivity_nativeHasPath(
        mut env: JNIEnv,
        _class: JClass,
        kind: JString,
        name: JString,
    ) -> jboolean {
        page_path(&mut env, &kind, &name).is_some() as jboolean
    }

    /// `WebActivity.nativeOpenPath(kind, name)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_demarc_c64_WebActivity_nativeOpenPath(
        mut env: JNIEnv,
        _class: JClass,
        kind: JString,
        name: JString,
    ) {
        if let Some(path) = page_path(&mut env, &kind, &name) {
            *super::PENDING.lock().unwrap() = Some(path);
        }
    }
}

#[cfg(target_os = "android")]
pub use android::{open, set_app};

#[cfg(not(target_os = "android"))]
pub fn open(_url: &str, _css: &str) {}

#[cfg(test)]
#[path = "tests/web_tests.rs"]
mod tests;
