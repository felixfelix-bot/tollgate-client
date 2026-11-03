package dev.tollgate.client;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.RouteInfo;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.net.InetAddress;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The smallest honest Android shell: find the gateway the device is routed
 * through, ask the core what it sells, show the answer. No polling loop yet, no
 * auto-buy — those need the network suggestions work, and a UI that lies about
 * what it did is worse than one that does less.
 */
public class MainActivity extends Activity {
    private final ExecutorService io = Executors.newSingleThreadExecutor();
    private final Handler ui = new Handler(Looper.getMainLooper());
    private TextView out;
    /** The buy button is disabled while a quote is outstanding: one attempt, one invoice. */
    private Button buy;
    /**
     * The quote being paid, if any.
     *
     * Mirrored into SharedPreferences, not just held in memory: if the Activity
     * is recreated (rotation, theme change, process death) after the user has
     * paid the displayed BOLT11, an instance-only field would forget the quote
     * and offer a *second* invoice for the same access — a real double charge.
     */
    private String pendingQuote;
    private String pendingGate;
    /** The BOLT11 the customer was told to pay, so a recreation can show it again. */
    private String pendingInvoice;
    /** The quote's price, kept so the resumed message reads the same as the first. */
    private String pendingAmount;
    /** True when onCreate restored a live quote, so it must not re-probe the gate. */
    private boolean restored;
    /** Where the user may override the mint; blank means "ask the gateway". */
    private android.widget.EditText mintField;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        int pad = (int) (16 * getResources().getDisplayMetrics().density);
        root.setPadding(pad, pad, pad, pad);

        out = new TextView(this);
        out.setTextSize(14f);
        out.setText("idle");

