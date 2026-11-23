# Input layer (Android -> XInput / user32)

God of War (2018) PC reads its gamepad through **`XINPUT1_4.dll`** (ordinals
`#2 = XInputGetState`, `#3 = XInputSetState`, confirmed from the game's IAT) and
its keyboard/mouse through **`user32`** (`GetAsyncKeyState`, `GetKeyboardState`,
raw input, ...). The port has no Windows input stack, so `runtime/src/input.rs`
owns the single, thread-safe source of truth:

```
Android UI thread                host game thread
  KeyEvent   ─┐                     ┌─ XInputGetState  (ord #2)
  MotionEvent ─┤  runtime::input     ├─ XInputSetState  (ord #3)
  TouchEvent  ─┼─► InputState ──────┤─ GetAsyncKeyState / keyboard
  MouseEvent  ─┘   (Arc<Mutex>)      └─ mouse position / buttons
```

The game runs on its own host thread and *reads* the state; the JNI bridge on
the Android UI thread *writes* it. `InputState` is cheap to clone (it is an
`Arc` internally), and [`input::global()`] returns the process-wide instance so
both sides share one object.

## Honesty rules (D005 / D012)

* **A pad is never faked as connected.** `xinput_get_state` returns `None`
  until something real reports a controller: a gamepad key/axis, an explicit
  `set_connected(true)` from the Java layer, or the touch overlay being enabled.
  `None` is the caller's `ERROR_DEVICE_NOT_CONNECTED` (1167).
* **Unknown inputs are logged, not guessed.** Unmapped keycodes, axes, mouse
  buttons and touches outside the overlay are counted and logged to logcat
  (`[gow.input] ... (xN)`), then ignored.
* **`XInputSetState` on a disconnected slot returns `ERROR_DEVICE_NOT_CONNECTED`**,
  like real XInput — we do not pretend we buzzed a controller that isn't there.

## Gamepad model (`GamepadState`)

Byte-for-byte identical to Win32 `XINPUT_STATE` (16 bytes, `#[repr(C)]`; the
size and field offsets are pinned by `const` asserts at compile time).
`GamepadState::write_to(ptr)` is what the XInput stub uses to fill the guest's
`XINPUT_STATE*`.

| Field | Type | Notes |
|---|---|---|
| `dwPacketNumber` | `u32` | bumped on every state change |
| `wButtons` | `u16` | bitfield below |
| `bLeftTrigger` / `bRightTrigger` | `u8` | 0..=255 |
| `sThumbLX` / `sThumbLY` | `i16` | left stick, negative = left/down |
| `sThumbRX` / `sThumbRY` | `i16` | right stick, negative = left/down |

Button masks (exact `XINPUT_GAMEPAD_*`):

| Button | Mask | Button | Mask |
|---|---|---|---|
| D-Pad up | `0x0001` | A | `0x1000` |
| D-Pad down | `0x0002` | B | `0x2000` |
| D-Pad left | `0x0004` | X | `0x4000` |
| D-Pad right | `0x0008` | Y | `0x8000` |
| Start | `0x0010` | LB | `0x0100` |
| Back | `0x0020` | RB | `0x0200` |
| Left thumb | `0x0040` | Right thumb | `0x0080` |

Slots `0..3`. All Android events target **slot 0** by default (GoW is
single-player); the `*_for(index, ...)` variants feed other slots later.

## Android -> XInput mapping

Gamepad keycodes (`android_keycode_to_button`) map straight to the mask above,
including the D-pad and `KEYCODE_BUTTON_L2/R2` as digital fallbacks for the
triggers. Axes (`on_motion`):

| Android axis | XInput |
|---|---|
| `AXIS_X` / `AXIS_Y` | `sThumbLX` / `sThumbLY` |
| `AXIS_Z` / `AXIS_RZ` (Xbox on Android) | `sThumbRX` / `sThumbRY` |
| `AXIS_RX` / `AXIS_RY` (alias used by some pads) | `sThumbRX` / `sThumbRY` |
| `AXIS_LTRIGGER` / `AXIS_GAS` | `bLeftTrigger` |
| `AXIS_RTRIGGER` / `AXIS_BRAKE` | `bRightTrigger` |
| `AXIS_HAT_X` / `AXIS_HAT_Y` | D-pad left/right, up/down |

Sticks are converted with `stick_to_i16` (deadzone 0.15, live range rescaled to
full scale); triggers with `trigger_to_u8` (clamped to 0..=255). `AXIS_THROTTLE`,
`AXIS_VSCROLL`, `AXIS_HSCROLL` are recognised as *not* gamepad axes and logged.

## Default keyboard -> game binding (keyboard/mouse players)

Keyboard keycodes go to the `user32` virtual-key state (`on_key` /
`on_key_event`), which is what the game's keyboard path reads. Because GoW lets
the player rebind keys itself, the host layer only has to deliver the raw keys
honestly; the D-pad keycodes are also translated to arrow keys so menu
navigation works out of the box.

| Action | Keyboard |
|---|---|
| Move | `W A S D` |
| Camera / aim | mouse (relative motion) |
| Light attack | left mouse button |
| Heavy attack | right mouse button |
| Interact | `E` |
| Block / parry | `Shift` (left) |
| Dodge / roll | `Space` |
| Rage / Spartan Rage | `Ctrl` |
| Aim throw (axe) | right mouse button (hold) |
| Recall axe | `Q` |
| Walk (slow) | `Alt` (left) |
| Pause / menu | `Esc` |
| Confirm / back | `Enter` / `Esc` (also D-pad + A/B) |

Mouse: absolute position, buttons (`VK_LBUTTON`/`VK_RBUTTON`/`VK_MBUTTON`), an
accumulated wheel, and accumulated relative motion (`take_relative`) for
raw-input camera look.

## Touch overlay -> controller

On a phone the game is played through a virtual gamepad drawn on the SurfaceView.
Regions are normalized `[0,1]` ellipses (`x = px / width`, `y = py / height`);
the first region containing a touch wins, so **discrete buttons are listed
before the big stick regions**. The default layout
(`TouchLayout::default_game()`, landscape) is:

```
 ┌────────────────────────────────────────────────────────────┐
 │  LT●                                                  ●RT   │
 │   LB●                                                ●RB    │
 │                                                             │
 │  ┌─────┐                                   Y                │
 │  │D-PAD│                                 X   B              │
 │  └─────┘                                   A                │
 │                                                             │
 │   ◎ left stick                            ◎ right stick     │
 │   (movement)                              (camera)          │
 │                       BACK        START                     │
 └────────────────────────────────────────────────────────────┘
```

| Overlay region | XInput |
|---|---|
| Left stick (drag) | left thumbstick (`sThumbLX/LY`) — **movement** |
| Right stick (drag) | right thumbstick (`sThumbRX/RY`) — **camera** |
| D-pad (direction of press) | D-pad up/down/left/right |
| Bottom-right cluster | `A` `B` `X` `Y` |
| `LB` / `RB` | `LEFT_SHOULDER` / `RIGHT_SHOULDER` |
| `LT` / `RT` | left / right trigger (digital, 0 or 255) |
| `BACK` / `START` | `BACK` / `START` |

The first touch enables the overlay, which makes the virtual pad report
**connected** (slot 0). `set_touch_overlay(false)` clears its contribution and
drops held pointers; the pad then reports disconnected unless a real controller
is present. Physical and virtual input are merged: a virtual `A` is not released
by a physical `B` changing state.

These coordinates are a starting point — tune them on-device, or ship a
different `TouchLayout` via `set_touch_layout`.

## API the JNI/Java layer calls

`InputState` (use `input::global()` or your own `InputState::new()`):

| Method | From Android |
|---|---|
| `on_key_event(source, keycode, down)` | `KeyEvent.getSource()`, `getKeyCode()`, action |
| `on_key(keycode, down)` | shortcut when the source is unknown |
| `on_key_event_for(index, source, keycode, down)` | per-controller |
| `on_motion(axis, value)` | `MotionEvent.getAxisValue(axis)` |
| `on_motion_for(index, axis, value)` | per-controller |
| `on_touch(pointer_id, x, y, down)` | `x = getX()/width`, `y = getY()/height` |
| `on_mouse_move(x, y)` | `ACTION_MOVE` / `HOVER_MOVE`, pixels |
| `on_mouse_button(button, down)` | `getButtonState() & BIT` |
| `on_mouse_wheel(delta)` | `AXIS_VSCROLL` sign |
| `set_connected(index, bool)` | when Java knows a device is attached |
| `set_touch_overlay(bool)` | overlay on/off |
| `set_touch_layout(layout)` / `set_deadzone(f)` | tuning |
| `dump_state()` | compact snapshot for logs / an on-screen debug overlay |

The game-side `win32`/XInput dispatch should use `input::global()`:
`xinput_get_state(i)` -> `Some` -> `state.write_to(arg1)`, return 0; `None` ->
return `ERROR_DEVICE_NOT_CONNECTED`. Rumble goes through `xinput_set_state`
and can be polled by the Android `Vibrator` bridge via `rumble(i)`.

## Wiring (not done in this change)

`runtime/src/input.rs` is self-contained; it is **not yet registered** and no
existing file was modified. To expose it:

1. Add one line to `runtime/src/lib.rs`: `pub mod input;`
2. Route the XInput IAT slots (`xinput1_4.dll` ord `#2`/`#3`) through
   `crate::input::global()` in `runtime/src/win32.rs`.
3. Add `nativeKey` / `nativeMotion` / `nativeTouch` JNI entry points that call
   the methods above, and override `onKeyDown`/`onTouchEvent` in `GowActivity`.
