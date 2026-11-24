package com.gow.arm64;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Launcher: pick a Vulkan driver (system or a Turnip package from
 * /storage/emulated/0/god/drivers or the Android file picker), then Start.
 *
 * The driver must live in app-private storage before dlopen (adrenotools /
 * Android both require it), so a chosen package is imported into
 * files/gpu_drivers/&lt;name&gt;/ and the choice is remembered.
 */
public class DriverSetupActivity extends Activity {
    private static final String TAG = "GoW.setup";
    private static final String PREFS = "gow";
    private static final String PREF_DRIVER = "selected_driver"; // "" = system
    private static final String DRIVER_ROOT = "gpu_drivers";

    private SharedPreferences prefs;
    private LinearLayout driverList;
    private TextView selectedLabel;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        prefs = getSharedPreferences(PREFS, MODE_PRIVATE);

        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setBackgroundColor(Color.parseColor("#101418"));
        int pad = dp(20);
        page.setPadding(pad, pad, pad, pad);

        TextView title = new TextView(this);
        title.setText("God of War — arm64");
        title.setTextColor(Color.WHITE);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 22);
        page.addView(title);

        TextView sub = new TextView(this);
        sub.setText("Driver Vulkan");
        sub.setTextColor(Color.parseColor("#8FA3AD"));
        sub.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        sub.setPadding(0, dp(4), 0, dp(14));
        page.addView(sub);

        selectedLabel = new TextView(this);
        selectedLabel.setTextColor(Color.parseColor("#5FA8B8"));
        selectedLabel.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        selectedLabel.setPadding(0, 0, 0, dp(10));
        page.addView(selectedLabel);

        ScrollView scroller = new ScrollView(this);
        driverList = new LinearLayout(this);
        driverList.setOrientation(LinearLayout.VERTICAL);
        scroller.addView(driverList);
        LinearLayout.LayoutParams lp =
                new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f);
        page.addView(scroller, lp);

        LinearLayout buttons = new LinearLayout(this);
        buttons.setOrientation(LinearLayout.HORIZONTAL);
        buttons.setPadding(0, dp(14), 0, 0);

        Button importBtn = styledButton("Importar ZIP…");
        importBtn.setOnClickListener(v -> pickDriverFile());
        buttons.addView(importBtn, weightParams());

        Button startBtn = styledButton("Iniciar jogo");
        startBtn.setOnClickListener(v -> startGame());
        buttons.addView(startBtn, weightParams());

        page.addView(buttons);
        setContentView(page);

        refresh();
    }

    // ---- UI helpers -------------------------------------------------------

    private int dp(int v) {
        return (int) TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, v, getResources().getDisplayMetrics());
    }

    private LinearLayout.LayoutParams weightParams() {
        LinearLayout.LayoutParams p =
                new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        p.setMargins(dp(6), 0, dp(6), 0);
        return p;
    }

    private Button styledButton(String text) {
        Button b = new Button(this);
        b.setText(text);
        b.setAllCaps(false);
        b.setTextColor(Color.WHITE);
        GradientDrawable bg = new GradientDrawable();
        bg.setColor(Color.parseColor("#2E7A8C"));
        bg.setCornerRadius(dp(12));
        b.setBackground(bg);
        b.setPadding(dp(14), dp(10), dp(14), dp(10));
        return b;
    }

    // ---- driver listing ---------------------------------------------------

    private File driversDir() {
        File d = new File(getFilesDir(), DRIVER_ROOT);
        if (!d.exists()) d.mkdirs();
        return d;
    }

    private void refresh() {
        driverList.removeAllViews();
        String sel = prefs.getString(PREF_DRIVER, "");
        selectedLabel.setText("Selecionado: " + (sel.isEmpty() ? "Driver do sistema" : sel));

        addRow("Driver do sistema", "", "Vulkan da GPU (Adreno)");
        for (File f : listDrivers()) {
            String meta = readDriverMeta(f);
            addRow(f.getName(), f.getAbsolutePath(), meta);
        }
        // Also offer packages found in the shared game folder (not yet imported).
        for (File z : listPackagesInGameFolder()) {
            addRow("(importar) " + z.getName(), z.getAbsolutePath(), "pacote em /god/drivers");
        }
    }

    private List<File> listDrivers() {
        List<File> out = new ArrayList<>();
        File[] fs = driversDir().listFiles();
        if (fs == null) return out;
        Arrays.sort(fs, (a, b) -> a.getName().compareToIgnoreCase(b.getName()));
        for (File f : fs) {
            if (f.isDirectory()) out.add(f);
        }
        return out;
    }

    private List<File> listPackagesInGameFolder() {
        List<File> out = new ArrayList<>();
        File d = new File("/storage/emulated/0/god/drivers");
        File[] fs = d.listFiles();
        if (fs == null) return out;
        Arrays.sort(fs, (a, b) -> a.getName().compareToIgnoreCase(b.getName()));
        for (File f : fs) {
            String n = f.getName().toLowerCase();
            if (f.isFile() && (n.endsWith(".zip") || n.endsWith(".adpkg"))) out.add(f);
        }
        return out;
    }

    private String readDriverMeta(File dir) {
        File meta = new File(dir, "meta.json");
        if (meta.exists()) {
            try (InputStream in = new java.io.FileInputStream(meta)) {
                byte[] buf = new byte[(int) Math.min(meta.length(), 4096)];
                int n = in.read(buf);
                String s = new String(buf, 0, Math.max(n, 0));
                String name = jsonString(s, "name");
                String ver = jsonString(s, "driverVersion");
                if (name != null || ver != null) {
                    return (name == null ? dir.getName() : name)
                            + (ver == null ? "" : " " + ver);
                }
            } catch (Exception ignored) {
            }
        }
        return "pacote de driver";
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

    private void addRow(String title, String path, String meta) {
        String sel = prefs.getString(PREF_DRIVER, "");
        String key = path.isEmpty() ? "" : new File(path).getName();
        boolean isSel = key.equals(sel) || (path.isEmpty() && sel.isEmpty());

        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.VERTICAL);
        GradientDrawable bg = new GradientDrawable();
        bg.setColor(isSel ? Color.parseColor("#1F3B44") : Color.parseColor("#182026"));
        bg.setCornerRadius(dp(10));
        bg.setStroke(dp(1), isSel ? Color.parseColor("#5FA8B8") : Color.parseColor("#243038"));
        row.setBackground(bg);
        row.setPadding(dp(14), dp(12), dp(14), dp(12));
        LinearLayout.LayoutParams rp =
                new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT);
        rp.setMargins(0, dp(5), 0, dp(5));
        row.setLayoutParams(rp);

        TextView t = new TextView(this);
        t.setText(title);
        t.setTextColor(Color.WHITE);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        row.addView(t);

        TextView m = new TextView(this);
        m.setText(meta);
        m.setTextColor(Color.parseColor("#7E929B"));
        m.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        row.addView(m);

        row.setOnClickListener(v -> onRowClicked(title, path));
        driverList.addView(row);
    }

    private void onRowClicked(String title, String path) {
        if (path.isEmpty()) {
            prefs.edit().putString(PREF_DRIVER, "").apply();
            refresh();
            return;
        }
        File f = new File(path);
        if (title.startsWith("(importar) ")) {
            importPackage(f);
            return;
        }
        // Already-imported driver: select it.
        prefs.edit().putString(PREF_DRIVER, f.getName()).apply();
        refresh();
    }

    // ---- importing ---------------------------------------------------------

    private void pickDriverFile() {
        // Prefer the system file picker; fall back to scanning /god/drivers.
        android.content.Intent i = new android.content.Intent(android.content.Intent.ACTION_OPEN_DOCUMENT);
        i.addCategory(android.content.Intent.CATEGORY_OPENABLE);
        i.setType("*/*");
        try {
            startActivityForResult(i, 42);
        } catch (Exception e) {
            Toast.makeText(this, "Sem seletor de arquivos; use /god/drivers", Toast.LENGTH_LONG).show();
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, android.content.Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == 42 && resultCode == RESULT_OK && data != null && data.getData() != null) {
            try (InputStream in = getContentResolver().openInputStream(data.getData())) {
                File tmp = new File(getCacheDir(), "driver_import.zip");
                try (FileOutputStream out = new FileOutputStream(tmp)) {
                    byte[] buf = new byte[65536];
                    int n;
                    while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
                }
                importPackage(tmp);
            } catch (Exception e) {
                Log.e(TAG, "import failed", e);
                Toast.makeText(this, "Falha ao ler o ZIP: " + e.getMessage(), Toast.LENGTH_LONG).show();
            }
        }
    }

    /** Extract an adrenotools/turnip zip (flat, basename only) into files/gpu_drivers/&lt;name&gt;. */
    private void importPackage(File zip) {
        try {
            String base = zip.getName().replaceAll("\\.(zip|adpkg)$", "");
            File dir = new File(driversDir(), base);
            if (dir.exists()) deleteRec(dir);
            dir.mkdirs();
            DriverZip.extract(zip, dir);
            String lib = DriverZip.libraryName(dir);
            if (lib == null) {
                Toast.makeText(this, "Pacote sem .so válido", Toast.LENGTH_LONG).show();
                deleteRec(dir);
                refresh();
                return;
            }
            prefs.edit().putString(PREF_DRIVER, dir.getName()).apply();
            Toast.makeText(this, "Driver importado: " + base, Toast.LENGTH_SHORT).show();
        } catch (Exception e) {
            Log.e(TAG, "importPackage", e);
            Toast.makeText(this, "Erro: " + e.getMessage(), Toast.LENGTH_LONG).show();
        }
        refresh();
    }

    private static void deleteRec(File f) {
        File[] c = f.listFiles();
        if (c != null) for (File x : c) deleteRec(x);
        //noinspection ResultOfMethodCallIgnored
        f.delete();
    }

    private void startGame() {
        String sel = prefs.getString(PREF_DRIVER, "");
        String libPath = "";
        if (!sel.isEmpty()) {
            File dir = new File(driversDir(), sel);
            String lib = DriverZip.libraryName(dir);
            if (lib != null) libPath = new File(dir, lib).getAbsolutePath();
        }
        android.content.Intent i = new android.content.Intent(this, GowActivity.class);
        i.putExtra(GowActivity.EXTRA_DRIVER_LIB, libPath);
        startActivity(i);
    }
}