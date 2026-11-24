package com.gow.arm64;

/**
 * Pure-Java geometry for the on-screen virtual gamepad.
 *
 * <p>All math is in <b>pixels</b> and depends only on the view size, the safe
 * insets (notch / system bars), a user scale factor and optional per-control
 * drag offsets expressed as fractions of the unit. There are <b>no Android
 * imports</b>, so the geometry can be exercised from a plain {@code main()}
 * (see {@code android/tools/GamepadLayoutDemo.java}) and reasoned about without
 * a device.
 *
 * <p>The {@code unit} is {@code min(width, height)}; every control is sized as
 * a fraction of it, so the pad scales with the screen. {@link #compact} shrinks
 * controls for small displays and {@code scale} lets the player fine-tune the
 * size on top of that.
 *
 * <h2>Regions and hit priority</h2>
 * {@link #regions} is returned in <b>hit-priority order</b>: discrete controls
 * (face buttons, shoulders, triggers, start/back, D-pad) first, the two large
 * sticks last. This mirrors the ordering rule used by
 * {@code runtime/src/input.rs} ({@code TouchLayout::default_game}), so a press
 * that lands on a button is never stolen by the stick underneath it.
 *
 * <h2>Extents</h2>
 * Each {@link Region} stores <b>hit</b> half-extents ({@code rx,ry}, deliberately
 * a little larger than the drawn shape so the controls are easy to hit) plus a
 * {@code hitScale}; the shape actually drawn has half-extents
 * {@code rx/hitScale, ry/hitScale}. {@link Region#contains} uses the generous
 * hit shape; {@link Region#visR()} / {@code visHalfW/H} give the drawn size.
 *
 * <h2>Coordinate model</h2>
 * Everything here is in view pixels with the origin at the top-left. The
 * overlay converts to the input layer's normalized {@code [0,1]} space only
 * where the input layer needs it.
 */
public final class GamepadLayout {

    // ------------------------------------------------------------------
    // Control ids. Stable integers: they are persisted in SharedPreferences
    // as drag-offset keys, so do not renumber them.
    // ------------------------------------------------------------------
    public static final int LEFT_STICK = 0;
    public static final int RIGHT_STICK = 1;
    public static final int DPAD = 2;
    public static final int A = 3;
    public static final int B = 4;
    public static final int X = 5;
    public static final int Y = 6;
    public static final int LB = 7;
    public static final int RB = 8;
    public static final int LT = 9;
    public static final int RT = 10;
    public static final int START = 11;
    public static final int BACK = 12;
    public static final int TARGET_COUNT = 13;

    // Region shapes (for drawing + hit testing).
    public static final int KIND_STICK = 0;
    public static final int KIND_BUTTON = 1;
    public static final int KIND_DPAD = 2;
    public static final int KIND_PILL = 3;

    // ------------------------------------------------------------------
    // Sizing constants, all fractions of the unit (min(width,height)).
    // ------------------------------------------------------------------
    private static final float STICK_R = 0.150f;      // visual base radius
    private static final float STICK_KNOB_R = 0.062f; // thumb cap radius
    private static final float STICK_HIT = 1.12f;     // hit radius = visual * this
    private static final float BUTTON_R = 0.050f;
    private static final float BUTTON_HIT = 1.40f;
    private static final float BUTTON_SPREAD = 0.090f; // diamond half-diagonal
    private static final float DPAD_R = 0.108f;        // arm reach from centre
    private static final float DPAD_HIT = 1.22f;
    private static final float PILL_W = 0.150f, PILL_H = 0.064f;
    private static final float PILL_HIT = 1.30f;
    private static final float SMALL_PILL_W = 0.100f, SMALL_PILL_H = 0.048f;
    /** Size multiplier applied in compact mode. */
    public static final float COMPACT = 0.85f;

    /** Knob radius as a fraction of the stick radius (shared with the view). */
    public static final float KNOB_FRACTION = STICK_KNOB_R / STICK_R;

    /** One hit/draw region. See the class doc for the extent convention. */
    public static final class Region {
        public final int target;
        public final int kind;
        public final float cx, cy;        // displayed centre (clamped on screen), px
        public final float rawCx, rawCy;  // requested centre before the on-screen clamp, px
        public final float rx, ry;        // HIT half extents, px
        public final float hitScale;      // drawn = hit / hitScale
        public final String label;        // may be null

        Region(int target, int kind, float cx, float cy, float rawCx, float rawCy,
               float rx, float ry, float hitScale, String label) {
            this.target = target;
            this.kind = kind;
            this.cx = cx;
            this.cy = cy;
            this.rawCx = rawCx;
            this.rawCy = rawCy;
            this.rx = rx;
            this.ry = ry;
            this.hitScale = hitScale;
            this.label = label;
        }

        /** Visual radius (sticks / buttons / D-pad). */
        public float visR() { return rx / hitScale; }

