package dev.tollgate.client;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.RouteInfo;
import android.os.IBinder;

import java.net.InetAddress;

/**
 * Foreground session watcher.
 *
 * Every 60 s it reads the gate's view of this client's session and asks the core
 * whether the session needs topping up. It **never spends a stored token** and
 * never pays anything itself: when a top-up is due it creates a Lightning quote
 * and posts it as a notification, for the customer to pay in their own wallet.
 *
 * Why not automatic payment: a BOLT11 has to be paid by something holding funds.
 * Doing that unattended on the device means a Cashu wallet with durable budgets
 * and NUT-07 reconciliation, which this build does not have — see
 * docs/android-verification.md. Until then "automatic" means "detects expiry and
 * hands you a ready quote", which is honest, and the alternative would be a
 * repeat-spend bug.
 */
public class TollgateService extends Service {
    static final String CHANNEL = "tollgate";
    static final int NOTIFICATION_ID = 42;
    private static final long INTERVAL_MS = 60_000;

    /** Top up when this much (percent) of the allotment is left. */
    private static final long THRESHOLD_PCT = 20;
    /** Cost of one top-up, in sats. The module's step price is 1 sat in the lab. */
    private static final long COST = 1;
    /** Refuse to spend past this many sats in a day. 0 = no cap recorded yet. */
    private static final long DAILY_BUDGET = 100;

    private volatile boolean running = true;
    /** An open quote this service prepared; non-null blocks a second invoice. */
    private volatile String pendingQuote;
    private volatile String pendingGate;
    /** Today's spend. In memory only: this build has no durable ledger yet. */
    private long spentToday = 0;
    private boolean foreground = false;
    private Thread worker;

    @Override
    public void onCreate() {
        super.onCreate();
        // An open quote survives a service restart, or the next tick would mint
        // a second invoice for a top-up the customer may have already paid.
        pendingQuote = getSharedPreferences("tollgate", MODE_PRIVATE)
                .getString("pending_quote", null);
        NotificationChannel ch = new NotificationChannel(
                CHANNEL, "TollGate", NotificationManager.IMPORTANCE_LOW);
        ((NotificationManager) getSystemService(NOTIFICATION_SERVICE)).createNotificationChannel(ch);
        // A refused foreground start must never degrade to background work.
        try {
            startForeground(NOTIFICATION_ID, notification("watching the gateway", "no TollGate found yet"));
            foreground = true;
        } catch (RuntimeException e) {
            foreground = false;
            running = false;
            android.util.Log.w("TollgateService", "foreground start refused", e);
            stopSelf();
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // onStartCommand can still arrive after stopSelf() in onCreate().
        if (!foreground || !running) {
            stopSelf();
            return START_NOT_STICKY;
        }
        if (worker == null) {
            worker = new Thread(new Runnable() {
                @Override
                public void run() {
                    while (running) {
                        try {
                            tick();
                        } catch (RuntimeException t) {
                            post("error", String.valueOf(t));
                        }
                        try {
                            Thread.sleep(INTERVAL_MS);
                        } catch (InterruptedException e) {
                            return;
                        }
                    }
                }
            });
            worker.start();
        }
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        running = false;
        if (worker != null) worker.interrupt();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private String gateway() {
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

    private void tick() {
        String gw = gateway();
        if (gw == null) {
            post("no route", "not on a network");
            return;
        }
        String url = "http://" + gw + ":2121/";
        String described = Core.describe(url);
        if (described.contains("\"error\"")) {
            // Not a TollGate, or unreachable: say so and stay quiet otherwise.
            post("no TollGate at " + gw, described.replaceAll("\\s+", " "));
            return;
        }

        // Do not read the saved bearer token or call Core.buy here. A new
        // attempt ID on each tick bypassed journal deduplication after an
        // ambiguous outcome; JNI also resets spent_today on every invocation.
        renewIfDue(gw, url);
    }

    /**
     * Ask the core whether this session needs topping up, and if so prepare a
     * quote.
     *
     * The core decides; this method only acts on an explicit `"top-up"`. The
     * `pending` flag it is given is the in-memory equivalent of the durable
     * journal the token lane lacks: while a quote is outstanding it stays true,
     * so a 60 s tick can never mint a second invoice for one top-up.
     */
    private void renewIfDue(String gw, String url) {
        if (pendingQuote != null) {
            // Still waiting on the quote we already posted: poll it rather than
            // asking for another.
            post("top-up awaiting payment", "a quote is already open for " + gw);
            return;
        }
        String decision = Core.renewalDecision(
                url, THRESHOLD_PCT, DAILY_BUDGET, spentToday, COST, pendingQuote != null, 8000L);
        String action = field(decision, "action");
        if (action == null) {
            post("cannot read session", decision);
            return;
        }
        if ("blocked".equals(action)) {
            post("session " + field(decision, "state"), field(decision, "why"));
            return;
        }
        if (!"top-up".equals(action)) {
            // "wait": headroom left. Say so quietly, no purchase, no quote.
            post("session " + field(decision, "state"),
                    field(decision, "remaining") + " of " + field(decision, "total") + " left");
            return;
        }
        // Top-up is due. Get a mint and a quote; never pay anything here.
        String mint = field(Core.resolveMint(url, prefs().getString("mint", ""), 8000L), "mint");
        if (mint == null) {
            post("no mint", "cannot prepare a top-up quote");
            return;
        }
        String invoice = Core.requestInvoice(url, COST, mint, 8000L);
        String quote = field(invoice, "quote");
        if (quote == null || quote.isEmpty()) {
            post("no quote", invoice);
            return;
        }
        pendingQuote = quote;
        pendingGate = url;
        prefs().edit().putString("pending_quote", quote).putString("pending_gate", url).apply();
        post("top up now", field(decision, "why") + " — open the app to pay");
    }

    private android.content.SharedPreferences prefs() {
        return getSharedPreferences("tollgate", MODE_PRIVATE);
    }

    /** One flat string/number field out of the core's JSON; null if absent. */
    private static String field(String json, String key) {
        if (json == null) return null;
        String needle = "\"" + key + "\":";
        int i = json.indexOf(needle);
        if (i < 0) return null;
        int start = i + needle.length();
        if (start >= json.length()) return null;
        if (json.charAt(start) == '"') {
            int end = json.indexOf('"', start + 1);
            return end < 0 ? null : json.substring(start + 1, end);
        }
        int end = start;
        while (end < json.length() && "-0123456789".indexOf(json.charAt(end)) >= 0) end++;
        return end == start ? null : json.substring(start, end);
    }

    private Notification notification(String title, String body) {
        Intent open = new Intent(this, MainActivity.class);
        PendingIntent pi = PendingIntent.getActivity(
                this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this, CHANNEL)
                .setContentTitle(title)
                .setContentText(body)
                .setSmallIcon(android.R.drawable.stat_sys_warning)
                .setContentIntent(pi)
                .setOnlyAlertOnce(true)
                .build();
    }

    private void post(String title, String body) {
        ((NotificationManager) getSystemService(NOTIFICATION_SERVICE))
                .notify(NOTIFICATION_ID, notification(title, body));
    }
}
