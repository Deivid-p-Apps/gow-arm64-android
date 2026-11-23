//! Input layer: Android input events -> the Win32/XInput model the game reads.
//!
//! God of War (2018) PC reads its controller through `XINPUT1_4.dll`
//! (ordinals #2 = `XInputGetState`, #3 = `XInputSetState`, confirmed from the
//! game's IAT) and its keyboard/mouse through `user32` (`GetAsyncKeyState`,
//! `GetKeyboardState`, raw input, ...).
//!
//! This module owns the *single, thread-safe* source of truth for all of that:
//! the recompiled game runs on its own host thread and calls
//! [`InputState::xinput_get_state`] / reads the keyboard/mouse state; the
//! Android UI thread (via the JNI layer) feeds events in with
//! [`InputState::on_key`], [`InputState::on_motion`], [`InputState::on_touch`],
//! the mouse methods, and the `*_for` variants.
//!
//! ## Honesty rules (see `docs/DECISIONS.md` D005/D012)
//! * A pad is **not connected** until something real reports it: a gamepad key
//!   or axis, an explicit [`InputState::set_connected`], or the touch overlay
//!   being enabled. `xinput_get_state` then returns `None`, which is the honest
//!   `ERROR_DEVICE_NOT_CONNECTED` — we never invent a connected pad.
//! * Unknown keycodes / axes / touches are counted and logged on logcat, never
//!   silently swallowed and never mapped to something they are not.
//!
//! ## Layout note
//! [`GamepadState`] is `#[repr(C)]` and byte-for-byte identical to the Win32
//! `XINPUT_STATE` (16 bytes); [`GamepadState::write_to`] is what the XInput
//! import stub uses to fill the guest's `XINPUT_STATE*`. Compile-time asserts
//! below pin the size and field offsets.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

// ---------------------------------------------------------------------------
// XInput constants (from xinput.h)
// ---------------------------------------------------------------------------

/// `XInputGetState` / `XInputSetState` return value for a disconnected slot.
pub const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
/// `ERROR_SUCCESS`.
pub const ERROR_SUCCESS: u32 = 0;

/// Highest controller index XInput exposes, plus one.
pub const MAX_PADS: usize = 4;

/// The slot all Android events are routed to by default (GoW is single-player).
pub const PRIMARY_PAD: usize = 0;

// Exact `XINPUT_GAMEPAD_*` masks.
pub const XINPUT_GAMEPAD_DPAD_UP: u16 = 0x0001;
pub const XINPUT_GAMEPAD_DPAD_DOWN: u16 = 0x0002;
pub const XINPUT_GAMEPAD_DPAD_LEFT: u16 = 0x0004;
pub const XINPUT_GAMEPAD_DPAD_RIGHT: u16 = 0x0008;
pub const XINPUT_GAMEPAD_START: u16 = 0x0010;
pub const XINPUT_GAMEPAD_BACK: u16 = 0x0020;
pub const XINPUT_GAMEPAD_LEFT_THUMB: u16 = 0x0040;
pub const XINPUT_GAMEPAD_RIGHT_THUMB: u16 = 0x0080;
pub const XINPUT_GAMEPAD_LEFT_SHOULDER: u16 = 0x0100;
pub const XINPUT_GAMEPAD_RIGHT_SHOULDER: u16 = 0x0200;
pub const XINPUT_GAMEPAD_A: u16 = 0x1000;
pub const XINPUT_GAMEPAD_B: u16 = 0x2000;
pub const XINPUT_GAMEPAD_X: u16 = 0x4000;
pub const XINPUT_GAMEPAD_Y: u16 = 0x8000;

/// Deadzone used by default for the analog sticks, in normalized units.
pub const DEFAULT_STICK_DEADZONE: f32 = 0.15;

// ---------------------------------------------------------------------------
// GamepadState — byte-for-byte XINPUT_STATE / XINPUT_GAMEPAD
// ---------------------------------------------------------------------------

/// XInput gamepad state. `#[repr(C)]`, 16 bytes, identical layout to the Win32
/// `XINPUT_STATE { DWORD dwPacketNumber; XINPUT_GAMEPAD Gamepad; }`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(non_snake_case)] // field names mirror xinput.h on purpose
pub struct GamepadState {
    /// Monotonic counter, incremented whenever the state changes.
    pub dwPacketNumber: u32,
    /// Bitfield of `XINPUT_GAMEPAD_*` masks.
    pub wButtons: u16,
    /// 0..=255.
    pub bLeftTrigger: u8,
    /// 0..=255.
    pub bRightTrigger: u8,
    /// -32768..=32767 (negative = left).
    pub sThumbLX: i16,
    /// -32768..=32767 (negative = down).
    pub sThumbLY: i16,
    /// -32768..=32767 (negative = left).
    pub sThumbRX: i16,
    /// -32768..=32767 (negative = down).
    pub sThumbRY: i16,
}

impl GamepadState {
    pub const fn neutral() -> Self {
        Self {
            dwPacketNumber: 0,
            wButtons: 0,
            bLeftTrigger: 0,
            bRightTrigger: 0,
            sThumbLX: 0,
            sThumbLY: 0,
            sThumbRX: 0,
            sThumbRY: 0,
        }
    }

    /// True if any bit of `mask` is set in `wButtons`.
    pub fn is_pressed(&self, mask: u16) -> bool {
        self.wButtons & mask != 0
    }

    /// Serialize to the 16 little-endian bytes of `XINPUT_STATE`.
    pub fn to_le_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.dwPacketNumber.to_le_bytes());
        b[4..6].copy_from_slice(&self.wButtons.to_le_bytes());
        b[6] = self.bLeftTrigger;
        b[7] = self.bRightTrigger;
        b[8..10].copy_from_slice(&self.sThumbLX.to_le_bytes());
        b[10..12].copy_from_slice(&self.sThumbLY.to_le_bytes());
        b[12..14].copy_from_slice(&self.sThumbRX.to_le_bytes());
        b[14..16].copy_from_slice(&self.sThumbRY.to_le_bytes());
        b
    }

    /// Parse the 16 little-endian bytes of an `XINPUT_STATE`.
    pub fn from_le_bytes(b: &[u8; 16]) -> Self {
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let i16_at = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]);
        Self {
            dwPacketNumber: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            wButtons: u16_at(4),
            bLeftTrigger: b[6],
            bRightTrigger: b[7],
            sThumbLX: i16_at(8),
            sThumbLY: i16_at(10),
            sThumbRX: i16_at(12),
            sThumbRY: i16_at(14),
        }
    }

    /// Write this state as an `XINPUT_STATE` to guest memory (`dst` must point
    /// at at least 16 writable bytes).
    ///
    /// # Safety
    /// `dst` must be valid for 16 bytes of writes.
    pub unsafe fn write_to(&self, dst: *mut u8) {
        let bytes = self.to_le_bytes();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
    }
}

// Layout proof: 16 bytes, fields exactly where xinput.h puts them.
const _: () = assert!(std::mem::size_of::<GamepadState>() == 16);
const _: () = assert!(std::mem::offset_of!(GamepadState, dwPacketNumber) == 0);
const _: () = assert!(std::mem::offset_of!(GamepadState, wButtons) == 4);
const _: () = assert!(std::mem::offset_of!(GamepadState, bLeftTrigger) == 6);
const _: () = assert!(std::mem::offset_of!(GamepadState, bRightTrigger) == 7);
const _: () = assert!(std::mem::offset_of!(GamepadState, sThumbLX) == 8);
const _: () = assert!(std::mem::offset_of!(GamepadState, sThumbLY) == 10);
const _: () = assert!(std::mem::offset_of!(GamepadState, sThumbRX) == 12);
const _: () = assert!(std::mem::offset_of!(GamepadState, sThumbRY) == 14);

