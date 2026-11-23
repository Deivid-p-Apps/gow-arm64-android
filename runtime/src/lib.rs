//! gow-runtime: guest dispatcher, memory, and (in progress) Win32/NT + graphics.

pub mod dispatch;
#[cfg(target_os = "android")]
pub mod jni;

pub use dispatch::{GowCpu, GowFunc};
