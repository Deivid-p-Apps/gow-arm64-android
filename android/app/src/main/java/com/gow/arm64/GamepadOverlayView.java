package com.gow.arm64;

import android.content.Context;
import android.content.SharedPreferences;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Typeface;
import android.os.Build;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.util.Log;
import android.util.SparseBooleanArray;
import android.util.SparseIntArray;
import android.view.DisplayCutout;
import android.view.InputDevice;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.view.WindowInsets;

/**
 * Transparent on-screen virtual gamepad drawn over the game's {@code SurfaceView}.
 *
 * <p>It is deliberately dependency-free: everything is anti-aliased
 * {@link Canvas} primitives (circles, rounded rectangles, a small gear) with a
 * dark translucent fill and a thin light outline. No image assets, no libraries.
 *
 * <h2>Input model</h2>
 * The overlay behaves like a real Bluetooth gamepad, emitting <b>motion axis</b>
 * and <b>key</b> events into {@code runtime/src/input.rs} through
 * {@link InputSink}. The mapping is exactly the one documented in
 * {@code docs/INPUT.md} and understood by {@code input.rs} today, so <b>no
 * native change is required</b>:
 *
 * <pre>
 *   left stick  → AXIS_X / AXIS_Y      (sThumbLX / sThumbLY, movement)
 *   right stick → AXIS_Z / AXIS_RZ     (sThumbRX / sThumbRY, camera)
 *   LT / RT     → AXIS_LTRIGGER / AXIS_RTRIGGER  (digital 0 or 1)
 *   D-pad       → AXIS_HAT_X / AXIS_HAT_Y
 *   A B X Y     → KEYCODE_BUTTON_A/B/X/Y
 *   LB / RB     → KEYCODE_BUTTON_L1/R1
 *   START/BACK  → KEYCODE_BUTTON_START / BUTTON_SELECT  (source SOURCE_GAMEPAD)
 * </pre>
 *
 * Key events carry {@link InputDevice#SOURCE_GAMEPAD} so
 * {@code on_key_event} attributes them to the controller rather than the
 * keyboard. Triggers use the analog axes rather than the digital
 * {@code L2/R2} fallback, so {@code bLeftTrigger}/{@code bRightTrigger} get 0/255.
 *
 * <h2>Pointer ownership (never drop a pointer)</h2>
 * The overlay is the single touch authority for the window: it claims every
 * gesture and tracks each pointer id independently, so any press order works
 * (stick + button, two sticks + buttons, a control added while another is held).
 * A pointer that does <b>not</b> land on a control is forwarded to the game
 * unchanged through {@link InputSink#gameTouch}, so a tap in the middle of the
 * screen still reaches the game — the overlay does not swallow it. (Claiming the
 * gesture, rather than returning {@code false}, is what makes multi-touch
 * order-independent: once a touch is routed away from this view, Android will
 * not route a later pointer of the same gesture back to it.)
 *
 * <h2>Edit mode</h2>
 * Triple-tap a free area, or tap the gear, to enter edit mode: drag controls to
 * move them, use the top panel to change size / opacity / compact and to reset.
 * Geometry is persisted in {@code SharedPreferences}.
 */
public class GamepadOverlayView extends View {

    /** Where the overlay delivers input. Implemented by the activity. */
    public interface InputSink {
        /** A gamepad axis value, as {@code MotionEvent#getAxisValue(axis)}. */
        void axis(int axis, float value);

        /** A gamepad key event with an explicit Android source. */
        void key(int source, int keyCode, boolean down);

        /** A non-control touch, normalized {@code [0,1]}, for the game itself. */
        void gameTouch(int pointerId, float x, float y, boolean down);
    }

    private static final String TAG = "GoW-gp";
    private static final String PREFS = "gamepad_overlay";

    /** Haptic tick on button press. Toggle from the activity if desired. */
    public static boolean hapticsEnabled = true;

    // ---- Android constants we emit (kept explicit for auditing) ----------
    private static final int SRC_GAMEPAD = InputDevice.SOURCE_GAMEPAD; // 0x401
    private static final int AXIS_X = MotionEvent.AXIS_X;
    private static final int AXIS_Y = MotionEvent.AXIS_Y;
    private static final int AXIS_Z = MotionEvent.AXIS_Z;
    private static final int AXIS_RZ = MotionEvent.AXIS_RZ;
    private static final int AXIS_LTRIGGER = MotionEvent.AXIS_LTRIGGER;
    private static final int AXIS_RTRIGGER = MotionEvent.AXIS_RTRIGGER;
    private static final int AXIS_HAT_X = MotionEvent.AXIS_HAT_X;
    private static final int AXIS_HAT_Y = MotionEvent.AXIS_HAT_Y;