// ---------------------------------------------------------------------------
// Android constants we consume (android.view.KeyEvent / MotionEvent / InputDevice)
// ---------------------------------------------------------------------------

/// Android `KeyEvent`/`MotionEvent`/`InputDevice` numeric constants.
pub mod android {
    // KeyEvent keycodes.
    pub const KEYCODE_DPAD_UP: i32 = 19;
    pub const KEYCODE_DPAD_DOWN: i32 = 20;
    pub const KEYCODE_DPAD_LEFT: i32 = 21;
    pub const KEYCODE_DPAD_RIGHT: i32 = 22;
    pub const KEYCODE_DPAD_CENTER: i32 = 23;
    pub const KEYCODE_A: i32 = 29;
    pub const KEYCODE_B: i32 = 30;
    pub const KEYCODE_C: i32 = 31;
    pub const KEYCODE_D: i32 = 32;
    pub const KEYCODE_E: i32 = 33;
    pub const KEYCODE_F: i32 = 34;
    pub const KEYCODE_G: i32 = 35;
    pub const KEYCODE_H: i32 = 36;
    pub const KEYCODE_I: i32 = 37;
    pub const KEYCODE_J: i32 = 38;
    pub const KEYCODE_K: i32 = 39;
    pub const KEYCODE_L: i32 = 40;
    pub const KEYCODE_M: i32 = 41;
    pub const KEYCODE_N: i32 = 42;
    pub const KEYCODE_O: i32 = 43;
    pub const KEYCODE_P: i32 = 44;
    pub const KEYCODE_Q: i32 = 45;
    pub const KEYCODE_R: i32 = 46;
    pub const KEYCODE_S: i32 = 47;
    pub const KEYCODE_T: i32 = 48;
    pub const KEYCODE_U: i32 = 49;
    pub const KEYCODE_V: i32 = 50;
    pub const KEYCODE_W: i32 = 51;
    pub const KEYCODE_X: i32 = 52;
    pub const KEYCODE_Y: i32 = 53;
    pub const KEYCODE_Z: i32 = 54;
    pub const KEYCODE_0: i32 = 7;
    pub const KEYCODE_9: i32 = 16;
    pub const KEYCODE_COMMA: i32 = 55;
    pub const KEYCODE_PERIOD: i32 = 56;
    pub const KEYCODE_TAB: i32 = 61;
    pub const KEYCODE_SPACE: i32 = 62;
    pub const KEYCODE_ENTER: i32 = 66;
    pub const KEYCODE_DEL: i32 = 67;
    pub const KEYCODE_GRAVE: i32 = 68;
    pub const KEYCODE_MINUS: i32 = 69;
    pub const KEYCODE_EQUALS: i32 = 70;
    pub const KEYCODE_LEFT_BRACKET: i32 = 71;
    pub const KEYCODE_RIGHT_BRACKET: i32 = 72;
    pub const KEYCODE_BACKSLASH: i32 = 73;
    pub const KEYCODE_SEMICOLON: i32 = 74;
    pub const KEYCODE_APOSTROPHE: i32 = 75;
    pub const KEYCODE_SLASH: i32 = 76;
    pub const KEYCODE_PAGE_UP: i32 = 92;
    pub const KEYCODE_PAGE_DOWN: i32 = 93;
    pub const KEYCODE_ESCAPE: i32 = 111;
    pub const KEYCODE_FORWARD_DEL: i32 = 112;
    pub const KEYCODE_CTRL_LEFT: i32 = 113;
    pub const KEYCODE_CTRL_RIGHT: i32 = 114;
    pub const KEYCODE_ALT_LEFT: i32 = 57;
    pub const KEYCODE_ALT_RIGHT: i32 = 58;
    pub const KEYCODE_SHIFT_LEFT: i32 = 59;
    pub const KEYCODE_SHIFT_RIGHT: i32 = 60;
    pub const KEYCODE_MOVE_HOME: i32 = 122;
    pub const KEYCODE_MOVE_END: i32 = 123;
    pub const KEYCODE_F1: i32 = 131;
    pub const KEYCODE_F12: i32 = 142;
    pub const KEYCODE_SYSRQ: i32 = 120;

    // Gamepad-only keycodes (android.view.KeyEvent).
    pub const KEYCODE_BUTTON_A: i32 = 96;
    pub const KEYCODE_BUTTON_B: i32 = 97;
    pub const KEYCODE_BUTTON_C: i32 = 98;
    pub const KEYCODE_BUTTON_X: i32 = 99;
    pub const KEYCODE_BUTTON_Y: i32 = 100;
    pub const KEYCODE_BUTTON_Z: i32 = 101;
    pub const KEYCODE_BUTTON_L1: i32 = 102;
    pub const KEYCODE_BUTTON_R1: i32 = 103;
    pub const KEYCODE_BUTTON_L2: i32 = 104;
    pub const KEYCODE_BUTTON_R2: i32 = 105;
    pub const KEYCODE_BUTTON_THUMBL: i32 = 106;
    pub const KEYCODE_BUTTON_THUMBR: i32 = 107;
    pub const KEYCODE_BUTTON_START: i32 = 108;
    pub const KEYCODE_BUTTON_SELECT: i32 = 109;
    pub const KEYCODE_BUTTON_MODE: i32 = 110;

    // MotionEvent axis ids.
    pub const AXIS_X: i32 = 0;
    pub const AXIS_Y: i32 = 1;
    pub const AXIS_Z: i32 = 11;
    pub const AXIS_RX: i32 = 12;
    pub const AXIS_RY: i32 = 13;
    pub const AXIS_RZ: i32 = 14;
    pub const AXIS_HAT_X: i32 = 15;
    pub const AXIS_HAT_Y: i32 = 16;
    pub const AXIS_LTRIGGER: i32 = 17;
    pub const AXIS_RTRIGGER: i32 = 18;
    pub const AXIS_THROTTLE: i32 = 19;
    pub const AXIS_BRAKE: i32 = 23;
    pub const AXIS_GAS: i32 = 22;
    pub const AXIS_VSCROLL: i32 = 9;
    pub const AXIS_HSCROLL: i32 = 10;

    // InputDevice source flags. Note all of these set SOURCE_CLASS_BUTTON
    // (0x1), so a raw `&` on the full values is ambiguous; use the dedicated
    // `SOURCE_*_FLAG` bits below to tell a controller apart from a keyboard.
    pub const SOURCE_UNKNOWN: i32 = 0x0000_0000;
    pub const SOURCE_KEYBOARD: i32 = 0x0000_0101;
    pub const SOURCE_DPAD: i32 = 0x0000_0201;
    pub const SOURCE_GAMEPAD: i32 = 0x0000_0401;
    pub const SOURCE_JOYSTICK: i32 = 0x0100_0010;
    pub const SOURCE_MOUSE: i32 = 0x0000_2002;
    pub const SOURCE_TOUCHSCREEN: i32 = 0x0000_1002;

    /// Distinguishing bits (SOURCE_DPAD has 0x200, SOURCE_GAMEPAD 0x400, the
    /// USB joystick/orientation bit is 0x0100_0000).
    pub const SOURCE_DPAD_FLAG: i32 = 0x0000_0200;
    pub const SOURCE_GAMEPAD_FLAG: i32 = 0x0000_0400;
    pub const SOURCE_JOYSTICK_FLAG: i32 = 0x0100_0000;

