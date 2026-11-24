# On-screen virtual gamepad (GamepadOverlayView)

A real, on-device virtual controller drawn over the game surface. Dark,
semi-transparent, minimalist; no image assets and no third-party dependencies —
everything is anti-aliased `Canvas` primitives.

```
  ┌───────────────────────────────────────────────────────────────────────┐
  │  (LT)  (LB)                                    [ gear ]      (RB) (RT) │
  │                                                                       │
  │                                                                       │
  │    ┌─────┐                                        Y                    │
  │    │  D  │                                      X   B      ◎ right      │
  │    └─────┘                                        A        stick        │
  │                    ( BACK )    ( START )                              │
  │    ◎ left stick                                                       │
  └───────────────────────────────────────────────────────────────────────┘
```

* Fill `#59000000` (≈35% black), outline `#59FFFFFF`, idle text `#B3FFFFFF`.
* Pressed state brightens to a translucent accent (`#4FC3F7` fill/stroke).
* Strokes are thin (`≈0.045 × radius`) and AA'd, so the controls stay legible
  over any frame without obscuring it.

## Files

| File | Role |
|---|---|
| `android/app/src/main/java/com/gow/arm64/GamepadLayout.java` | Pure-Java geometry (no Android imports): region sizes/positions, hit testing, drag offsets. Unit-testable offline. |
| `android/app/src/main/java/com/gow/arm64/GamepadOverlayView.java` | The `View`: drawing, pointer tracking, edit mode, haptics, persistence; emits input to the `InputSink`. |
| `android/app/src/main/java/com/gow/arm64/GowActivity.java` | Hosts the `FrameLayout` (SurfaceView + overlay) and implements `InputSink` against the JNI entry points. |
| `android/tools/GamepadLayoutDemo.java` | Runnable `main()` that prints the geometry and runs sanity checks (needs a JDK; harmless if absent). |
| `runtime/src/jni.rs` | Existing `nativeMotion`/`nativeKey`/`nativeTouch` deliver the input; adds `nativeClearTouchLayout`. |

No layout XML is needed: the view tree is composed programmatically
(`FrameLayout` → `SurfaceView` + overlay), so there is no `res/` directory and
nothing to inflate.

## Control layout

Landscape, sized as fractions of the unit `u = min(width, height)`, so it scales
with the screen. Everything is inset by the display cutout / system-bar safe
insets and a margin (`0.055u`, `0.030u` in compact mode).

| Control | Placement | Drawn size |
|---|---|---|
| Left stick | bottom-left corner | base radius `0.150u`, knob `0.062u` |
| Right stick | bottom-right corner | same |
| LT / RT | top corners (outermost) | pill `0.150u × 0.064u` |
| LB / RB | top corners (inboard of LT/RT) | pill `0.150u × 0.064u` |
| A / B / X / Y | right, above the right stick, diamond of half-diagonal `0.090u` | circle r `0.050u` |
| D-pad | left, above the left stick | cross, arm reach `0.108u` |
| BACK / START | bottom centre, low | pill `0.100u × 0.048u` |

Hit shapes are larger than the drawn shapes (buttons `1.55×`, D-pad `1.22×`,
pills `1.30×`, sticks `1.16×`) so the controls are comfortable on glass.

**Compact mode** multiplies all sizes by `0.85` and tightens the margin; it is
selected from `smallestScreenWidthDp < 600` by default and can be toggled in the
editor.

## Mapping to the input layer

The overlay emits gamepad **motion axes** and **key events**, exactly the model
`runtime/src/input.rs` already consumes (see `docs/INPUT.md`). **No new mapping
logic was needed**; the existing JNI entry points are reused:

| Overlay control | Emitted event | `nativeMotion`/`nativeKey` → `input.rs` |
|---|---|---|
| Left stick | `AXIS_X`, `AXIS_Y` | `sThumbLX` / `sThumbLY` (movement) |
| Right stick | `AXIS_Z`, `AXIS_RZ` | `sThumbRX` / `sThumbRY` (camera) |
| LT / RT | `AXIS_LTRIGGER` / `AXIS_RTRIGGER` (0 or 1) | `bLeftTrigger` / `bRightTrigger` (0/255) |
| D-pad | `AXIS_HAT_X`, `AXIS_HAT_Y` | D-pad up/down/left/right (threshold 0.5) |
| A / B / X / Y | `KEYCODE_BUTTON_A/B/X/Y` (96/97/99/100) | `XINPUT_GAMEPAD_A/B/X/Y` |
| LB / RB | `KEYCODE_BUTTON_L1/R1` (102/103) | `LEFT_SHOULDER` / `RIGHT_SHOULDER` |
| START / BACK | `KEYCODE_BUTTON_START/SELECT` (108/109) | `START` / `BACK` |