    private static int buttonKeyCode(int target) {
        switch (target) {
            case GamepadLayout.A: return KeyEvent.KEYCODE_BUTTON_A;      // 96
            case GamepadLayout.B: return KeyEvent.KEYCODE_BUTTON_B;      // 97
            case GamepadLayout.X: return KeyEvent.KEYCODE_BUTTON_X;      // 99
            case GamepadLayout.Y: return KeyEvent.KEYCODE_BUTTON_Y;      // 100
            case GamepadLayout.LB: return KeyEvent.KEYCODE_BUTTON_L1;    // 102
            case GamepadLayout.RB: return KeyEvent.KEYCODE_BUTTON_R1;    // 103
            case GamepadLayout.START: return KeyEvent.KEYCODE_BUTTON_START; // 108
            case GamepadLayout.BACK: return KeyEvent.KEYCODE_BUTTON_SELECT; // 109
            default: return -1;
        }
    }

    // ---- drawing palette -------------------------------------------------
    private static final int FILL_IDLE = 0x59000000;      // 35% black
    private static final int STROKE_IDLE = 0x59FFFFFF;    // subtle light outline
    private static final int TEXT_IDLE = 0xB3FFFFFF;
    private static final int FILL_PRESSED = 0x664FC3F7;
    private static final int STROKE_PRESSED = 0xE64FC3F7;
    private static final int TEXT_PRESSED = 0xFFFFFFFF;
    private static final int ACCENT = 0xFF4FC3F7;
    private static final int PANEL_FILL = 0xCC101418;
    private static final int PANEL_TEXT = 0xF2FFFFFF;

    private static final int PANEL_LABELS = 7;
    private static final String[] PANEL_TEXT_LABELS = {
            "SIZE -", "SIZE +", "DIM", "BRIGHT", "FIT", "RESET", "DONE"
    };

    // ---- configuration (persisted) --------------------------------------
    private float scale = 1f;
    private float opacity = 1f;
    private boolean compact;
    private final float[] offsets = new float[GamepadLayout.TARGET_COUNT * 2];

    // ---- state -----------------------------------------------------------
    private InputSink sink;
    private SharedPreferences prefs;
    private GamepadLayout layout;      // with offsets
    private GamepadLayout homeLayout;  // offsets zeroed, for drag math
    private int inL, inT, inR, inB;    // safe insets, px

    private boolean editMode;
    private int editDragPointer = -1;
    private int editDragTarget = -1;

    // Per-target press counts (two fingers can hold the same button).
    private final int[] pressCount = new int[GamepadLayout.TARGET_COUNT];
    private final boolean[] pressed = new boolean[GamepadLayout.TARGET_COUNT];

    private final SparseIntArray pointerTarget = new SparseIntArray();
    private final SparseBooleanArray forwardedPointers = new SparseBooleanArray();

    private final Stick leftStick = new Stick(GamepadLayout.LEFT_STICK);
    private final Stick rightStick = new Stick(GamepadLayout.RIGHT_STICK);

    private int dpadPointer = -1;
    private float dpadNx, dpadNy;
    private boolean dpadActive;

    // Triple-tap detection.
    private long tapFirstTime;
    private int tapCount;
    private float tapAnchorX, tapAnchorY;

    // Edit-panel geometry and gear.
    private final RectF[] panelRects = new RectF[PANEL_LABELS];
    private float gearX, gearY, gearR;

    private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint stroke = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final RectF tmpRect = new RectF();

    private final Vibrator vibrator;

    public GamepadOverlayView(Context context, InputSink sink) {
        super(context);
        this.sink = sink;
        prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        loadPrefs(context);
        setBackgroundColor(Color.TRANSPARENT);
        setFocusable(false);
        setFocusableInTouchMode(false);
        setClickable(false);
        text.setTypeface(Typeface.create(Typeface.DEFAULT, Typeface.BOLD));
        text.setTextAlign(Paint.Align.CENTER);
        stroke.setStyle(Paint.Style.STROKE);
        vibrator = (Vibrator) context.getSystemService(Context.VIBRATOR_SERVICE);
    }