        /** Visual half-width of a pill. */
        public float visHalfW() { return rx / hitScale; }
        /** Visual half-height of a pill. */
        public float visHalfH() { return ry / hitScale; }

        /** True when {@code (x,y)} is inside this region's (generous) hit shape. */
        public boolean contains(float x, float y) {
            if (kind == KIND_PILL) {
                // Stadium / rounded rectangle: corner radius = shorter half extent.
                float hw = rx, hh = ry;
                float corner = Math.min(hw, hh);
                float dx = Math.max(Math.abs(x - cx) - (hw - corner), 0f);
                float dy = Math.max(Math.abs(y - cy) - (hh - corner), 0f);
                return dx * dx + dy * dy <= corner * corner;
            }
            float dx = (x - cx) / Math.max(rx, 1e-3f);
            float dy = (y - cy) / Math.max(ry, 1e-3f);
            return dx * dx + dy * dy <= 1f;
        }

        @Override public String toString() {
            return String.format("%-11s centre=(%7.1f,%7.1f) hit=(%6.1f,%6.1f) vis=%.1f",
                    label != null ? label : nameOf(target), cx, cy, rx, ry, visR());
        }
    }

    public final int width, height;
    public final float unit;
    public final float scale;
    public final boolean compact;
    /** Hit-priority order: discrete controls first, sticks last. */
    public final Region[] regions;
    public final Region leftStick, rightStick, dpad;

    private GamepadLayout(int width, int height, float unit, float scale, boolean compact,
                          Region[] regions, Region leftStick, Region rightStick, Region dpad) {
        this.width = width;
        this.height = height;
        this.unit = unit;
        this.scale = scale;
        this.compact = compact;
        this.regions = regions;
        this.leftStick = leftStick;
        this.rightStick = rightStick;
        this.dpad = dpad;
    }

    /**
     * Compute the geometry.
     *
     * @param width,height  view size in px
     * @param insetL,insetT,insetR,insetB  safe insets in px (notch / bars)
     * @param scale         user size multiplier (&gt;0, typically 0.8..1.25)
     * @param compact       shrink controls for small screens
     * @param offsets       per-target drag offsets, {@code [target*2]=dx},
     *                      {@code [target*2+1]=dy}, in fractions of the unit;
     *                      may be null or short (treated as zero)
     */
    public static GamepadLayout compute(int width, int height,
                                        int insetL, int insetT, int insetR, int insetB,
                                        float scale, boolean compact, float[] offsets) {
        float[] off = (offsets != null && offsets.length >= TARGET_COUNT * 2)
                ? offsets : new float[TARGET_COUNT * 2];
        if (scale <= 0f) scale = 1f;

        float u = Math.min(width, height);
        float s = scale * (compact ? COMPACT : 1f);

        float margin = (compact ? 0.030f : 0.055f) * u;
        float l = insetL + margin;
        float t = insetT + margin;
        float r = width - insetR - margin;
        float b = height - insetB - margin;
        if (r < l) r = l;
        if (b < t) b = t;

        float stickR = STICK_R * u * s;               // visual
        float stickHit = stickR * STICK_HIT;          // hit
        float btnR = BUTTON_R * u * s;                // visual
        float btnHit = btnR * BUTTON_HIT;             // hit
        float btnSpread = BUTTON_SPREAD * u * s;
        float dpadR = DPAD_R * u * s;                 // visual
        float dpadHit = dpadR * DPAD_HIT;             // hit
        float pillHW = PILL_W * 0.5f * u * s;         // visual half width
        float pillHH = PILL_H * 0.5f * u * s;         // visual half height
        float sPillHW = SMALL_PILL_W * 0.5f * u * s;
        float sPillHH = SMALL_PILL_H * 0.5f * u * s;
        float gap = 0.020f * u * s;

        Region[] by = new Region[TARGET_COUNT];

        // ---- top corners: triggers (outermost) + shoulders (inward) ----
        float topY = t + pillHH;
        float ltX = l + pillHW;
        float lbX = ltX + pillHW * 2f + gap;
        float rtX = r - pillHW;
        float rbX = rtX - pillHW * 2f - gap;
        by[LT] = mk(LT, KIND_PILL, "LT", ltX, topY, pillHW * PILL_HIT, pillHH * PILL_HIT, PILL_HIT,
                off, u, l, t, r, b);
        by[LB] = mk(LB, KIND_PILL, "LB", lbX, topY, pillHW * PILL_HIT, pillHH * PILL_HIT, PILL_HIT,
                off, u, l, t, r, b);
        by[RT] = mk(RT, KIND_PILL, "RT", rtX, topY, pillHW * PILL_HIT, pillHH * PILL_HIT, PILL_HIT,
                off, u, l, t, r, b);
        by[RB] = mk(RB, KIND_PILL, "RB", rbX, topY, pillHW * PILL_HIT, pillHH * PILL_HIT, PILL_HIT,
                off, u, l, t, r, b);

        // ---- the two thumbsticks live in the bottom corners ----
        Region leftStick = mkStick(LEFT_STICK, l + stickR, b - stickR, stickHit, off, u, l, t, r, b);
        Region rightStick = mkStick(RIGHT_STICK, r - stickR, b - stickR, stickHit, off, u, l, t, r, b);
        by[LEFT_STICK] = leftStick;
        by[RIGHT_STICK] = rightStick;

        // ---- upper row for the D-pad (left) and face diamond (right) ----
        float upperY = b - 2f * stickR - 0.180f * u * s;
        // keep clear of the shoulder row on very short screens
        upperY = Math.max(upperY, t + pillHH * 2f + gap + dpadR + gap);
        float dpadX = l + dpadR;
        float faceX = r - btnR - btnSpread;
        by[Y] = mk(Y, KIND_BUTTON, "Y", faceX, upperY - btnSpread, btnHit, btnHit, BUTTON_HIT,
                off, u, l, t, r, b);
        by[A] = mk(A, KIND_BUTTON, "A", faceX, upperY + btnSpread, btnHit, btnHit, BUTTON_HIT,
                off, u, l, t, r, b);
        by[X] = mk(X, KIND_BUTTON, "X", faceX - btnSpread, upperY, btnHit, btnHit, BUTTON_HIT,
                off, u, l, t, r, b);
        by[B] = mk(B, KIND_BUTTON, "B", faceX + btnSpread, upperY, btnHit, btnHit, BUTTON_HIT,
                off, u, l, t, r, b);
        by[DPAD] = mk(DPAD, KIND_DPAD, "D", dpadX, upperY, dpadHit, dpadHit, DPAD_HIT,
                off, u, l, t, r, b);
        Region dpad = by[DPAD];

        // ---- centre: BACK / START, low and centred ----
        float cY = b - sPillHH;
        float cX = width * 0.5f;
        by[BACK] = mk(BACK, KIND_PILL, "BACK", cX - (sPillHW + gap), cY,
                sPillHW * PILL_HIT, sPillHH * PILL_HIT, PILL_HIT, off, u, l, t, r, b);
        by[START] = mk(START, KIND_PILL, "START", cX + (sPillHW + gap), cY,
                sPillHW * PILL_HIT, sPillHH * PILL_HIT, PILL_HIT, off, u, l, t, r, b);

        // Hit-priority order: everything discrete first, the big sticks last.
        Region[] order = {
                by[A], by[B], by[X], by[Y], by[LB], by[RB], by[LT], by[RT],
                by[START], by[BACK], dpad, leftStick, rightStick,
        };
        return new GamepadLayout(width, height, u, scale, compact, order, leftStick, rightStick, dpad);
    }

