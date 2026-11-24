//! Logging: writes `[gow]` diagnostics to Android logcat AND, when enabled, to
//! a real-time file so a crash never loses them. The file lives in the game
//! folder (`/storage/emulated/0/god/Port/`) as requested, so it survives an app
//! kill. Toggle via `set_file_logging` (driven from the setup screen).

use std::ffi::CString;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[cfg(target_os = "android")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::os::raw::c_char, text: *const std::os::raw::c_char) -> i32;
}

static FILE_LOGGING: AtomicBool = AtomicBool::new(false);
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static LOG_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Enable/disable the on-disk log. `dir` is created if needed; the file is
/// `gow-<pid>.log` inside it. Called from JNI (Java toggle / always-on).
pub fn set_file_logging(enabled: bool, dir: &str) {
    *LOG_DIR.lock().unwrap() = Some(dir.to_string());
    if enabled {
        if let Err(e) = open_log_file(dir) {
            glog_internal(&format!("[gow.log] cannot open log file in {dir}: {e}"));
            FILE_LOGGING.store(false, Ordering::SeqCst);
            return;
        }
        FILE_LOGGING.store(true, Ordering::SeqCst);
    } else {
        FILE_LOGGING.store(false, Ordering::SeqCst);
        *LOG_FILE.lock().unwrap() = None;
    }
}

fn open_log_file(dir: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = format!("{dir}/gow-{}.log", std::process::id());
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    *LOG_FILE.lock().unwrap() = Some(f);
    glog_internal(&format!("[gow.log] logging to {path}"));
    Ok(())
}

/// Write to logcat only (used before file logging is configured).
fn glog_internal(msg: &str) {
    #[cfg(target_os = "android")]
    {
        if let (Ok(tag), Ok(text)) = (CString::new("GoW"), CString::new(msg)) {
            unsafe { __android_log_write(4, tag.as_ptr(), text.as_ptr()); }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        eprintln!("{msg}");
    }
}

pub fn log_line(msg: &str) {
    glog_internal(msg);
    if FILE_LOGGING.load(Ordering::Relaxed) {
        if let Ok(mut guard) = LOG_FILE.lock() {
            if let Some(f) = guard.as_mut() {
                // Timestamp + message; flush every line so a hard crash keeps it.
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let _ = writeln!(f, "{ts} {msg}");
                let _ = f.flush();
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_gow_arm64_GowActivity_nativeSetFileLogging(
    env: *mut std::ffi::c_void,
    _thiz: *mut std::ffi::c_void,
    enabled: u8,
    dir: *mut std::ffi::c_void,
) {
    // Reuse the JNI string helper from jni.rs.
    let dir_s = unsafe { crate::jni::jstring_to_string_pub(env, dir) };
    let d = if dir_s.is_empty() { "/storage/emulated/0/god/Port".to_string() } else { dir_s };
    set_file_logging(enabled != 0, &d);
}

#[macro_export]
macro_rules! glog {
    ($($arg:tt)*) => {{
        $crate::log::log_line(&format!($($arg)*));
    }};
}
