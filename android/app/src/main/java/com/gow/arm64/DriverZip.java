package com.gow.arm64;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

/**
 * Extracts an adrenotools-style driver package (a zip with {@code meta.json} and
 * a driver {@code .so}) into a directory, flattening paths to the basename so a
 * hostile archive cannot escape the target directory.
 */
final class DriverZip {
    private static final long MAX_TOTAL = 256L * 1024 * 1024; // 256 MB safety cap

    private DriverZip() {}

    static void extract(File zip, File outDir) throws IOException {
        if (!outDir.exists() && !outDir.mkdirs()) {
            throw new IOException("cannot create " + outDir);
        }
        long total = 0;
        try (ZipFile zf = new ZipFile(zip)) {
            java.util.Enumeration<? extends ZipEntry> en = zf.entries();
            while (en.hasMoreElements()) {
                ZipEntry e = en.nextElement();
                if (e.isDirectory()) continue;
                String base = new File(e.getName()).getName();
                if (base.isEmpty() || base.equals(".") || base.equals("..")) continue;
                // Keep only the driver library and its metadata.
                String lower = base.toLowerCase();
                if (!lower.endsWith(".so") && !lower.equals("meta.json")) continue;
                total += Math.max(e.getSize(), 0);
                if (total > MAX_TOTAL) throw new IOException("package too large");
                File dst = new File(outDir, base);
                try (InputStream in = zf.getInputStream(e);
                     OutputStream out = new FileOutputStream(dst)) {
                    byte[] buf = new byte[65536];
                    int n;
                    while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
                }
            }
        }
    }

    /**
     * The driver library to load: {@code meta.json}'s {@code libraryName} when
     * present, else the single {@code .so} in the directory. Null if neither.
     */
    static String libraryName(File dir) {
        File meta = new File(dir, "meta.json");
        if (meta.exists()) {
            try (InputStream in = new java.io.FileInputStream(meta)) {
                byte[] buf = new byte[(int) Math.min(meta.length(), 8192)];
                int n = in.read(buf);
                String s = new String(buf, 0, Math.max(n, 0));
                String lib = jsonString(s, "libraryName");
                if (lib != null) {
                    File f = new File(dir, new File(lib).getName());
                    if (f.exists()) return f.getName();
                }
            } catch (Exception ignored) {
            }
        }
        File[] sos = dir.listFiles((d, name) -> name.toLowerCase().endsWith(".so"));
        if (sos != null && sos.length == 1) return sos[0].getName();
        if (sos != null && sos.length > 1) {
            // Prefer a freedreno/turnip-looking name, else the first.
            for (File f : sos) {
                if (f.getName().toLowerCase().contains("freedreno")) return f.getName();
            }
            return sos[0].getName();
        }
        return null;
    }

    private static String jsonString(String json, String key) {
        int i = json.indexOf("\"" + key + "\"");
        if (i < 0) return null;
        int colon = json.indexOf(':', i);
        if (colon < 0) return null;
        int q1 = json.indexOf('"', colon + 1);
        if (q1 < 0) return null;
        int q2 = json.indexOf('"', q1 + 1);
        if (q2 < 0) return null;
        return json.substring(q1 + 1, q2);
    }
}