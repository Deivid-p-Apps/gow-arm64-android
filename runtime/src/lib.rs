//! gow-runtime: dispatcher + guest memory + (in progress) Win32/NT layer.

pub mod dispatch;

pub use dispatch::{GowCpu, GowFunc};
