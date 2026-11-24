import com.gow.arm64.GamepadLayout;

/**
 * Offline demo / smoke test for {@link GamepadLayout}. Run with a JDK:
 *
 * <pre>
 *   cd android
 *   mkdir -p out
 *   javac -d out app/src/main/java/com/gow/arm64/GamepadLayout.java tools/GamepadLayoutDemo.java
 *   java -cp out GamepadLayoutDemo
 * </pre>
 *
 * It prints the computed geometry for a few realistic screen sizes and exercises
 * the hit tester. Harmless (and skipped) when no JDK is present — the project
 * builds with Gradle, not this file.
 */
public final class GamepadLayoutDemo {

    public static void main(String[] args) {
        System.out.println("GamepadLayout demo\n");
        // 1080x2400 landscape-ish view (the test device), plus a small phone and
        // a tablet. Insets model a notch on one edge.
        check(2400, 1080, 0, 40, 120, 0, 1.0f, false, "Edge 30 Fusion (landscape, notch)");
        check(1920, 1080, 0, 0, 0, 0, 1.0f, false, "1920x1080 default");
        check(1280, 720, 0, 24, 48, 0, 1.0f, true, "small phone, compact");
        check(2560, 1600, 0, 0, 0, 0, 1.15f, false, "tablet, scaled up");

        System.out.println("\nSanity checks:");
        sanity();
        System.out.println("\nOK");
    }

    private static void check(int w, int h, int il, int it, int ir, int ib,
                              float scale, boolean compact, String title) {
        GamepadLayout L = GamepadLayout.compute(w, h, il, it, ir, ib, scale, compact, null);
        System.out.printf("== %s  view=%dx%d insets=(%d,%d,%d,%d) scale=%.2f compact=%s%n",
                title, w, h, il, it, ir, ib, scale, compact);
        System.out.printf("   unit=%.1f  %d regions%n", L.unit, L.regions.length);
        for (GamepadLayout.Region r : L.regions) {
            System.out.println("   " + r);
        }
        System.out.println();
    }

    private static void sanity() {
        GamepadLayout L = GamepadLayout.compute(1920, 1080, 0, 0, 0, 0, 1.0f, false, null);

        // 1. Every control id must be present exactly once.
        for (int t = 0; t < GamepadLayout.TARGET_COUNT; t++) {
            require(L.regionFor(t) != null, "missing region for " + GamepadLayout.nameOf(t));
        }

        // 2. Control centres must be on screen.
        for (GamepadLayout.Region r : L.regions) {
            require(r.cx > 0 && r.cx < L.width, "centre x off screen: " + r);
            require(r.cy > 0 && r.cy < L.height, "centre y off screen: " + r);
        }

        // 3. Hit test at each centre returns that control.
        for (GamepadLayout.Region r : L.regions) {
            GamepadLayout.Region hit = L.hitTest(r.cx, r.cy);
            require(hit != null && hit.target == r.target,
                    "hit test at centre of " + GamepadLayout.nameOf(r.target) + " -> " + hit);
        }

        // 4. A tap in the dead centre of the screen hits nothing (pass-through).
        GamepadLayout.Region mid = L.hitTest(L.width / 2f, L.height / 2f);
        require(mid == null, "screen centre should be free, got " + mid);

        // 5. Sticks are in the bottom corners, face diamond on the right.
        require(L.leftStick.cx < L.width * 0.5f, "left stick should be on the left");
        require(L.rightStick.cx > L.width * 0.5f, "right stick should be on the right");
        require(L.leftStick.cy > L.height * 0.5f, "left stick should be low");
        require(L.regionFor(GamepadLayout.Y).cy < L.regionFor(GamepadLayout.A).cy, "Y above A");
        require(L.regionFor(GamepadLayout.X).cx < L.regionFor(GamepadLayout.B).cx, "X left of B");

        // 6. Sticks are the lowest priority (last) in the hit order.
        require(L.regions[L.regions.length - 1].kind == GamepadLayout.KIND_STICK,
                "last region should be a stick");

        System.out.println("   all sanity checks passed");
    }

    private static void require(boolean cond, String msg) {
        if (!cond) throw new AssertionError(msg);
    }
}
