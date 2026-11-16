//! JNI entry points for `io.github.hatak.sharedmousekeyboard.Native`.

use std::path::PathBuf;
use std::sync::Arc;

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jdouble, jint, jstring, JNI_FALSE, JNI_TRUE};
use jni::{JNIEnv, JavaVM};
use smk_core::android_host::{self, AndroidHost};
use smk_core::embed;
use smk_core::input::{InputEvent, Rect};

struct JniHost {
    vm: JavaVM,
    obj: GlobalRef,
}

impl JniHost {
    fn call<R>(&self, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
        let mut env = self.vm.attach_current_thread_permanently().ok()?;
        let result = f(&mut env, self.obj.as_obj());
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
            return None;
        }
        result.ok()
    }

    fn call_void(&self, name: &str, sig: &str, args: &[JValue]) {
        self.call(|env, obj| env.call_method(obj, name, sig, args).map(|_| ()));
    }
}

impl AndroidHost for JniHost {
    fn inject(&self, event: InputEvent) -> bool {
        let (kind, x, y, a, b) = match event {
            InputEvent::MouseMove { x, y } => (0, x, y, 0, 0),
            InputEvent::MouseButton { button, down } => (1, 0.0, 0.0, embed::button_index(button), down as jint),
            InputEvent::Scroll { dx, dy } => (2, 0.0, 0.0, dx, dy),
            InputEvent::Key { code, down } => (3, 0.0, 0.0, code as jint, down as jint),
        };
        self.call(|env, obj| {
            env.call_method(
                obj,
                "inject",
                "(IDDII)Z",
                &[JValue::Int(kind), JValue::Double(x), JValue::Double(y), JValue::Int(a), JValue::Int(b)],
            )?
            .z()
        })
        .unwrap_or(false)
    }

    fn screen(&self) -> Rect {
        let size = self.call(|env, obj| {
            let w = env.call_method(obj, "screenWidth", "()I", &[])?.i()?;
            let h = env.call_method(obj, "screenHeight", "()I", &[])?.i()?;
            Ok((w, h))
        });
        let (w, h) = size.filter(|(w, h)| *w > 0 && *h > 0).unwrap_or((1080, 1920));
        Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }
    }

    fn clipboard_text(&self) -> Option<String> {
        self.call(|env, obj| {
            let s = env.call_method(obj, "clipboardText", "()Ljava/lang/String;", &[])?.l()?;
            if s.is_null() {
                return Ok(None);
            }
            Ok(Some(env.get_string(&JString::from(s))?.into()))
        })
        .flatten()
    }

    fn set_clipboard_text(&self, text: &str) {
        self.call(|env, obj| {
            let s = env.new_string(text)?;
            env.call_method(obj, "setClipboardText", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).map(|_| ())
        });
    }

    fn permission_granted(&self, id: &str) -> bool {
        self.call(|env, obj| {
            let s = env.new_string(id)?;
            env.call_method(obj, "permissionGranted", "(Ljava/lang/String;)Z", &[JValue::Object(&s)])?.z()
        })
        .unwrap_or(false)
    }

    fn capture_screen(&self) -> bool {
        self.call(|env, obj| env.call_method(obj, "captureScreen", "()Z", &[])?.z()).unwrap_or(false)
    }

    fn open_settings(&self, id: &str) {
        self.call(|env, obj| {
            let s = env.new_string(id)?;
            env.call_method(obj, "openSettings", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).map(|_| ())
        });
    }
}

fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Registry};

    let filter = EnvFilter::new("info,smk_core=info");
    #[cfg(target_os = "android")]
    {
        if let Ok(layer) = tracing_android::layer("smk") {
            let _ = Registry::default().with(filter).with(layer).try_init();
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = Registry::default().with(filter).with(tracing_subscriber::fmt::layer()).try_init();
    }
}

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

fn start_engine(host: Arc<JniHost>, config_dir: PathBuf, download_dir: PathBuf) -> anyhow::Result<()> {
    init_logging();
    android_host::set_host(host.clone());
    let notify = host.clone();
    let on_change: smk_core::engine::ChangeCallback = Arc::new(move || notify.call_void("stateChanged", "()V", &[]));
    embed::start(config_dir, download_dir, true, on_change)
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    host: JObject,
    config_dir: JString,
    download_dir: JString,
) -> jboolean {
    let result = (|| -> anyhow::Result<()> {
        let vm = env.get_java_vm()?;
        let obj = env.new_global_ref(&host)?;
        let config_dir = PathBuf::from(jstr(&mut env, &config_dir));
        let download_dir = PathBuf::from(jstr(&mut env, &download_dir));
        start_engine(Arc::new(JniHost { vm, obj }), config_dir, download_dir)
    })();
    match result {
        Ok(()) => JNI_TRUE,
        Err(e) => {
            tracing::error!("engine failed to start: {e:#}");
            JNI_FALSE
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_state(env: JNIEnv, _class: JClass) -> jstring {
    env.new_string(embed::state_json()).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_command(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    a: JString,
    b: JString,
) -> jstring {
    let (name, a, b) = (jstr(&mut env, &name), jstr(&mut env, &a), jstr(&mut env, &b));
    env.new_string(embed::command(&name, &a, &b)).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_input(
    _env: JNIEnv,
    _class: JClass,
    kind: jint,
    a: jdouble,
    b: jdouble,
) {
    embed::input(kind, a, b);
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_shareText(mut env: JNIEnv, _class: JClass, text: JString) {
    embed::share_text(jstr(&mut env, &text));
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_clipboardChanged(_env: JNIEnv, _class: JClass) {
    android_host::notify_clipboard_changed();
}

#[no_mangle]
pub extern "system" fn Java_io_github_hatak_sharedmousekeyboard_Native_snapshot(
    mut env: JNIEnv,
    _class: JClass,
    png: JByteArray,
    error: JString,
) {
    let result = if png.is_null() {
        Err(jstr(&mut env, &error))
    } else {
        env.convert_byte_array(&png).map_err(|e| e.to_string())
    };
    embed::snapshot_ready(result);
}
