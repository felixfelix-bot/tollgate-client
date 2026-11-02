package dev.tollgate.client;

/**
 * The only bridge to Rust. Every call takes and returns JSON, so the
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

    /**
     * Ask the gate to create a Lightning quote. Returns
     * {@code {"quote","invoice","amount","mint_url","expiry"}} or {@code {"error":…}}.
     * The BOLT11 in {@code invoice} is what the customer pays — no Cashu token
     * exists on this device, so there is nothing here to lose if it is lost.
     */
    public static native String requestInvoice(String gateUrl, long amountSat, String mintUrl, long timeoutMs);

    /**
     * Poll a quote for access. {@code {"state","access_granted","allotment","metric"}}.
     * {@code access_granted:true} is the only success signal; a
     * {@code paid-but-not-granted} answer means retry, never pay again.
     */
    public static native String pollInvoice(String gateUrl, String quote, long timeoutMs);
}
