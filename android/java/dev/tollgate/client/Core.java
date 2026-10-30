package dev.tollgate.client;

/**
 * The only bridge to Rust. Two calls, both taking and returning JSON, so the
 * Kotlin/Java side never has to agree with Rust on a struct layout.
 */
public final class Core {
    static {
        System.loadLibrary("tollgate_client_core");
    }

    private Core() {}

    /** Probe a gateway and describe what it sells. Returns JSON, or {"error":…}. */
    public static native String describe(String gateUrl);

    /** Echo the caps as the core will apply them, so the UI shows the truth. */
    public static native String capsFor(String unit, double maxPerMinute, double dailyBudget);
}
