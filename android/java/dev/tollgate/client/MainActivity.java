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
    /** The quote being paid, if any. Kept so "Check payment" polls the same quote. */
    private String pendingQuote;
    private String pendingGate;
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

        root.addView(scan);
        root.addView(buy);
        root.addView(mintField);
        root.addView(saveMint);
        root.addView(token);
        root.addView(save);
        root.addView(service);
        root.addView(out);

        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

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
                // Ask the GATE which mints it accepts, then choose. The advert
                // is the authority: a quote at a mint the gate does not accept
                // is a guaranteed refusal, so this client never carries its own
                // hardcoded mint default.
                String mint = configured;
                if (mint.isEmpty()) {
                    String described = Core.describe(url);
                    mint = firstMint(described);
                }
                if (mint == null || mint.isEmpty()) {
                    final String msg = "no mint: set one, or the gateway advertises none";
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            buy.setEnabled(true);
                            out.setText(msg);
                        }
                    });
                    return;
                }
                final String chosen = mint;
                final String json = Core.requestInvoice(url, 1L, chosen, 8000L);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        buy.setEnabled(true);
                        String q = field(json, "quote");
                        if (q == null) {
                            out.setText("no quote:\n\n" + json.replace(",", ",\n"));
                            return;
                        }
                        pendingQuote = q;
                        pendingGate = url;
                        buy.setText("Check payment");
                        out.setText("pay this invoice ("
                                + field(json, "amount") + " sat):\n\n"
                                + field(json, "invoice")
                                + "\n\nthen tap \"Check payment\".");
                    }
                });
            }
        });
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
                            pendingQuote = null;
                            pendingGate = null;
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
     * The first mint URL in a {@code describe} answer, or null.
     *
     * {@code describe} emits {@code "mints":["https://…","…"]}. This reads the
     * first entry only: the gate lists the mints it accepts in advert order and
     * any of them is payable, so picking the first is a choice, not a guess
     * about which is "right".
     */
    private static String firstMint(String describeJson) {
        if (describeJson == null) return null;
        int i = describeJson.indexOf("\"mints\":[");
        if (i < 0) return null;
        int start = describeJson.indexOf('"', i + 9);
        if (start < 0) return null;
        int end = describeJson.indexOf('"', start + 1);
        if (end < 0) return null;
        String m = describeJson.substring(start + 1, end);
        return m.isEmpty() ? null : m;
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