        Button scan = new Button(this);
        scan.setText("Find gateway and price it");
        scan.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                checkGateway();
            }
        });

        final android.widget.EditText token = new android.widget.EditText(this);
        token.setHint("automatic spending disabled — do not store real funds");
        token.setText(prefs().getString("token", ""));

        Button save = new Button(this);
        save.setText("Save note");
        save.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                prefs().edit().putString("token", token.getText().toString().trim()).apply();
                out.setText("note saved locally — automatic spending is disabled");
            }
        });

        Button service = new Button(this);
        service.setText("Start background watch");
        service.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startForegroundService(new Intent(MainActivity.this, TollgateService.class));
                out.setText("discovery-only watch requested (60 s); automatic spending disabled");
            }
        });

        buy = new Button(this);
        buy.setText("Buy access (Lightning)");
        buy.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                buyAccess();
            }
        });

        // Restore an outstanding quote before the first tap can replace it.
        pendingQuote = prefs().getString("pending_quote", null);
        pendingGate = prefs().getString("pending_gate", null);
        pendingInvoice = prefs().getString("pending_invoice", null);
        pendingAmount = prefs().getString("pending_amount", null);
        if (pendingQuote != null && !pendingQuote.isEmpty() && pendingGate != null) {
            restored = true;
            buy.setText("Check payment");
            // Restoring the button without the invoice it labels leaves the
            // customer with "Check payment" over a blank box and nothing to
            // pay — safe from a double charge, but unusable. Show the same
            // BOLT11 again; if the invoice was never stored, say so instead of
            // pretending, and let Discard quote clear the stale id.
            if (pendingInvoice != null && !pendingInvoice.isEmpty()) {
                out.setText(invoicePrompt(pendingAmount, pendingInvoice));
            } else {
                out.setText("a quote is already open, but its invoice is not on this"
                        + " device — tap \"Check payment\" if you have paid it, or"
                        + " \"Discard quote\" to ask the gateway for a new one.");
            }
        }

        mintField = new android.widget.EditText(this);
        mintField.setHint("mint URL (blank = use the gateway's own)");
        mintField.setText(prefs().getString("mint", ""));
        Button saveMint = new Button(this);
        saveMint.setText("Save mint");
        saveMint.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                prefs().edit().putString("mint", mintField.getText().toString().trim()).apply();
                out.setText("mint saved; blank means use the gateway's own");
            }
        });

        Button abandon = new Button(this);
        abandon.setText("Discard quote");
        abandon.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                clearPending();
                buy.setText("Buy access (Lightning)");
                out.setText("quote discarded; nothing was paid by this app");
            }
        });

        root.addView(scan);
        root.addView(buy);
        root.addView(abandon);
        root.addView(mintField);
        root.addView(saveMint);
        root.addView(token);
        root.addView(save);
        root.addView(service);
        root.addView(out);

        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

        // A resumed quote owns the screen. checkGateway() writes "probing …"
        // into the same TextView, so running it here would erase the invoice
        // the customer came back for — the exact blank-box state this restore
        // exists to prevent.
        if (restored) {
            return;
        }
        checkGateway();
    }

    private android.content.SharedPreferences prefs() {
        return getSharedPreferences("tollgate", MODE_PRIVATE);
    }

    /** The gateway the kernel is actually routing through — same rule as --auto. */
    private String defaultGateway() {
        ConnectivityManager cm = (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        Network active = cm.getActiveNetwork();
        if (active == null) return null;
        LinkProperties lp = cm.getLinkProperties(active);
        if (lp == null) return null;
        for (RouteInfo r : lp.getRoutes()) {
            if (!r.isDefaultRoute()) continue;
            InetAddress gw = r.getGateway();
            if (gw != null && gw.getAddress().length == 4) return gw.getHostAddress();
        }
        return null;
    }

    private void checkGateway() {
        final String gw = defaultGateway();
        if (gw == null) {
            out.setText("No IPv4 default route: not on a network yet.");
            return;
        }
        final String url = "http://" + gw + ":2121/";
        out.setText("probing " + url + " …");
        io.execute(new Runnable() {
            @Override
            public void run() {
                final String json = Core.describe(url);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        out.setText("gateway " + gw + "\n\n" + json.replace(",", ",\n"));
                    }
                });
            }
        });
    }

    /**
     * Buy access over the Lightning lane.
     *
     * Two steps on purpose. The first asks the gate for a quote and shows the
     * BOLT11; the customer pays it in whatever Lightning wallet they already
     * have. The second polls the SAME quote until the gate reports
     * {@code access_granted}. Nothing is paid by this app, and a quote that was
     * paid but not granted is a retry — never a second invoice.
     */
    private void buyAccess() {
        if (pendingQuote != null) {
            pollPending();
            return;
        }
        final String gw = defaultGateway();
        if (gw == null) {
            out.setText("No IPv4 default route: not on a network yet.");
            return;
        }
        final String url = "http://" + gw + ":2121/";
        final String configured = prefs().getString("mint", "").trim();
        buy.setEnabled(false);
        out.setText("asking " + url + " for a quote …");
        io.execute(new Runnable() {
            @Override
            public void run() {
              try {
                // The core resolves the mint: the advert is the authority, and
                // `mint_choice` only honours an override the gate advertises.
                // This used to scrape describe's JSON in Java, which read an
                // empty mints array as the literal string "decision".
                String resolved = Core.resolveMint(url, configured, 8000L);
                final String mint = field(resolved, "mint");
                if (mint == null || mint.isEmpty()) {
                    final String msg = "no mint: " + resolved;
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            buy.setEnabled(true);
                            out.setText(msg);
                        }
                    });
                    return;
                }
                final String json = Core.requestInvoice(url, 1L, mint, 8000L);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        buy.setEnabled(true);
                        String q = field(json, "quote");
                        // Empty as well as absent: a malformed or hostile gate
                        // answering {"quote":""} would otherwise be remembered
                        // as a live quote and poll a blank id forever.
                        if (q == null || q.isEmpty()) {
                            out.setText("no quote:\n\n" + json.replace(",", ",\n"));
                            return;
                        }
                        rememberPending(q, url, field(json, "invoice"), field(json, "amount"));
                        buy.setText("Check payment");
                        out.setText(invoicePrompt(field(json, "amount"), field(json, "invoice")));
                    }
                });
              } finally {
                // Any throw escaping here would otherwise leave the app's only
                // purchase button disabled forever (and kill the process, since
                // this is an executor thread). Expected gate errors arrive as
                // JSON and are handled above; this is for the unexpected.
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        buy.setEnabled(true);
                    }
                });
              }
            }
        });
    }

    /** Forget the outstanding quote, in memory and in prefs. */
    private void clearPending() {
        pendingQuote = null;
        pendingGate = null;
        pendingInvoice = null;
        pendingAmount = null;
        prefs().edit().remove("pending_quote").remove("pending_gate")
                .remove("pending_invoice").remove("pending_amount").apply();
    }

    /** Remember the outstanding quote across Activity recreation. */
    private void rememberPending(String quote, String gate, String invoice, String amount) {
        pendingQuote = quote;
        pendingGate = gate;
        pendingInvoice = invoice;
        pendingAmount = amount;
        prefs().edit().putString("pending_quote", quote).putString("pending_gate", gate)
                .putString("pending_invoice", invoice).putString("pending_amount", amount)
                .apply();
    }

    /**
     * The one place the "pay this" message is worded.
     *
     * First issue and post-rotation resume must read identically, or a customer
     * who rotated cannot tell the invoice they are looking at is the same one.
     */
    private static String invoicePrompt(String amount, String invoice) {
        return "pay this invoice (" + (amount == null ? "" : amount) + " sat):\n\n"
                + invoice
                + "\n\nthen tap \"Check payment\".";
    }

    private void pollPending() {
        final String url = pendingGate;
        final String quote = pendingQuote;
        buy.setEnabled(false);
        io.execute(new Runnable() {
            @Override
            public void run() {
                final String json = Core.pollInvoice(url, quote, 8000L);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        buy.setEnabled(true);
                        if (json.contains("\"access_granted\":true")) {
                            clearPending();
                            buy.setText("Buy access (Lightning)");
                            out.setText("ACCESS GRANTED\n\n" + json.replace(",", ",\n"));
                        } else if (json.contains("\"retry\":true")) {
                            out.setText("paid, gate not open yet — tap again\n\n"
                                    + json.replace(",", ",\n"));
                        } else {
                            out.setText("not granted yet\n\n" + json.replace(",", ",\n"));
                        }
                    }
                });
            }
        });
    }

    /**
     * Read one flat string/number field out of the core's JSON.
     *
     * Deliberately not a JSON parser: the core emits a flat object with no
     * nesting and no escaped quotes in the fields the UI reads, and pulling in
     * a parser to read two keys would be more code than the whole shell. A
     * missing key returns null, which every caller treats as "show it raw".
     */
    private static String field(String json, String key) {
        String needle = "\"" + key + "\":";
        int i = json.indexOf(needle);
        if (i < 0) return null;
        int start = i + needle.length();
        if (start >= json.length()) return null;
        char c = json.charAt(start);
        if (c == '"') {
            int end = json.indexOf('"', start + 1);
            if (end < 0) return null;
            return json.substring(start + 1, end);
        }
        int end = start;
        while (end < json.length() && "-0123456789".indexOf(json.charAt(end)) >= 0) end++;
        return end == start ? null : json.substring(start, end);
    }
}