    // MotionEvent button ids (for SOURCE_MOUSE).
    pub const BUTTON_PRIMARY: i32 = 1;
    pub const BUTTON_SECONDARY: i32 = 2;
    pub const BUTTON_TERTIARY: i32 = 4;
    pub const BUTTON_BACK: i32 = 8;
    pub const BUTTON_FORWARD: i32 = 16;
}

/// Windows virtual-key codes (`user32` `GetAsyncKeyState` etc.).
pub mod vk {
    pub const VK_LBUTTON: u16 = 0x01;
    pub const VK_RBUTTON: u16 = 0x02;
    pub const VK_MBUTTON: u16 = 0x04;
    pub const VK_XBUTTON1: u16 = 0x05;
    pub const VK_XBUTTON2: u16 = 0x06;
    pub const VK_BACK: u16 = 0x08;
    pub const VK_TAB: u16 = 0x09;
    pub const VK_RETURN: u16 = 0x0D;
    pub const VK_SHIFT: u16 = 0x10;
    pub const VK_CONTROL: u16 = 0x11;
    pub const VK_MENU: u16 = 0x12;
    pub const VK_CAPITAL: u16 = 0x14;
    pub const VK_ESCAPE: u16 = 0x1B;
    pub const VK_SPACE: u16 = 0x20;
    pub const VK_PRIOR: u16 = 0x21;
    pub const VK_NEXT: u16 = 0x22;
    pub const VK_END: u16 = 0x23;
    pub const VK_HOME: u16 = 0x24;
    pub const VK_LEFT: u16 = 0x25;
    pub const VK_UP: u16 = 0x26;
    pub const VK_RIGHT: u16 = 0x27;
    pub const VK_DOWN: u16 = 0x28;
    pub const VK_INSERT: u16 = 0x2D;
    pub const VK_DELETE: u16 = 0x2E;
    pub const VK_LSHIFT: u16 = 0xA0;
    pub const VK_RSHIFT: u16 = 0xA1;
    pub const VK_LCONTROL: u16 = 0xA2;
    pub const VK_RCONTROL: u16 = 0xA3;
    pub const VK_LMENU: u16 = 0xA4;
    pub const VK_RMENU: u16 = 0xA5;
    pub const VK_F1: u16 = 0x70;
}

/// Map an Android gamepad keycode to an XInput `wButtons` mask.
///
/// `START`/`SELECT` are Android's `KEYCODE_BUTTON_START`/`BUTTON_SELECT`;
/// the D-pad keycodes are included because many controllers deliver the hat as
/// discrete key events rather than `AXIS_HAT_*`.
pub fn android_keycode_to_button(code: i32) -> Option<u16> {
    use android as a;
    Some(match code {
        a::KEYCODE_BUTTON_A => XINPUT_GAMEPAD_A,
        a::KEYCODE_BUTTON_B => XINPUT_GAMEPAD_B,
        a::KEYCODE_BUTTON_X => XINPUT_GAMEPAD_X,
        a::KEYCODE_BUTTON_Y => XINPUT_GAMEPAD_Y,
        a::KEYCODE_BUTTON_L1 => XINPUT_GAMEPAD_LEFT_SHOULDER,
        a::KEYCODE_BUTTON_R1 => XINPUT_GAMEPAD_RIGHT_SHOULDER,
        a::KEYCODE_BUTTON_L2 => XINPUT_GAMEPAD_LEFT_SHOULDER, // digital fallback for LT
        a::KEYCODE_BUTTON_R2 => XINPUT_GAMEPAD_RIGHT_SHOULDER, // digital fallback for RT
        a::KEYCODE_BUTTON_THUMBL => XINPUT_GAMEPAD_LEFT_THUMB,
        a::KEYCODE_BUTTON_THUMBR => XINPUT_GAMEPAD_RIGHT_THUMB,
        a::KEYCODE_BUTTON_START => XINPUT_GAMEPAD_START,
        a::KEYCODE_BUTTON_SELECT => XINPUT_GAMEPAD_BACK,
        a::KEYCODE_DPAD_UP => XINPUT_GAMEPAD_DPAD_UP,
        a::KEYCODE_DPAD_DOWN => XINPUT_GAMEPAD_DPAD_DOWN,
        a::KEYCODE_DPAD_LEFT => XINPUT_GAMEPAD_DPAD_LEFT,
        a::KEYCODE_DPAD_RIGHT => XINPUT_GAMEPAD_DPAD_RIGHT,
        _ => return None,
    })
}

/// True for keycodes that only exist on a gamepad (so a key event with an
/// unknown source can still be attributed to a controller honestly).
pub fn android_keycode_is_dedicated_gamepad(code: i32) -> bool {
    use android as a;
    matches!(
        code,
        a::KEYCODE_BUTTON_A
            | a::KEYCODE_BUTTON_B
            | a::KEYCODE_BUTTON_C
            | a::KEYCODE_BUTTON_X
            | a::KEYCODE_BUTTON_Y
            | a::KEYCODE_BUTTON_Z
            | a::KEYCODE_BUTTON_L1
            | a::KEYCODE_BUTTON_R1
            | a::KEYCODE_BUTTON_L2
            | a::KEYCODE_BUTTON_R2
            | a::KEYCODE_BUTTON_THUMBL
            | a::KEYCODE_BUTTON_THUMBR
            | a::KEYCODE_BUTTON_START
            | a::KEYCODE_BUTTON_SELECT
            | a::KEYCODE_BUTTON_MODE
    )
}

