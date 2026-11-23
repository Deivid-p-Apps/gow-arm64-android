//! gow-runtime: guest dispatcher, memory, Win32/NT layer, and graphics.

pub mod dispatch;
pub mod win32;
pub mod steam;
#[cfg(target_os = "android")]
pub mod jni;

pub use dispatch::{GowCpu, GowFunc};