    public void setInputSink(InputSink sink) {
        this.sink = sink;
    }

    public boolean isEditMode() {
        return editMode;
    }

    // =====================================================================
    // Persistence
    // =====================================================================

    private void loadPrefs(Context context) {
        scale = prefs.getFloat("scale", 1f);
        opacity = prefs.getFloat("opacity", 1f);
        compact = prefs.getBoolean("compact",
                context.getResources().getConfiguration().smallestScreenWidthDp < 600);
        for (int i = 0; i < GamepadLayout.TARGET_COUNT; i++) {
            offsets[i * 2] = prefs.getFloat("off_" + i + "_x", 0f);
            offsets[i * 2 + 1] = prefs.getFloat("off_" + i + "_y", 0f);
        }
    }

    private void savePrefs() {
        SharedPreferences.Editor e = prefs.edit();
        e.putFloat("scale", scale);
        e.putFloat("opacity", opacity);
        e.putBoolean("compact", compact);
        for (int i = 0; i < GamepadLayout.TARGET_COUNT; i++) {
            e.putFloat("off_" + i + "_x", offsets[i * 2]);
            e.putFloat("off_" + i + "_y", offsets[i * 2 + 1]);
        }
        e.apply();
    }

    // =====================================================================
    // Layout / insets
    // =====================================================================

    @Override
    protected void onAttachedToWindow() {
        super.onAttachedToWindow();
        requestApplyInsets();
    }

    @Override
    @SuppressWarnings("deprecation")
    public WindowInsets onApplyWindowInsets(WindowInsets insets) {
        int l = insets.getSystemWindowInsetLeft();
        int t = insets.getSystemWindowInsetTop();
        int r = insets.getSystemWindowInsetRight();
        int b = insets.getSystemWindowInsetBottom();
        DisplayCutout cut = insets.getDisplayCutout();
        if (cut != null) {
            l = Math.max(l, cut.getSafeInsetLeft());
            t = Math.max(t, cut.getSafeInsetTop());
            r = Math.max(r, cut.getSafeInsetRight());
            b = Math.max(b, cut.getSafeInsetBottom());
        }
        if (l != inL || t != inT || r != inR || b != inB) {
            inL = l; inT = t; inR = r; inB = b;
            rebuild();
        }
        return insets;
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        rebuild();
    }

    private void rebuild() {
        int w = getWidth(), h = getHeight();
        if (w <= 0 || h <= 0) return;
        layout = GamepadLayout.compute(w, h, inL, inT, inR, inB, scale, compact, offsets);
        homeLayout = GamepadLayout.compute(w, h, inL, inT, inR, inB, scale, compact, null);

        float u = layout.unit;
        // Gear sits at the top centre; the edit panel is a row of pills BELOW
        // it, so the two never overlap (otherwise the panel's centre button and
        // the gear would fight for the same touch).
        gearR = 0.028f * u * scale;
        gearX = w * 0.5f;
        gearY = inT + 0.024f * u * scale + gearR;

        float ph = 0.066f * u * scale;
        float gap = 0.012f * u;
        float pw = Math.min(0.130f * u, (w - 2f * inL - (PANEL_LABELS - 1) * gap) / PANEL_LABELS);
        float total = PANEL_LABELS * pw + (PANEL_LABELS - 1) * gap;
        float x0 = (w - total) * 0.5f;
        float py = gearY + gearR + 0.026f * u * scale;
        for (int i = 0; i < PANEL_LABELS; i++) {
            float lx = x0 + i * (pw + gap);
            panelRects[i] = new RectF(lx, py, lx + pw, py + ph);
        }
        invalidate();
    }

    // =====================================================================
    // Touch handling
    // =====================================================================

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (layout == null) return false;

        if (editMode) {
            return handleEditTouch(event);
        }

