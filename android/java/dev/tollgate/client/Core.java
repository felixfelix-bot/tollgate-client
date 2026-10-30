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
     * Spend one note at a gateway. Returns JSON: {"state":"granted",…} or
     * {"error":…}. `attemptId` keys the journal, so the same attempt is refused
     * rather than paid twice.
     */
    public static native String buy(String gateUrl, String token, String attemptId, String journalPath);

    /** Echo the caps as the core will apply them, so the UI shows the truth. */
    public static native String capsFor(String unit, double maxPerMinute, double dailyBudget);
}
