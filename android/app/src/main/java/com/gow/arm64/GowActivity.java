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
    private native int nativeRun(String libPath, long imageBase, long imageSize, int entryRva);
    private native void nativeSurfaceCreated(Surface surface);
    private native void nativeSurfaceChanged(int w, int h);
    private native void nativeSurfaceDestroyed();

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
}