    private static Region mkStick(int target, float cx, float cy, float hit,
                                  float[] off, float u, float l, float t, float r, float b) {
        return mk(target, KIND_STICK, null, cx, cy, hit, hit, STICK_HIT, off, u, l, t, r, b);
    }

    private static Region mk(int target, int kind, String label, float cx, float cy,
                             float rx, float ry, float hitScale,
                             float[] off, float u, float l, float t, float r, float b) {
        // Clamp by the DRAWN extents so the visible shape stays on screen; the
        // (larger) hit shape may protrude slightly past the margin.
        float hx = rx / hitScale, hy = ry / hitScale;
        float rawX = cx + off[target * 2] * u;
        float rawY = cy + off[target * 2 + 1] * u;
        float x = clampCenter(rawX, hx, l, r);
        float y = clampCenter(rawY, hy, t, b);
        return new Region(target, kind, x, y, rawX, rawY, rx, ry, hitScale, label);
    }

    /** Keep a control's <em>drawn</em> box inside {@code [lo,hi]}. */
    private static float clampCenter(float c, float half, float lo, float hi) {
        if (hi - lo <= 2f * half) return (lo + hi) * 0.5f;
        return Math.max(lo + half, Math.min(hi - half, c));
    }

    /** First region containing {@code (x,y)} in hit-priority order, or null. */
    public Region hitTest(float x, float y) {
        for (Region rgn : regions) {
            if (rgn.contains(x, y)) return rgn;
        }
        return null;
    }

    /** The region for a specific control id (identity within {@link #regions}). */
    public Region regionFor(int target) {
        for (Region rgn : regions) {
            if (rgn.target == target) return rgn;
        }
        return null;
    }

    /** True when {@code target} is one of the two analog sticks. */
    public static boolean isStick(int target) {
        return target == LEFT_STICK || target == RIGHT_STICK;
    }

    public static String nameOf(int target) {
        switch (target) {
            case LEFT_STICK: return "LEFT_STICK";
            case RIGHT_STICK: return "RIGHT_STICK";
            case DPAD: return "DPAD";
            case A: return "A";
            case B: return "B";
            case X: return "X";
            case Y: return "Y";
            case LB: return "LB";
            case RB: return "RB";
            case LT: return "LT";
            case RT: return "RT";
            case START: return "START";
            case BACK: return "BACK";
            default: return "?" + target;
        }
    }
}
