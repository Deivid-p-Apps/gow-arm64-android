//! Logging bridge: Rust `log`-style output to Android logcat, falling back to
//! stderr on other platforms. Keeps the `[gow]` diagnostics visible via `adb`.

use std::ffi::CString;

#[cfg(target_os = "android")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::os::raw::c_char, text: *const std::os::raw::c_char) -> i32;
}

pub fn log_line(msg: &str) {
    #[cfg(target_os = "android")]
    {
        if let (Ok(tag), Ok(text)) = (CString::new("GoW"), CString::new(msg)) {
            unsafe { __android_log_write(4 /* INFO */, tag.as_ptr(), text.as_ptr()); }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        eprintln!("{msg}");
    }
}

#[macro_export]
macro_rules! glog {
    ($($arg:tt)*) => {{
        $crate::log::log_line(&format!($($arg)*));
    }};
}
