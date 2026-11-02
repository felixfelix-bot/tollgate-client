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

    /**
     * Legacy ABI tombstone: always returns not-attempted with code
     * payment-integration-unavailable. No argument is used; no IO or spending.
     * This is also disabled in hardened feature builds.
     */
    public static native String buy(String gateUrl, String token, String attemptId, String journalPath);

    /** Display-only caps; does not enroll policy or authorize payment. */
    public static native String capsFor(String unit, double maxPerMinute, double dailyBudget);
}
