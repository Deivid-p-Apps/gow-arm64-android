//! gow-runtime: guest dispatcher, memory, Win32/NT layer, and graphics.

pub mod dispatch;
pub mod hostcall;
pub mod pe_load;
pub mod crt;
pub mod log;
pub mod win32;
pub mod input;
pub mod audio;
pub mod fileio;
pub mod graphics;
pub mod dxbc;
pub mod threads;
pub mod vk;
pub mod steam;
pub mod sse_extra;
pub mod x87;
#[cfg(target_os = "android")]
pub mod jni;

pub use dispatch::{GowCpu, GowFunc};