/// Map an Android keycode to a Windows virtual-key code (for `user32`).
pub fn android_keycode_to_vk(code: i32) -> Option<u16> {
    use android as a;
    // Letters A-Z.
    if (a::KEYCODE_A..=a::KEYCODE_Z).contains(&code) {
        return Some(0x41 + (code - a::KEYCODE_A) as u16);
    }
    // Digits 0-9.
    if (a::KEYCODE_0..=a::KEYCODE_9).contains(&code) {
        return Some(0x30 + (code - a::KEYCODE_0) as u16);
    }
    // Function keys F1-F12.
    if (a::KEYCODE_F1..=a::KEYCODE_F12).contains(&code) {
        return Some(vk::VK_F1 + (code - a::KEYCODE_F1) as u16);
    }
    Some(match code {
        a::KEYCODE_DPAD_UP => vk::VK_UP,
        a::KEYCODE_DPAD_DOWN => vk::VK_DOWN,
        a::KEYCODE_DPAD_LEFT => vk::VK_LEFT,
        a::KEYCODE_DPAD_RIGHT => vk::VK_RIGHT,
        a::KEYCODE_DPAD_CENTER => vk::VK_RETURN,
        a::KEYCODE_TAB => vk::VK_TAB,
        a::KEYCODE_SPACE => vk::VK_SPACE,
        a::KEYCODE_ENTER => vk::VK_RETURN,
        a::KEYCODE_ESCAPE => vk::VK_ESCAPE,
        a::KEYCODE_DEL => vk::VK_BACK,
        a::KEYCODE_FORWARD_DEL => vk::VK_DELETE,
        a::KEYCODE_MOVE_HOME => vk::VK_HOME,
        a::KEYCODE_MOVE_END => vk::VK_END,
        a::KEYCODE_PAGE_UP => vk::VK_PRIOR,
        a::KEYCODE_PAGE_DOWN => vk::VK_NEXT,
        a::KEYCODE_SHIFT_LEFT => vk::VK_LSHIFT,
        a::KEYCODE_SHIFT_RIGHT => vk::VK_RSHIFT,
        a::KEYCODE_CTRL_LEFT => vk::VK_LCONTROL,
        a::KEYCODE_CTRL_RIGHT => vk::VK_RCONTROL,
        a::KEYCODE_ALT_LEFT => vk::VK_LMENU,
        a::KEYCODE_ALT_RIGHT => vk::VK_RMENU,
        a::KEYCODE_MINUS => 0xBD,
        a::KEYCODE_EQUALS => 0xBB,
        a::KEYCODE_LEFT_BRACKET => 0xDB,
        a::KEYCODE_RIGHT_BRACKET => 0xDD,
        a::KEYCODE_BACKSLASH => 0xDC,
        a::KEYCODE_SEMICOLON => 0xBA,
        a::KEYCODE_APOSTROPHE => 0xDE,
        a::KEYCODE_GRAVE => 0xC0,
        a::KEYCODE_COMMA => 0xBC,
        a::KEYCODE_PERIOD => 0xBE,
        a::KEYCODE_SLASH => 0xBF,
        a::KEYCODE_SYSRQ => 0x2C, // VK_SNAPSHOT
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Keyboard / mouse state
// ---------------------------------------------------------------------------

/// Keyboard state as a set of Windows virtual-key codes (0..=511).
#[derive(Clone, Default)]
pub struct KeyboardState {
    bits: [u64; 8],
}

impl KeyboardState {
    pub fn set(&mut self, vk: u16, down: bool) {
        let vk = vk as usize;
        if vk >= 512 {
            return;
        }
        let word = vk / 64;
        let bit = 1u64 << (vk % 64);
        if down {
            self.bits[word] |= bit;
        } else {
            self.bits[word] &= !bit;
        }
    }

    pub fn get(&self, vk: u16) -> bool {
        let vk = vk as usize;
        if vk >= 512 {
            return false;
        }
        self.bits[vk / 64] & (1u64 << (vk % 64)) != 0
    }

    pub fn clear(&mut self) {
        self.bits = [0; 8];
    }
}

/// Mouse state: absolute position, buttons, wheel and relative motion since the
/// last [`MouseState::take_relative`] (what a raw-input/high-precision mouse
/// look needs).
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    /// Bitmask of Android `BUTTON_*` ids.
    pub buttons: i32,
    /// Accumulated vertical wheel clicks.
    pub wheel: i32,
    /// Accumulated relative X/Y since the last `take_relative`.
    pub dx: i32,
    pub dy: i32,
}

impl MouseState {
    /// Returns and clears the accumulated relative motion.
    pub fn take_relative(&mut self) -> (i32, i32) {
        let r = (self.dx, self.dy);
        self.dx = 0;
        self.dy = 0;
        r
    }
}

// ---------------------------------------------------------------------------
// Touch overlay layout
// ---------------------------------------------------------------------------

/// What a touch region drives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TouchTarget {
    LeftStick,
    RightStick,
    /// D-pad; the direction is derived from where inside the region you press.
    DPad,
    /// A digital button, given as an `XINPUT_GAMEPAD_*` mask.
    Button(u16),
    /// Analog left trigger, driven digitally (0 or 255).
    LeftTrigger,
    /// Analog right trigger, driven digitally (0 or 255).
    RightTrigger,
}

/// An elliptical touch region in normalized `[0,1]` coordinates
/// (`x = px / width`, `y = py / height`).
#[derive(Clone, Copy, Debug)]
pub struct TouchRegion {
    pub cx: f32,
    pub cy: f32,
    pub rx: f32,
    pub ry: f32,
    pub target: TouchTarget,
}

impl TouchRegion {
    pub const fn new(cx: f32, cy: f32, rx: f32, ry: f32, target: TouchTarget) -> Self {
        Self { cx, cy, rx, ry, target }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        let dx = (x - self.cx) / self.rx.max(1e-6);
        let dy = (y - self.cy) / self.ry.max(1e-6);
        dx * dx + dy * dy <= 1.0
    }

    /// Signed, clamped stick vector; `y` is inverted so up is positive.
    pub fn stick_vector(&self, x: f32, y: f32) -> (f32, f32) {
        let nx = ((x - self.cx) / self.rx.max(1e-6)).clamp(-1.0, 1.0);
        let ny = (-(y - self.cy) / self.ry.max(1e-6)).clamp(-1.0, 1.0);
        let m = (nx * nx + ny * ny).sqrt();
        if m > 1.0 {
            (nx / m, ny / m)
        } else {
            (nx, ny)
        }
    }
}

/// The full touch overlay: an ordered list of regions (first hit wins).
#[derive(Clone, Debug)]
pub struct TouchLayout {
    pub regions: Vec<TouchRegion>,
}

impl TouchLayout {
    /// A sane default for a landscape phone. These are starting values; tune
    /// on-device (or ship a different layout) via [`InputState::set_touch_layout`].
    pub fn default_game() -> Self {
        use TouchTarget::*;
        // Order matters: the sticks are the largest regions, so they come last
        // and every discrete button/trigger/D-pad inside them wins the hit test.
        let regions = vec![
            // -- left hand: discrete controls first --
            TouchRegion::new(0.06, 0.09, 0.05, 0.05, LeftTrigger), // LT
            TouchRegion::new(0.19, 0.10, 0.045, 0.045, Button(XINPUT_GAMEPAD_LEFT_SHOULDER)), // LB
            TouchRegion::new(0.055, 0.32, 0.065, 0.095, DPad),
            TouchRegion::new(0.15, 0.70, 0.14, 0.20, LeftStick),
            // -- right hand: discrete controls first --
            TouchRegion::new(0.94, 0.09, 0.05, 0.05, RightTrigger), // RT
            TouchRegion::new(0.81, 0.10, 0.045, 0.045, Button(XINPUT_GAMEPAD_RIGHT_SHOULDER)), // RB
            TouchRegion::new(0.84, 0.72, 0.042, 0.042, Button(XINPUT_GAMEPAD_Y)),
            TouchRegion::new(0.76, 0.80, 0.042, 0.042, Button(XINPUT_GAMEPAD_X)),
            TouchRegion::new(0.92, 0.80, 0.042, 0.042, Button(XINPUT_GAMEPAD_B)),
            TouchRegion::new(0.84, 0.88, 0.042, 0.042, Button(XINPUT_GAMEPAD_A)),
            TouchRegion::new(0.74, 0.70, 0.11, 0.17, RightStick),
            // -- center --
            TouchRegion::new(0.44, 0.30, 0.045, 0.045, Button(XINPUT_GAMEPAD_BACK)),
            TouchRegion::new(0.56, 0.30, 0.045, 0.045, Button(XINPUT_GAMEPAD_START)),
        ];
        Self { regions }
    }

    /// First region containing `(x, y)`.
    pub fn hit(&self, x: f32, y: f32) -> Option<(TouchTarget, TouchRegion)> {
        self.regions
            .iter()
            .find(|r| r.contains(x, y))
            .map(|r| (r.target, *r))
    }
}

impl Default for TouchLayout {
    fn default() -> Self {
        Self::default_game()
    }
}

// ---------------------------------------------------------------------------
// Internal per-pad state
// ---------------------------------------------------------------------------

/// One controller slot. Physical (gamepad key/axis) and touch-overlay
/// contributions are kept separate and merged on read, so releasing a virtual
/// button never stomps a real one and vice versa.
#[derive(Clone, Default)]
struct PadState {
    physical_connected: bool,
    touch_enabled: bool,
    packet: u32,

    // Physical contributions.
    p_buttons: u16,
    p_lt: u8,
    p_rt: u8,
    p_lx: i16,
    p_ly: i16,
    p_rx: i16,
    p_ry: i16,

    // Touch-overlay contributions.
    t_buttons: u16,
    t_lt: u8,
    t_rt: u8,
    t_lx: i16,
    t_ly: i16,
    t_rx: i16,
    t_ry: i16,
    lstick_pointer: Option<i32>,
    rstick_pointer: Option<i32>,

    rumble_left: u16,
    rumble_right: u16,
}

impl PadState {
    fn connected(&self) -> bool {
        self.physical_connected || self.touch_enabled
    }

    fn bump(&mut self) {
        self.packet = self.packet.wrapping_add(1);
    }

    /// Mark the physical pad connected. Returns true if this was a new connect.
    fn connect_physical(&mut self) -> bool {
        if self.physical_connected {
            return false;
        }
        self.physical_connected = true;
        self.bump();
        true
    }

    /// The state the game sees, or `None` when nothing is connected.
    fn snapshot(&self) -> Option<GamepadState> {
        if !self.connected() {
            return None;
        }
        let left_stick_from_touch = self.lstick_pointer.is_some();
        let right_stick_from_touch = self.rstick_pointer.is_some();
        let (lx, ly) = if left_stick_from_touch { (self.t_lx, self.t_ly) } else { (self.p_lx, self.p_ly) };
        let (rx, ry) = if right_stick_from_touch { (self.t_rx, self.t_ry) } else { (self.p_rx, self.p_ry) };
        Some(GamepadState {
            dwPacketNumber: self.packet,
            wButtons: self.p_buttons | self.t_buttons,
            bLeftTrigger: self.p_lt.max(self.t_lt),
            bRightTrigger: self.p_rt.max(self.t_rt),
            sThumbLX: lx,
            sThumbLY: ly,
            sThumbRX: rx,
            sThumbRY: ry,
        })
    }
}

/// Rate-limited diagnostic counters (one set of slots, shared).
#[derive(Default)]
struct LogCounters {
    unknown_keys: u64,
    unknown_axes: u64,
    touch_outside: u64,
    rumble: u64,
}

fn count_log(counter: &mut u64, msg: &str) {
    *counter += 1;
    let n = *counter;
    if n <= 16 || n.is_multiple_of(512) {
        crate::glog!("{msg} (x{n})");
    }
}

/// Everything the input layer owns.
struct InputData {
    pads: [PadState; MAX_PADS],
    keyboard: KeyboardState,
    mouse: MouseState,
    touch: TouchLayout,
    /// pointer id -> (target, region) currently held.
    touch_pointers: HashMap<i32, (TouchTarget, TouchRegion)>,
    deadzone: f32,
    logged: LogCounters,
}

// ---------------------------------------------------------------------------
// InputState — the public, shareable handle
// ---------------------------------------------------------------------------

/// Thread-safe input state. Clone it freely (it is an `Arc` internally) or use
/// the process-wide [`global`] instance from the JNI and import layers.
#[derive(Clone)]
pub struct InputState {
    inner: Arc<Mutex<InputData>>,
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(InputData {
                pads: std::array::from_fn(|_| PadState::default()),
                keyboard: KeyboardState::default(),
                mouse: MouseState::default(),
                touch: TouchLayout::default_game(),
                touch_pointers: HashMap::new(),
                deadzone: DEFAULT_STICK_DEADZONE,
                logged: LogCounters::default(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, InputData> {
        // A poisoned lock must not take the whole game down; recover the data.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- configuration ------------------------------------------------------

    /// Set the analog-stick deadzone (normalized units, default
    /// [`DEFAULT_STICK_DEADZONE`]).
    pub fn set_deadzone(&self, deadzone: f32) {
        self.lock().deadzone = deadzone.clamp(0.0, 0.99);
    }

    pub fn deadzone(&self) -> f32 {
        self.lock().deadzone
    }

    /// Replace the touch-overlay layout.
    pub fn set_touch_layout(&self, layout: TouchLayout) {
        self.lock().touch = layout;
    }

    /// Explicitly mark a controller slot connected/disconnected. Use this when
    /// the Java layer knows a real device is attached (e.g. per Android
    /// `InputDevice` id). A disconnected slot reports `None`.
    pub fn set_connected(&self, index: u32, connected: bool) {
        let mut d = self.lock();
        if index as usize >= MAX_PADS {
            count_log(&mut d.logged.unknown_axes, &format!("[gow.input] set_connected bad index {index}"));
            return;
        }
        let pad = &mut d.pads[index as usize];
        if pad.physical_connected != connected {
            pad.physical_connected = connected;
            pad.bump();
            crate::glog!("[gow.input] pad {index} {}", if connected { "connected" } else { "disconnected" });
        }
    }

    pub fn is_connected(&self, index: u32) -> bool {
        (index as usize) < MAX_PADS && self.lock().pads[index as usize].connected()
    }

    /// Press/release one physical button mask on a slot. The first button
    /// press implies the controller is connected.
    fn set_pad_button(&self, index: u32, mask: u16, down: bool) {
        let mut d = self.lock();
        let pad = &mut d.pads[index as usize];
        if down {
            pad.p_buttons |= mask;
        } else {
            pad.p_buttons &= !mask;
        }
        if pad.connect_physical() {
            crate::glog!("[gow.input] gamepad {index} connected (button 0x{mask:04x})");
        }
        pad.bump();
    }

    // -- XInput surface (what the game reads) -------------------------------

    /// `XInputGetState`: `Some(state)` for a connected slot, `None` otherwise
    /// (the caller turns `None` into `ERROR_DEVICE_NOT_CONNECTED`).
    pub fn xinput_get_state(&self, index: u32) -> Option<GamepadState> {
        if index as usize >= MAX_PADS {
            return None;
        }
        self.lock().pads[index as usize].snapshot()
    }

    /// `XInputSetState`: store rumble on a connected slot and return
    /// `ERROR_SUCCESS`; return `ERROR_DEVICE_NOT_CONNECTED` otherwise (honest:
    /// we refuse to pretend we buzzed a controller that isn't there).
    ///
    /// The Android `Vibrator` bridge can poll [`InputState::rumble`].
    pub fn xinput_set_state(&self, index: u32, left_motor: u16, right_motor: u16) -> u32 {
        if index as usize >= MAX_PADS {
            return ERROR_DEVICE_NOT_CONNECTED;
        }
        let mut d = self.lock();
        let pad = &mut d.pads[index as usize];
        if !pad.connected() {
            return ERROR_DEVICE_NOT_CONNECTED;
        }
        pad.rumble_left = left_motor;
        pad.rumble_right = right_motor;
        if left_motor != 0 || right_motor != 0 {
            count_log(
                &mut d.logged.rumble,
                &format!("[gow.input] rumble pad {index} L={left_motor} R={right_motor}"),
            );
        }
        ERROR_SUCCESS
    }

    /// Current rumble values for a slot (for the Android Vibrator bridge).
    pub fn rumble(&self, index: u32) -> Option<(u16, u16)> {
        if index as usize >= MAX_PADS {
            return None;
        }
        let d = self.lock();
        let pad = &d.pads[index as usize];
        if pad.connected() {
            Some((pad.rumble_left, pad.rumble_right))
        } else {
            None
        }
    }

    // -- keyboard / mouse (what `user32` will read) -------------------------

    /// True if the Windows virtual key `vk` is currently down.
    pub fn vk_down(&self, vk: u16) -> bool {
        self.lock().keyboard.get(vk)
    }

    /// True if the key identified by an Android keycode is currently down.
    pub fn android_key_down(&self, android_keycode: i32) -> bool {
        match android_keycode_to_vk(android_keycode) {
            Some(vk) => self.vk_down(vk),
            None => false,
        }
    }

    pub fn mouse_state(&self) -> MouseState {
        self.lock().mouse
    }

    /// Take (and clear) the accumulated relative mouse motion.
    pub fn take_mouse_relative(&self) -> (i32, i32) {
        self.lock().mouse.take_relative()
    }

    // -- Android event entry points -----------------------------------------

    /// Key event with an explicit Android `InputDevice` source
    /// (`KeyEvent.getSource()`). This is the preferred entry point: it lets a
    /// D-pad keycode be attributed to the controller or the keyboard correctly.
    pub fn on_key_event(&self, source: i32, android_keycode: i32, down: bool) {
        self.on_key_event_for(PRIMARY_PAD as u32, source, android_keycode, down);
    }

    /// Key event for a specific controller slot.
    pub fn on_key_event_for(&self, pad: u32, source: i32, android_keycode: i32, down: bool) {
        use android as a;
        if pad as usize >= MAX_PADS {
            let mut d = self.lock();
            count_log(&mut d.logged.unknown_keys, &format!("[gow.input] key for bad pad {pad}"));
            return;
        }
        let gamepad_source =
            source & (a::SOURCE_GAMEPAD_FLAG | a::SOURCE_JOYSTICK_FLAG | a::SOURCE_DPAD_FLAG) != 0;

        if gamepad_source {
            if let Some(mask) = android_keycode_to_button(android_keycode) {
                self.set_pad_button(pad, mask, down);
            } else {
                let mut d = self.lock();
                count_log(
                    &mut d.logged.unknown_keys,
                    &format!("[gow.input] unmapped gamepad keycode {android_keycode} src=0x{source:x}"),
                );
            }
            return;
        }

        // Keyboard: map to a Windows virtual key and record it.
        match android_keycode_to_vk(android_keycode) {
            Some(vk) => {
                let mut d = self.lock();
                d.keyboard.set(vk, down);
            }
            None => {
                let mut d = self.lock();
                count_log(
                    &mut d.logged.unknown_keys,
                    &format!("[gow.input] unmapped keycode {android_keycode} src=0x{source:x}"),
                );
            }
        }
    }

    /// Simple key entry point (no source). A keycode that only exists on a
    /// gamepad is attributed to the controller; everything else to the
    /// keyboard. Prefer [`InputState::on_key_event`] when the source is known.
    pub fn on_key(&self, android_keycode: i32, down: bool) {
        if android_keycode_is_dedicated_gamepad(android_keycode) {
            self.on_key_event_for(PRIMARY_PAD as u32, android::SOURCE_GAMEPAD, android_keycode, down);
        } else {
            self.on_key_event(android::SOURCE_KEYBOARD, android_keycode, down);
        }
    }

    /// Motion axis event (gamepad/joystick). `value` is the raw Android axis
    /// value: sticks and hats are `[-1,1]`, triggers should be normalized to
    /// `[0,1]` by the caller (negatives are clamped to 0).
    pub fn on_motion(&self, axis: i32, value: f32) {
        self.on_motion_for(PRIMARY_PAD as u32, axis, value);
    }

    /// Motion axis event for a specific controller slot.
    pub fn on_motion_for(&self, index: u32, axis: i32, value: f32) {
        use android as a;
        if index as usize >= MAX_PADS {
            let mut d = self.lock();
            count_log(&mut d.logged.unknown_axes, &format!("[gow.input] motion for bad pad {index}"));
            return;
        }
        let mut d = self.lock();
        let InputData { pads, deadzone, logged, .. } = &mut *d;
        let dz = *deadzone;
        let pad = &mut pads[index as usize];

        match axis {
            a::AXIS_X => pad.p_lx = stick_to_i16(value, dz),
            a::AXIS_Y => pad.p_ly = stick_to_i16(value, dz),
            // Xbox pads on Android report the right stick as Z/RZ; RX/RY are
            // accepted as aliases because some controllers use those.
            a::AXIS_Z | a::AXIS_RX => pad.p_rx = stick_to_i16(value, dz),
            a::AXIS_RZ | a::AXIS_RY => pad.p_ry = stick_to_i16(value, dz),
            a::AXIS_LTRIGGER | a::AXIS_GAS => pad.p_lt = trigger_to_u8(value),
            a::AXIS_RTRIGGER | a::AXIS_BRAKE => pad.p_rt = trigger_to_u8(value),
            a::AXIS_HAT_X => {
                if value > 0.5 {
                    pad.p_buttons |= XINPUT_GAMEPAD_DPAD_RIGHT;
                    pad.p_buttons &= !XINPUT_GAMEPAD_DPAD_LEFT;
                } else if value < -0.5 {
                    pad.p_buttons |= XINPUT_GAMEPAD_DPAD_LEFT;
                    pad.p_buttons &= !XINPUT_GAMEPAD_DPAD_RIGHT;
                } else {
                    pad.p_buttons &= !(XINPUT_GAMEPAD_DPAD_LEFT | XINPUT_GAMEPAD_DPAD_RIGHT);
                }
            }
            a::AXIS_HAT_Y => {
                // Android hat Y is positive down.
                if value > 0.5 {
                    pad.p_buttons |= XINPUT_GAMEPAD_DPAD_DOWN;
                    pad.p_buttons &= !XINPUT_GAMEPAD_DPAD_UP;
                } else if value < -0.5 {
                    pad.p_buttons |= XINPUT_GAMEPAD_DPAD_UP;
                    pad.p_buttons &= !XINPUT_GAMEPAD_DPAD_DOWN;
                } else {
                    pad.p_buttons &= !(XINPUT_GAMEPAD_DPAD_UP | XINPUT_GAMEPAD_DPAD_DOWN);
                }
            }
            a::AXIS_THROTTLE | a::AXIS_VSCROLL | a::AXIS_HSCROLL => {
                // Known but not part of the XInput model: not a gamepad axis.
                count_log(&mut logged.unknown_axes, &format!("[gow.input] ignored non-gamepad axis {axis}"));
                return;
            }
            _ => {
                count_log(&mut logged.unknown_axes, &format!("[gow.input] unmapped axis {axis} value={value}"));
                return;
            }
        }
        if pad.connect_physical() {
            crate::glog!("[gow.input] gamepad {index} connected (axis {axis})");
        }
        pad.bump();
    }

    /// Touch event from the overlay. `pointer_id` is the Android pointer id,
    /// `x`/`y` are normalized to `[0,1]` (`px / width`, `py / height`).
    /// The first touch enables the virtual pad (which then reports connected);
    /// touches outside every region are logged, not guessed.
    pub fn on_touch(&self, pointer_id: i32, x: f32, y: f32, down: bool) {
        let mut d = self.lock();
        let dz = d.deadzone;
        let InputData { pads, touch, touch_pointers, logged, .. } = &mut *d;
        let pad = &mut pads[PRIMARY_PAD];

        if down {
            let (target, region) = if let Some(&held) = touch_pointers.get(&pointer_id) {
                held
            } else {
                match touch.hit(x, y) {
                    Some((t, r)) => {
                        touch_pointers.insert(pointer_id, (t, r));
                        (t, r)
                    }
                    None => {
                        count_log(
                            &mut logged.touch_outside,
                            &format!("[gow.input] touch p{pointer_id} ({x:.3},{y:.3}) outside overlay"),
                        );
                        return;
                    }
                }
            };
            if !pad.touch_enabled {
                pad.touch_enabled = true;
                pad.bump();
                crate::glog!("[gow.input] touch overlay active -> virtual gamepad {PRIMARY_PAD} connected");
            }
            apply_touch(pad, TouchPress { pointer_id, target, region, x, y, down: true, deadzone: dz });
            pad.bump();
        } else if let Some((target, region)) = touch_pointers.remove(&pointer_id) {
            apply_touch(pad, TouchPress { pointer_id, target, region, x, y, down: false, deadzone: dz });
            pad.bump();
        }
    }

    /// Enable/disable the touch overlay. Disabling clears its contribution and
    /// drops any held pointers; the pad then reports disconnected unless a real
    /// controller is also present.
    pub fn set_touch_overlay(&self, enabled: bool) {
        let mut d = self.lock();
        let InputData { pads, touch_pointers, .. } = &mut *d;
        let pad = &mut pads[PRIMARY_PAD];
        if pad.touch_enabled == enabled {
            return;
        }
        pad.touch_enabled = enabled;
        if !enabled {
            pad.t_buttons = 0;
            pad.t_lt = 0;
            pad.t_rt = 0;
            pad.t_lx = 0;
            pad.t_ly = 0;
            pad.t_rx = 0;
            pad.t_ry = 0;
            pad.lstick_pointer = None;
            pad.rstick_pointer = None;
            touch_pointers.clear();
        }
        pad.bump();
        crate::glog!("[gow.input] touch overlay {}", if enabled { "enabled" } else { "disabled" });
    }

    /// Mouse button event. `button` is an Android `MotionEvent.BUTTON_*` id.
    /// Also reflected into the keyboard state as `VK_LBUTTON`/`VK_RBUTTON`/
    /// `VK_MBUTTON` so `GetAsyncKeyState` sees it.
    pub fn on_mouse_button(&self, button: i32, down: bool) {
        use android as a;
        let mut d = self.lock();
        let vk = match button {
            a::BUTTON_PRIMARY => Some(vk::VK_LBUTTON),
            a::BUTTON_SECONDARY => Some(vk::VK_RBUTTON),
            a::BUTTON_TERTIARY => Some(vk::VK_MBUTTON),
            a::BUTTON_BACK => Some(vk::VK_XBUTTON1),
            a::BUTTON_FORWARD => Some(vk::VK_XBUTTON2),
            _ => None,
        };
        match vk {
            Some(vk) => {
                d.keyboard.set(vk, down);
                if down {
                    d.mouse.buttons |= button;
                } else {
                    d.mouse.buttons &= !button;
                }
            }
            None => count_log(&mut d.logged.unknown_keys, &format!("[gow.input] unknown mouse button {button}")),
        }
    }

    /// Set the absolute mouse position (pixels), accumulating relative motion.
    pub fn on_mouse_move(&self, x: f32, y: f32) {
        let mut d = self.lock();
        let nx = x.round() as i32;
        let ny = y.round() as i32;
        d.mouse.dx += nx - d.mouse.x;
        d.mouse.dy += ny - d.mouse.y;
        d.mouse.x = nx;
        d.mouse.y = ny;
    }

    /// Accumulate vertical wheel clicks.
    pub fn on_mouse_wheel(&self, delta: i32) {
        self.lock().mouse.wheel += delta;
    }

    /// Take (and clear) the accumulated wheel clicks.
    pub fn take_mouse_wheel(&self) -> i32 {
        let mut d = self.lock();
        std::mem::take(&mut d.mouse.wheel)
    }

    // -- diagnostics --------------------------------------------------------

    /// A compact, log-friendly snapshot of everything (buttons/axes in hex).
    pub fn dump_state(&self) -> String {
        let d = self.lock();
        let mut s = String::new();
        for (i, p) in d.pads.iter().enumerate() {
            match p.snapshot() {
                Some(g) => s.push_str(&format!(
                    "pad{i}=c btn=0x{:04x} lt={} rt={} L=({},{}) R=({},{}) rumble=({},{}) ",
                    g.wButtons, g.bLeftTrigger, g.bRightTrigger, g.sThumbLX, g.sThumbLY, g.sThumbRX, g.sThumbRY,
                    p.rumble_left, p.rumble_right
                )),
                None => s.push_str(&format!("pad{i}=- ")),
            }
        }
        s.push_str(&format!(
            "mouse=({},{}) btn={} wheel={} touch_ptrs={}",
            d.mouse.x,
            d.mouse.y,
            d.mouse.buttons,
            d.mouse.wheel,
            d.touch_pointers.len()
        ));
        s
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a normalized stick axis `[-1,1]` to `[-32767, 32767]`, applying a
/// deadzone (mapping the live range back to full scale).
pub fn stick_to_i16(v: f32, deadzone: f32) -> i16 {
    let v = v.clamp(-1.0, 1.0);
    let a = v.abs();
    let a = if a <= deadzone { 0.0 } else { (a - deadzone) / (1.0 - deadzone) };
    let scaled = (a * 32767.0).round();
    let signed = if v < 0.0 { -scaled } else { scaled };
    signed.clamp(-32768.0, 32767.0) as i16
}

/// Convert a trigger value to `0..=255`. Accepts `[0,1]` (standard) and clamps
/// negatives (some devices report rest at `-1`) to 0.
pub fn trigger_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A touch resolved to a target region, with everything needed to apply it.
#[derive(Clone, Copy)]
struct TouchPress {
    pointer_id: i32,
    target: TouchTarget,
    region: TouchRegion,
    x: f32,
    y: f32,
    down: bool,
    deadzone: f32,
}

/// Apply (or release) a touch target on a pad.
fn apply_touch(pad: &mut PadState, ev: TouchPress) {
    let TouchPress { pointer_id, target, region, x, y, down, deadzone } = ev;
    match target {
        TouchTarget::LeftStick => {
            if down {
                let (nx, ny) = region.stick_vector(x, y);
                pad.t_lx = stick_to_i16(nx, deadzone);
                pad.t_ly = stick_to_i16(ny, deadzone);
                pad.lstick_pointer = Some(pointer_id);
            } else if pad.lstick_pointer == Some(pointer_id) {
                pad.t_lx = 0;
                pad.t_ly = 0;
                pad.lstick_pointer = None;
            }
        }
        TouchTarget::RightStick => {
            if down {
                let (nx, ny) = region.stick_vector(x, y);
                pad.t_rx = stick_to_i16(nx, deadzone);
                pad.t_ry = stick_to_i16(ny, deadzone);
                pad.rstick_pointer = Some(pointer_id);
            } else if pad.rstick_pointer == Some(pointer_id) {
                pad.t_rx = 0;
                pad.t_ry = 0;
                pad.rstick_pointer = None;
            }
        }
        TouchTarget::DPad => {
            let dpad = XINPUT_GAMEPAD_DPAD_UP
                | XINPUT_GAMEPAD_DPAD_DOWN
                | XINPUT_GAMEPAD_DPAD_LEFT
                | XINPUT_GAMEPAD_DPAD_RIGHT;
            if down {
                let (nx, ny) = region.stick_vector(x, y);
                let mut bits = 0u16;
                if nx > 0.4 {
                    bits |= XINPUT_GAMEPAD_DPAD_RIGHT;
                } else if nx < -0.4 {
                    bits |= XINPUT_GAMEPAD_DPAD_LEFT;
                }
                if ny > 0.4 {
                    bits |= XINPUT_GAMEPAD_DPAD_UP;
                } else if ny < -0.4 {
                    bits |= XINPUT_GAMEPAD_DPAD_DOWN;
                }
                pad.t_buttons = (pad.t_buttons & !dpad) | bits;
            } else {
                pad.t_buttons &= !dpad;
            }
        }
        TouchTarget::Button(mask) => {
            if down {
                pad.t_buttons |= mask;
            } else {
                pad.t_buttons &= !mask;
            }
        }
        TouchTarget::LeftTrigger => {
            pad.t_lt = if down { 255 } else { 0 };
        }
        TouchTarget::RightTrigger => {
            pad.t_rt = if down { 255 } else { 0 };
        }
    }
}

/// The process-wide input state. `win32`/XInput dispatch and the JNI layer
/// should use this so they share one source of truth.
static GLOBAL: OnceLock<InputState> = OnceLock::new();

pub fn global() -> &'static InputState {
    GLOBAL.get_or_init(InputState::new)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_by_default_is_honest() {
        let s = InputState::new();
        for i in 0..MAX_PADS as u32 {
            assert!(s.xinput_get_state(i).is_none(), "pad {i} must start disconnected");
            assert!(!s.is_connected(i));
        }
        // Out-of-range index also reports "no device".
        assert!(s.xinput_get_state(9).is_none());
    }

    #[test]
    fn gamepad_button_connects_and_reports() {
        let s = InputState::new();
        s.on_key(android::KEYCODE_BUTTON_A, true);
        let g = s.xinput_get_state(0).expect("pad connected after a real button");
        assert_eq!(g.wButtons, XINPUT_GAMEPAD_A);
        s.on_key(android::KEYCODE_BUTTON_A, false);
        assert_eq!(s.xinput_get_state(0).unwrap().wButtons, 0);
    }

    #[test]
    fn right_stick_deadzone() {
        let s = InputState::new();
        s.on_motion(android::AXIS_Z, 0.05); // inside deadzone
        s.on_motion(android::AXIS_RZ, -1.0);
        let g = s.xinput_get_state(0).unwrap();
        assert_eq!(g.sThumbRX, 0);
        assert_eq!(g.sThumbRY, -32767);
    }

    #[test]
    fn keyboard_maps_to_vk() {
        let s = InputState::new();
        s.on_key(android::KEYCODE_W, true);
        assert!(s.vk_down(vk::VK_SPACE) == false);
        assert!(s.vk_down(0x57)); // 'W'
        assert!(s.android_key_down(android::KEYCODE_W));
        // A keyboard key must NOT connect a controller.
        assert!(s.xinput_get_state(0).is_none());
        s.on_key(android::KEYCODE_W, false);
        assert!(!s.vk_down(0x57));
    }

    #[test]
    fn hat_axes_drive_dpad() {
        let s = InputState::new();
        s.on_motion(android::AXIS_HAT_X, 1.0);
        s.on_motion(android::AXIS_HAT_Y, -1.0);
        let g = s.xinput_get_state(0).unwrap();
        assert!(g.is_pressed(XINPUT_GAMEPAD_DPAD_RIGHT));
        assert!(g.is_pressed(XINPUT_GAMEPAD_DPAD_UP));
        s.on_motion(android::AXIS_HAT_X, 0.0);
        s.on_motion(android::AXIS_HAT_Y, 0.0);
        assert_eq!(s.xinput_get_state(0).unwrap().wButtons, 0);
    }

    #[test]
    fn touch_left_stick_moves_and_releases() {
        let s = InputState::new();
        // Center of the left stick region is (0.15, 0.70).
        s.on_touch(1, 0.15, 0.70, true);
        s.on_touch(1, 0.15 + 0.14, 0.70, true); // full right
        let g = s.xinput_get_state(0).unwrap();
        assert!(g.sThumbLX > 30000, "lx={}", g.sThumbLX);
        s.on_touch(1, 0.0, 0.0, false);
        let g = s.xinput_get_state(0).unwrap();
        assert_eq!(g.sThumbLX, 0);
        assert_eq!(g.sThumbLY, 0);
    }

    #[test]
    fn touch_button_bit_and_physical_merge() {
        let s = InputState::new();
        s.on_touch(2, 0.84, 0.88, true); // A
        let g = s.xinput_get_state(0).unwrap();
        assert!(g.is_pressed(XINPUT_GAMEPAD_A));
        // A physical B press doesn't clobber the virtual A.
        s.on_key(android::KEYCODE_BUTTON_B, true);
        let g = s.xinput_get_state(0).unwrap();
        assert!(g.is_pressed(XINPUT_GAMEPAD_A));
        assert!(g.is_pressed(XINPUT_GAMEPAD_B));
        s.on_touch(2, 0.84, 0.88, false);
        let g = s.xinput_get_state(0).unwrap();
        assert!(!g.is_pressed(XINPUT_GAMEPAD_A));
        assert!(g.is_pressed(XINPUT_GAMEPAD_B));
    }

    #[test]
    fn rumble_only_on_connected_pad() {
        let s = InputState::new();
        assert_eq!(s.xinput_set_state(0, 100, 200), ERROR_DEVICE_NOT_CONNECTED);
        s.set_connected(0, true);
        assert_eq!(s.xinput_set_state(0, 100, 200), ERROR_SUCCESS);
        assert_eq!(s.rumble(0), Some((100, 200)));
    }

    #[test]
    fn packet_number_advances() {
        let s = InputState::new();
        s.on_key(android::KEYCODE_BUTTON_X, true);
        let a = s.xinput_get_state(0).unwrap().dwPacketNumber;
        s.on_key(android::KEYCODE_BUTTON_X, false);
        let b = s.xinput_get_state(0).unwrap().dwPacketNumber;
        assert!(b > a);
    }

    #[test]
    fn xinput_state_bytes_roundtrip() {
        let g = GamepadState {
            dwPacketNumber: 0x1122_3344,
            wButtons: XINPUT_GAMEPAD_A | XINPUT_GAMEPAD_START,
            bLeftTrigger: 200,
            bRightTrigger: 10,
            sThumbLX: -12345,
            sThumbLY: 32000,
            sThumbRX: 1,
            sThumbRY: -1,
        };
        let bytes = g.to_le_bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(GamepadState::from_le_bytes(&bytes), g);
        // Field offsets as the guest expects them.
        assert_eq!(&bytes[4..6], &g.wButtons.to_le_bytes());
        assert_eq!(bytes[6], 200);
        assert_eq!(bytes[7], 10);

        let mut dst = [0u8; 16];
        unsafe { g.write_to(dst.as_mut_ptr()) };
        assert_eq!(dst, bytes);
    }

    #[test]
    fn trigger_clamps() {
        assert_eq!(trigger_to_u8(-1.0), 0);
        assert_eq!(trigger_to_u8(0.0), 0);
        assert_eq!(trigger_to_u8(1.0), 255);
        assert_eq!(trigger_to_u8(2.0), 255);
    }
}
