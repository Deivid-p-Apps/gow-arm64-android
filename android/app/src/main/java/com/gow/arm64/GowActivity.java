package com.gow.arm64;

import android.app.Activity;
import android.os.Bundle;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.util.Log;
import android.os.Environment;
import android.Manifest;
import android.content.pm.PackageManager;
import java.io.File;

/**
 * Host activity. Presents a SurfaceView (the ANativeWindow the Vulkan swapchain
 * will bind to) and drives the native runtime. The game's own files are read at
 * runtime from /storage/emulated/0/god/; the recompiled game library is loaded
 * from the app's private files directory.
 */
public class GowActivity extends Activity implements SurfaceHolder.Callback {
    private static final String TAG = "GoW";
    static { System.loadLibrary("gow_runtime"); }

    private native int nativeInit(String gameDir);
    private native int nativeLoadAndRun(String gameDir, String privDir,
                                        long imageBase, long imageSize, int entryRva);
    private native void nativeSurfaceCreated(Surface surface);
    private native void nativeSurfaceChanged(int w, int h);
    private native void nativeSurfaceDestroyed();
    private native void nativeKey(int source, int keycode, boolean down);
    private native void nativeMotion(int axis, float value);
    private native void nativeTouch(int pointerId, float x, float y, boolean down);

    private SurfaceView surfaceView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        surfaceView = new SurfaceView(this);
        surfaceView.getHolder().addCallback(this);
        setContentView(surfaceView);

        File godDir = new File("/storage/emulated/0/god");
        Log.i(TAG, "game dir exists: " + godDir.exists() + " " + godDir.getAbsolutePath());
        nativeInit(godDir.getAbsolutePath());

        // Constants from the PE header of GoW.exe (image base 0x140000000,
        // entry RVA 0x3ec680, image size 0x519f000). The recompiled-game .so
        // must be placed in the game dir; it is staged and loaded here.
        final long IMAGE_BASE = 0x140000000L;
        final long IMAGE_SIZE = 0x519f000L;
        final int ENTRY_RVA = 0x3ec680;
        String privDir = getFilesDir().getAbsolutePath();
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
        // Joystick axes.
        for (int axis = android.view.MotionEvent.AXIS_X; axis <= android.view.MotionEvent.AXIS_RZ; axis++) {
            nativeMotion(axis, event.getAxisValue(axis));
        }
        return true;
    }

    @Override
    public boolean onTouchEvent(android.view.MotionEvent event) {
        int action = event.getActionMasked();
        int idx = event.getActionIndex();
        int id = event.getPointerId(idx);
        switch (action) {
            case android.view.MotionEvent.ACTION_DOWN:
            case android.view.MotionEvent.ACTION_POINTER_DOWN:
                nativeTouch(id, event.getX(idx), event.getY(idx), true);
                break;
            case android.view.MotionEvent.ACTION_UP:
            case android.view.MotionEvent.ACTION_POINTER_UP:
                nativeTouch(id, event.getX(idx), event.getY(idx), false);
                break;
            case android.view.MotionEvent.ACTION_MOVE:
                for (int i = 0; i < event.getPointerCount(); i++) {
                    nativeTouch(event.getPointerId(i), event.getX(i), event.getY(i), true);
                }
                break;
        }
        return true;
    }
}