        final int action = event.getActionMasked();
        switch (action) {
            case MotionEvent.ACTION_DOWN: {
                int id = event.getPointerId(0);
                float x = event.getX(0), y = event.getY(0);
                handleDown(id, x, y);
                return true;
            }
            case MotionEvent.ACTION_POINTER_DOWN: {
                int idx = event.getActionIndex();
                handleDown(event.getPointerId(idx), event.getX(idx), event.getY(idx));
                return true;
            }
            case MotionEvent.ACTION_MOVE: {
                for (int i = 0; i < event.getPointerCount(); i++) {
                    int id = event.getPointerId(i);
                    float x = event.getX(i), y = event.getY(i);
                    if (!onControlMove(id, x, y) && forwardedPointers.get(id)) {
                        forward(id, x, y, true);
                    }
                }
                invalidate();
                return true;
            }
            case MotionEvent.ACTION_UP:
            case MotionEvent.ACTION_POINTER_UP: {
                int idx = event.getActionIndex();
                int id = event.getPointerId(idx);
                if (!onControlUp(id) && forwardedPointers.get(id)) {
                    forward(id, event.getX(idx), event.getY(idx), false);
                }
                invalidate();
                return true;
            }
            case MotionEvent.ACTION_CANCEL:
                onCancel();
                return true;
            default:
                return true;
        }
    }

    /** Decide what a freshly-pressed pointer drives: the gear, a control, or the game. */
    private void handleDown(int id, float x, float y) {
        if (hitGear(x, y)) {
            setEditMode(true);
            return;
        }
        GamepadLayout.Region r = layout.hitTest(x, y);
        if (r != null) {
            onControlDown(id, r, x, y);
            return;
        }
        // No control here (and no edit panel): hand the raw pointer to the game.
        maybeTripleTap(x, y);
        forward(id, x, y, true);
    }

    private void forward(int id, float x, float y, boolean down) {
        float w = Math.max(1f, getWidth());
        float h = Math.max(1f, getHeight());
        sink.gameTouch(id, x / w, y / h, down);
        if (down) forwardedPointers.put(id, true);
        else forwardedPointers.delete(id);
    }

    private boolean hitGear(float x, float y) {
        return Math.hypot(x - gearX, y - gearY) <= gearR * 2.2f;
    }

    // ---- control press/release ------------------------------------------

    private void onControlDown(int id, GamepadLayout.Region region, float x, float y) {
        int target = region.target;
        if (GamepadLayout.isStick(target)) {
            Stick stick = (target == GamepadLayout.LEFT_STICK) ? leftStick : rightStick;
            if (stick.pointer != -1) return; // already driven by another finger
            stick.pointer = id;
            stick.active = true;
            updateStick(stick, region, x, y);
            sendStickAxes(stick);
        } else if (target == GamepadLayout.DPAD) {
            if (dpadPointer != -1) return;
            dpadPointer = id;
            dpadActive = true;
            updateDpad(region, x, y);
        } else {
            pointerTarget.put(id, target);
            boolean was = pressed[target];
            pressCount[target]++;
            pressed[target] = true;
            if (!was) {
                sendButton(target, true);
                buzz();
                Log.i(TAG, "press " + GamepadLayout.nameOf(target));
            }
        }
        invalidate();
    }

    /** @return true if this pointer drives a control (stick/d-pad/button). */
    private boolean onControlMove(int id, float x, float y) {
        if (leftStick.pointer == id) {
            updateStick(leftStick, layout.leftStick, x, y);
            sendStickAxes(leftStick);
            return true;
        }
        if (rightStick.pointer == id) {
            updateStick(rightStick, layout.rightStick, x, y);
            sendStickAxes(rightStick);
            return true;
        }
        if (dpadPointer == id) {
            updateDpad(layout.dpad, x, y);
            return true;
        }
        return pointerTarget.indexOfKey(id) >= 0;
    }

    /** @return true if this pointer drove a control. */
    private boolean onControlUp(int id) {
        if (leftStick.pointer == id) {
            releaseStick(leftStick);
            sendStickAxes(leftStick);
            return true;
        }
        if (rightStick.pointer == id) {
            releaseStick(rightStick);
            sendStickAxes(rightStick);
            return true;
        }
        if (dpadPointer == id) {
            dpadPointer = -1;
            dpadActive = false;
            dpadNx = 0;
            dpadNy = 0;
            sink.axis(AXIS_HAT_X, 0f);
            sink.axis(AXIS_HAT_Y, 0f);
            return true;
        }
        int i = pointerTarget.indexOfKey(id);
        if (i >= 0) {
            int target = pointerTarget.valueAt(i);
            pointerTarget.removeAt(i);
            pressCount[target] = Math.max(0, pressCount[target] - 1);
            if (pressCount[target] == 0) {
                pressed[target] = false;
                sendButton(target, false);
            }
            return true;
        }
        return false;
    }

    private void onCancel() {
        if (leftStick.pointer != -1 || leftStick.active) {
            releaseStick(leftStick);
            sendStickAxes(leftStick);
        }
        if (rightStick.pointer != -1 || rightStick.active) {
            releaseStick(rightStick);
            sendStickAxes(rightStick);
        }
        if (dpadPointer != -1) {
            dpadPointer = -1;
            dpadActive = false;
            dpadNx = 0;
            dpadNy = 0;
            sink.axis(AXIS_HAT_X, 0f);
            sink.axis(AXIS_HAT_Y, 0f);
        }
        for (int i = 0; i < GamepadLayout.TARGET_COUNT; i++) {
            if (pressed[i]) {
                pressed[i] = false;
                pressCount[i] = 0;
                sendButton(i, false);
            }
        }
        pointerTarget.clear();
        for (int i = 0; i < forwardedPointers.size(); i++) {
            sink.gameTouch(forwardedPointers.keyAt(i), 0f, 0f, false);
        }
        forwardedPointers.clear();
        invalidate();
    }

    // ---- stick helpers ---------------------------------------------------

    private void updateStick(Stick s, GamepadLayout.Region r, float x, float y) {
        float vis = Math.max(r.visR(), 1f);
        float nx = (x - r.cx) / vis;
        float ny = -(y - r.cy) / vis; // input layer wants up-positive
        float m = (float) Math.hypot(nx, ny);
        if (m > 1f) {
            nx /= m;
            ny /= m;
        }
        s.nx = nx;
        s.ny = ny;
        s.ax = nx * vis;
        s.ay = -ny * vis;
    }

    private void releaseStick(Stick s) {
        s.pointer = -1;
        s.active = false;
        s.nx = 0;
        s.ny = 0;
    }

    private void sendStickAxes(Stick s) {
        if (s.target == GamepadLayout.LEFT_STICK) {
            sink.axis(AXIS_X, s.nx);
            sink.axis(AXIS_Y, s.ny);
        } else {
            sink.axis(AXIS_Z, s.nx);
            sink.axis(AXIS_RZ, s.ny);
        }
    }

    private void updateDpad(GamepadLayout.Region r, float x, float y) {
        float vis = Math.max(r.visR(), 1f);
        dpadNx = clamp((x - r.cx) / vis, -1f, 1f);
        dpadNy = clamp((y - r.cy) / vis, -1f, 1f); // screen-down positive, as HAT expects
        sink.axis(AXIS_HAT_X, dpadNx);
        sink.axis(AXIS_HAT_Y, dpadNy);
    }

    private void sendButton(int target, boolean down) {
        if (target == GamepadLayout.LT) {
            sink.axis(AXIS_LTRIGGER, down ? 1f : 0f);
        } else if (target == GamepadLayout.RT) {
            sink.axis(AXIS_RTRIGGER, down ? 1f : 0f);
        } else {
            int kc = buttonKeyCode(target);
            if (kc >= 0) sink.key(SRC_GAMEPAD, kc, down);
        }
    }

    private void buzz() {
        if (!hapticsEnabled || vibrator == null || !vibrator.hasVibrator()) return;
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                vibrator.vibrate(VibrationEffect.createOneShot(12, VibrationEffect.DEFAULT_AMPLITUDE));
            } else {
                vibrator.vibrate(12);
            }
        } catch (SecurityException e) {
            // No android.permission.VIBRATE in the manifest: stay silent rather
            // than crash. Add the permission to enable haptics (see GAMEPAD.md).
            hapticsEnabled = false;
            Log.w(TAG, "haptics disabled: " + e.getMessage());
        }
    }

    // ---- triple tap ------------------------------------------------------

    private void maybeTripleTap(float x, float y) {
        long now = android.os.SystemClock.uptimeMillis();
        if (tapCount == 0 || now - tapFirstTime > 1400
                || Math.hypot(x - tapAnchorX, y - tapAnchorY) > 0.12f * layout.unit) {
            tapCount = 1;
            tapFirstTime = now;
            tapAnchorX = x;
            tapAnchorY = y;
            return;
        }
        tapCount++;
        if (tapCount >= 3) {
            tapCount = 0;
            setEditMode(true);
        }
    }

    public void setEditMode(boolean on) {
        if (editMode == on) return;
        if (!on) savePrefs();
        if (on) onCancel(); // drop any held gamepad/game input
        editMode = on;
        editDragPointer = -1;
        editDragTarget = -1;
        Log.i(TAG, "edit mode " + (on ? "ON" : "OFF"));
        invalidate();
    }

    // =====================================================================
    // Edit mode
    // =====================================================================

    private boolean handleEditTouch(MotionEvent e) {
        final int action = e.getActionMasked();
        switch (action) {
            case MotionEvent.ACTION_DOWN:
            case MotionEvent.ACTION_POINTER_DOWN: {
                int idx = e.getActionIndex();
                int id = e.getPointerId(idx);
                float x = e.getX(idx), y = e.getY(idx);
                if (hitGear(x, y)) {
                    setEditMode(false);
                    return true;
                }
                int pi = panelHit(x, y);
                if (pi >= 0) {
                    editDragPointer = id;
                    editDragTarget = -100 - pi; // encode a panel button
                    return true;
                }
                GamepadLayout.Region r = layout.hitTest(x, y);
                if (r != null) {
                    editDragPointer = id;
                    editDragTarget = r.target;
                }
                invalidate();
                return true; // swallow everything while editing
            }
            case MotionEvent.ACTION_MOVE: {
                if (editDragPointer < 0 || editDragTarget < 0) return true;
                for (int i = 0; i < e.getPointerCount(); i++) {
                    if (e.getPointerId(i) != editDragPointer) continue;
                    dragControl(editDragTarget, e.getX(i), e.getY(i));
                    invalidate();
                    return true;
                }
                return true;
            }
            case MotionEvent.ACTION_UP:
            case MotionEvent.ACTION_POINTER_UP: {
                int idx = e.getActionIndex();
                if (e.getPointerId(idx) != editDragPointer) return true;
                if (editDragTarget <= -100) applyPanel(-editDragTarget - 100);
                editDragPointer = -1;
                editDragTarget = -1;
                invalidate();
                return true;
            }
            case MotionEvent.ACTION_CANCEL:
                editDragPointer = -1;
                editDragTarget = -1;
                invalidate();
                return true;
            default:
                return true;
        }
    }

    private void dragControl(int target, float x, float y) {
        GamepadLayout.Region home = homeLayout.regionFor(target);
        if (home == null) return;
        float u = layout.unit;
        offsets[target * 2] = (x - home.cx) / u;
        offsets[target * 2 + 1] = (y - home.cy) / u;
        rebuild();
    }

    private void applyPanel(int index) {
        switch (index) {
            case 0: scale = clamp(scale - 0.05f, 0.6f, 1.6f); rebuild(); break;
            case 1: scale = clamp(scale + 0.05f, 0.6f, 1.6f); rebuild(); break;
            case 2: opacity = clamp(opacity - 0.1f, 0.3f, 1f); invalidate(); break;
            case 3: opacity = clamp(opacity + 0.1f, 0.3f, 1f); invalidate(); break;
            case 4: compact = !compact; rebuild(); break;
            case 5: java.util.Arrays.fill(offsets, 0f); rebuild(); break;
            case 6: setEditMode(false); break;
            default: break;
        }
    }

    private int panelHit(float x, float y) {
        for (int i = 0; i < PANEL_LABELS; i++) {
            if (panelRects[i] != null && panelRects[i].contains(x, y)) return i;
        }
        return -1;
    }

    // =====================================================================
    // Drawing
    // =====================================================================

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        if (layout == null) return;

        boolean animating = false;
        animating |= drawStick(canvas, leftStick, layout.leftStick);
        animating |= drawStick(canvas, rightStick, layout.rightStick);
        drawDpad(canvas, layout.dpad);

        for (GamepadLayout.Region r : layout.regions) {
            if (r.kind == GamepadLayout.KIND_BUTTON) drawRoundButton(canvas, r);
            else if (r.kind == GamepadLayout.KIND_PILL) drawPill(canvas, r);
        }

        drawGear(canvas);
        if (editMode) drawPanel(canvas);

        if (animating) postInvalidateOnAnimation();
    }

    private boolean drawStick(Canvas canvas, Stick s, GamepadLayout.Region r) {
        if (!s.active) {
            s.ax += (0f - s.ax) * 0.28f; // ease back to centre
            s.ay += (0f - s.ay) * 0.28f;
        }
        float baseR = r.visR();
        float knobR = baseR * GamepadLayout.KNOB_FRACTION;

        fill.setColor(withAlpha(FILL_IDLE));
        stroke.setColor(withAlpha(STROKE_IDLE));
        stroke.setStrokeWidth(Math.max(2f, baseR * 0.045f));
        canvas.drawCircle(r.cx, r.cy, baseR, fill);
        canvas.drawCircle(r.cx, r.cy, baseR, stroke);

        float kx = r.cx + clamp(s.ax, -baseR, baseR);
        float ky = r.cy + clamp(s.ay, -baseR, baseR);
        boolean hot = s.active && (Math.abs(s.ax) > 1f || Math.abs(s.ay) > 1f);
        fill.setColor(withAlpha(hot ? FILL_PRESSED : 0x73000000));
        stroke.setColor(withAlpha(hot ? STROKE_PRESSED : STROKE_IDLE));
        canvas.drawCircle(kx, ky, knobR, fill);
        canvas.drawCircle(kx, ky, knobR, stroke);

        return !s.active && (Math.abs(s.ax) > 0.5f || Math.abs(s.ay) > 0.5f);
    }

    private void drawRoundButton(Canvas canvas, GamepadLayout.Region r) {
        boolean down = pressed[r.target];
        float visR = r.visR();
        fill.setColor(withAlpha(down ? FILL_PRESSED : FILL_IDLE));
        stroke.setColor(withAlpha(down ? STROKE_PRESSED : STROKE_IDLE));
        stroke.setStrokeWidth(Math.max(2f, visR * 0.06f));
        canvas.drawCircle(r.cx, r.cy, visR, fill);
        canvas.drawCircle(r.cx, r.cy, visR, stroke);

        text.setColor(withAlpha(down ? TEXT_PRESSED : TEXT_IDLE));
        text.setTextSize(visR * 1.02f);
        drawCentered(canvas, r.label, r.cx, r.cy);
    }

    private void drawPill(Canvas canvas, GamepadLayout.Region r) {
        boolean down = pressed[r.target];
        float hw = r.visHalfW(), hh = r.visHalfH();
        tmpRect.set(r.cx - hw, r.cy - hh, r.cx + hw, r.cy + hh);
        fill.setColor(withAlpha(down ? FILL_PRESSED : FILL_IDLE));
        stroke.setColor(withAlpha(down ? STROKE_PRESSED : STROKE_IDLE));
        stroke.setStrokeWidth(Math.max(2f, hh * 0.12f));
        canvas.drawRoundRect(tmpRect, hh, hh, fill);
        canvas.drawRoundRect(tmpRect, hh, hh, stroke);

        text.setColor(withAlpha(down ? TEXT_PRESSED : TEXT_IDLE));
        text.setTextSize(hh * 0.92f);
        drawCentered(canvas, r.label, r.cx, r.cy);
    }

    private void drawDpad(Canvas canvas, GamepadLayout.Region r) {
        float reach = r.visR();
        float half = reach * 0.40f;
        float corner = half;
        stroke.setStrokeWidth(Math.max(2f, reach * 0.05f));

        fill.setColor(withAlpha(FILL_IDLE));
        stroke.setColor(withAlpha(STROKE_IDLE));
        tmpRect.set(r.cx - half, r.cy - reach, r.cx + half, r.cy + reach);
        canvas.drawRoundRect(tmpRect, corner, corner, fill);
        canvas.drawRoundRect(tmpRect, corner, corner, stroke);
        tmpRect.set(r.cx - reach, r.cy - half, r.cx + reach, r.cy + half);
        canvas.drawRoundRect(tmpRect, corner, corner, fill);
        canvas.drawRoundRect(tmpRect, corner, corner, stroke);

        boolean up = dpadActive && dpadNy < -0.5f;
        boolean down = dpadActive && dpadNy > 0.5f;
        boolean left = dpadActive && dpadNx < -0.5f;
        boolean right = dpadActive && dpadNx > 0.5f;
        fill.setColor(withAlpha(FILL_PRESSED));
        stroke.setColor(withAlpha(STROKE_PRESSED));
        if (up) {
            tmpRect.set(r.cx - half, r.cy - reach, r.cx + half, r.cy);
            canvas.drawRoundRect(tmpRect, corner, corner, fill);
            canvas.drawRoundRect(tmpRect, corner, corner, stroke);
        }
        if (down) {
            tmpRect.set(r.cx - half, r.cy, r.cx + half, r.cy + reach);
            canvas.drawRoundRect(tmpRect, corner, corner, fill);
            canvas.drawRoundRect(tmpRect, corner, corner, stroke);
        }
        if (left) {
            tmpRect.set(r.cx - reach, r.cy - half, r.cx, r.cy + half);
            canvas.drawRoundRect(tmpRect, corner, corner, fill);
            canvas.drawRoundRect(tmpRect, corner, corner, stroke);
        }
        if (right) {
            tmpRect.set(r.cx, r.cy - half, r.cx + reach, r.cy + half);
            canvas.drawRoundRect(tmpRect, corner, corner, fill);
            canvas.drawRoundRect(tmpRect, corner, corner, stroke);
        }
    }

    private void drawGear(Canvas canvas) {
        stroke.setColor(withAlpha(editMode ? ACCENT : STROKE_IDLE));
        stroke.setStrokeWidth(Math.max(2f, gearR * 0.16f));
        stroke.setStrokeCap(Paint.Cap.ROUND);
        canvas.drawCircle(gearX, gearY, gearR * 0.55f, stroke);
        for (int i = 0; i < 8; i++) {
            double a = Math.PI * 2 * i / 8;
            float x1 = gearX + (float) Math.cos(a) * gearR * 0.58f;
            float y1 = gearY + (float) Math.sin(a) * gearR * 0.58f;
            float x2 = gearX + (float) Math.cos(a) * gearR;
            float y2 = gearY + (float) Math.sin(a) * gearR;
            canvas.drawLine(x1, y1, x2, y2, stroke);
        }
        stroke.setStrokeCap(Paint.Cap.BUTT);
    }

    private void drawPanel(Canvas canvas) {
        for (int i = 0; i < PANEL_LABELS; i++) {
            RectF rr = panelRects[i];
            if (rr == null) continue;
            float rad = rr.height() * 0.5f;
            fill.setColor(withAlpha(PANEL_FILL));
            stroke.setColor(withAlpha(STROKE_IDLE));
            stroke.setStrokeWidth(Math.max(2f, rr.height() * 0.06f));
            canvas.drawRoundRect(rr, rad, rad, fill);
            canvas.drawRoundRect(rr, rad, rad, stroke);
            text.setColor(withAlpha(PANEL_TEXT));
            text.setTextSize(rr.height() * 0.52f);
            drawCentered(canvas, PANEL_TEXT_LABELS[i], rr.centerX(), rr.centerY());
        }
        if (editDragTarget >= 0) {
            GamepadLayout.Region r = layout.regionFor(editDragTarget);
            if (r != null) {
                stroke.setColor(withAlpha(ACCENT));
                stroke.setStrokeWidth(Math.max(2f, layout.unit * 0.004f));
                if (r.kind == GamepadLayout.KIND_PILL) {
                    float hw = r.visHalfW(), hh = r.visHalfH();
                    tmpRect.set(r.cx - hw, r.cy - hh, r.cx + hw, r.cy + hh);
                    canvas.drawRoundRect(tmpRect, hh, hh, stroke);
                } else {
                    canvas.drawCircle(r.cx, r.cy, r.visR(), stroke);
                }
            }
        }
    }

    // ---- small drawing helpers ------------------------------------------

    private void drawCentered(Canvas canvas, String s, float cx, float cy) {
        if (s == null) return;
        Paint.FontMetrics fm = text.getFontMetrics();
        float baseline = cy - (fm.ascent + fm.descent) * 0.5f;
        canvas.drawText(s, cx, baseline, text);
    }

    private static float clamp(float v, float lo, float hi) {
        return v < lo ? lo : (v > hi ? hi : v);
    }

    private int withAlpha(int argb) {
        int a = (int) (((argb >>> 24) & 0xFF) * opacity);
        if (a < 0) a = 0;
        if (a > 255) a = 255;
        return (a << 24) | (argb & 0x00FFFFFF);
    }

    // =====================================================================
    // Per-stick runtime state
    // =====================================================================

    private static final class Stick {
        final int target;
        int pointer = -1;
        boolean active;
        float nx, ny;  // normalized axis target (y up-positive)
        float ax, ay;  // displayed knob displacement in px (screen y down)

        Stick(int target) { this.target = target; }
    }
}
