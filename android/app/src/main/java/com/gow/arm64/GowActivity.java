package com.gow.arm64;

import android.app.Activity;
import android.os.Bundle;
import android.view.InputDevice;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.util.Log;
import java.io.File;

/**
 * Host activity. Presents a SurfaceView (the ANativeWindow the Vulkan swapchain
 * will bind to) with the on-screen {@link GamepadOverlayView} layered on top,
 * and drives the native runtime. The game's own files are read at runtime from
 * /storage/emulated/0/god/; the recompiled game library is loaded from the
 * app's private files directory.
 *
 * <p>Touch routing: the overlay is a non-focusable transparent view over the
 * SurfaceView inside a {@link FrameLayout}. The overlay claims the gesture so
 * multi-touch is order-independent, drives the gamepad model for touches that
 * land on a control, and forwards every other pointer to the game unchanged
 * (normalized {@code [0,1]}) so nothing is dropped.
 */
public class GowActivity extends Activity implements SurfaceHolder.Callback {
    private static final String TAG = "GoW";
    static { System.loadLibrary("gow_runtime"); }

    private native int nativeInit(String gameDir);
    private native int nativeLoadDriver(String driversDir, String privDir);
    private native int nativeLoadAndRun(String gameDir, String privDir,
                                        long imageBase, long imageSize, int entryRva);
    private native void nativeSurfaceCreated(Surface surface);
    private native void nativeSurfaceChanged(int w, int h);
    private native void nativeSurfaceDestroyed();
    private native void nativeKey(int source, int keycode, boolean down);
    private native void nativeMotion(int axis, float value);
    private native void nativeTouch(int pointerId, float x, float y, boolean down);
    private native void nativeClearTouchLayout();

    private SurfaceView surfaceView;
    private GamepadOverlayView gamepad;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        surfaceView = new SurfaceView(this);
        surfaceView.getHolder().addCallback(this);

        // The Java overlay owns touch geometry; stop the Rust fallback layout
        // from re-interpreting pass-through touches against a different layout.
        // Guarded so a runtime .so built before this entry point still starts.
        try {
            nativeClearTouchLayout();
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "nativeClearTouchLayout missing (stale runtime .so): " + e.getMessage());
        }

        // The overlay drives runtime/src/input.rs through the existing
        // nativeMotion / nativeKey entry points (gamepad axes + gamepad
        // keycodes); touches that miss every control are forwarded to the game
        // as normalized touch coordinates. See docs/GAMEPAD.md for the mapping.
        gamepad = new GamepadOverlayView(this, new GamepadOverlayView.InputSink() {
            @Override public void axis(int axis, float value) {
                nativeMotion(axis, value);
            }
            @Override public void key(int source, int keyCode, boolean down) {
                nativeKey(source, keyCode, down);
            }
            @Override public void gameTouch(int pointerId, float x, float y, boolean down) {
                nativeTouch(pointerId, x, y, down);
            }
        });

        // SurfaceView fills the frame; the overlay is drawn on top and lets
        // non-control touches through to the game.
        FrameLayout root = new FrameLayout(this);
        root.setMotionEventSplittingEnabled(true);
        root.addView(surfaceView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        root.addView(gamepad, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        setContentView(root);

        File godDir = new File("/storage/emulated/0/god");
        Log.i(TAG, "game dir exists: " + godDir.exists() + " " + godDir.getAbsolutePath());
        nativeInit(godDir.getAbsolutePath());

        // Constants from the PE header of GoW.exe (image base 0x140000000,
        // entry RVA 0x3ec680, image size 0x519f000). The recompiled-game .so
        // must be placed in the game dir; it is staged and loaded here.
        final long IMAGE_BASE = 0x140000000L;
        final long IMAGE_SIZE = 0x519f000L;
        final int ENTRY_RVA = 0x3ec680;
        String privDir = getApplicationInfo().nativeLibraryDir;

        // Load a custom Vulkan driver (Turnip) from the drivers folder before
        // bringing up Vulkan, so the swapchain uses it.
        String driversDir = new java.io.File(godDir, "drivers").getAbsolutePath();
        int dr = nativeLoadDriver(driversDir, privDir);
        Log.i(TAG, "nativeLoadDriver returned " + dr);

        int rc = nativeLoadAndRun(godDir.getAbsolutePath(), privDir, IMAGE_BASE, IMAGE_SIZE, ENTRY_RVA);
        Log.i(TAG, "nativeLoadAndRun returned " + rc);
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        nativeSurfaceCreated(holder.getSurface());
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int w, int h) {
        nativeSurfaceChanged(w, h);
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        nativeSurfaceDestroyed();
    }

    @Override
    public boolean onKeyDown(int keyCode, android.view.KeyEvent event) {
        nativeKey(event.getSource(), keyCode, true);
        return true;
    }

    @Override
    public boolean onKeyUp(int keyCode, android.view.KeyEvent event) {
        nativeKey(event.getSource(), keyCode, false);
        return true;
    }

    @Override
    public boolean onGenericMotionEvent(android.view.MotionEvent event) {
        // Physical joystick axes only; the on-screen sticks do NOT come through
        // here (they are fed straight to the input layer from the overlay).
        if ((event.getSource() & InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
                || (event.getSource() & InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD) {
            for (int axis = android.view.MotionEvent.AXIS_X; axis <= android.view.MotionEvent.AXIS_RZ; axis++) {
                nativeMotion(axis, event.getAxisValue(axis));
            }
            return true;
        }
        return super.onGenericMotionEvent(event);
    }
}