Key events are sent with source `InputDevice.SOURCE_GAMEPAD` so
`on_key_event` routes them to the controller (not the keyboard). Sticks are
up-positive (`AXIS_Y`/`AXIS_RZ` are negated from screen-down); the D-pad hat is
screen-down-positive, matching Android/`input.rs`.

Triggers use the **analog** axes rather than the digital `KEYCODE_BUTTON_L2/R2`
fallback, so the game sees a full 0/255 pull (`trigger_to_u8`), not a digital
shoulder emulation.

### Why the Rust touch layout is cleared

`GamepadOverlayView` is the single touch-geometry authority. `GowActivity`
calls `nativeClearTouchLayout()` at startup, which empties the built-in
`TouchLayout` in `input.rs`. This prevents a pass-through touch from being
re-interpreted by the *old* Rust geometry and double-driving a stick or button.
With the layout empty, a forwarded pass-through touch is logged as "outside the
overlay" and ignored — never guessed (honesty rules D005/D012).

## Touch routing (multi-touch correct)

The overlay claims each gesture and tracks every pointer id **independently**:

* Each finger is classified once, on its own down event: gear → a control →
  otherwise a game pointer.
* Two sticks, stick + buttons, and two fingers on one button all work; a button
  uses a press count, so releasing one finger keeps it held while the other
  remains down.
* If a finger misses every control it is forwarded to `InputSink.gameTouch` and
  on to `nativeTouch` — **not dropped**. Claiming the gesture (rather than
  letting Android route it away) is what makes press order irrelevant: once a
  touch leaves this view, Android will not route a later pointer of the same
  gesture back to it.
* `ACTION_CANCEL` releases everything cleanly.

The overlay is **non-focusable** and never grabs hardware keys; a tap in the
middle of the screen reaches the game (via the forwarded touch path), so the
overlay never blocks the frame.

## Haptics

A short (~12 ms) `Vibrator` tick fires on the 0→1 edge of a button press.
Controlled by the static flag `GamepadOverlayView.hapticsEnabled`
(default `true`); set it to `false` from the activity to mute. It no-ops when
the device has no vibrator, and it disables itself (with a log warning) if
`android.permission.VIBRATE` is not granted — see follow-ups.

## Edit mode / settings

Enter edit mode by **triple-tapping a free area**, or by tapping the small gear
at top centre. In edit mode:

* **Drag any control** to reposition it (the top panel's buttons are excluded).
* **Top panel**: `SIZE -` / `SIZE +` (scale 0.6–1.6), `DIM` / `BRIGHT`
  (opacity 0.3–1.0), `FIT` (toggle compact), `RESET` (clear drag offsets),
  `DONE` (exit).
* The selected control is outlined in accent while dragged.

Everything (scale, opacity, compact, and per-control drag offsets as fractions
of the unit) is persisted in `SharedPreferences` (`gamepad_overlay`) and
restored on launch. The gear is always live, even outside edit mode, so you can
never get stuck.

## Screenshots (what to expect on-device)

* **Idle**: two faint dark circles low in each bottom corner (the sticks) with
  the knob centred; top-left and top-right each show two light-outlined pills
  side by side (`LT LB` / `RB RT`); a small cross (D-pad) on the left and a
  diamond `Y/X A/B` on the right, both in the upper half; two small pills
  (`BACK START`) low centre; a tiny gear top centre. The game shows through
  everywhere else.
* **Driving**: the pressed control's fill/stroke turn pale blue and the label
  brightens; a held stick's knob slides toward the finger and eases back to
  centre on release.
* **Edit mode** (after triple-tap or gear): the top row of pills appears and the
  gear turns accent-blue; dragging a control outlines it in accent.

## Verification / follow-ups

* `GamepadLayout` is pure Java: run `android/tools/GamepadLayoutDemo.java` with
  a JDK to print geometry and run the built-in sanity checks (all ids present,
  centres on screen, hit-test round-trips, screen centre is free, X/Y/A/B
  ordering). Without a JDK the file is inert.
* The runtime `.so` must be rebuilt to expose `nativeClearTouchLayout`; the
  activity calls it inside a `try`/`catch (UnsatisfiedLinkError)` so an older
  `.so` still launches (it just keeps the dormant Rust layout).
* **Permission**: add `<uses-permission android:name="android.permission.VIBRATE"/>`
  to `AndroidManifest.xml` to enable the haptic tick. Without it the code logs a
  warning and continues silently (it never crashes).
* No manifest/theme change is required otherwise. The activity is already
  `landscape` with `configChanges` handled; the overlay reads insets directly
  from the view.
* `docs/INPUT.md` still describes the Rust `TouchLayout` as the touch geometry
  and lists the JNI wiring as "not done". That section is now historical for the
  on-screen pad — update it (outside this change) to point here.
