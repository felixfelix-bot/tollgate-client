package dev.tollgate.client;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.RouteInfo;
import android.os.IBinder;

import java.net.InetAddress;

/**
 * Watches the gateway we are routed through and buys when it is selling.
 *
 * A foreground service, because this is exactly the case Android would otherwise
 * kill: the interesting moment is when the screen is off and the phone has just
 * joined a network. The notification is not decoration — it is the price and the
 * decision, and it disappears when there is nothing to sell.
 */
public class TollgateService extends Service {
    static final String CHANNEL = "tollgate";
    static final int NOTIFICATION_ID = 42;
    private static final long INTERVAL_MS = 60_000;

    private volatile boolean running = true;
    private Thread worker;

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationChannel ch = new NotificationChannel(
                CHANNEL, "TollGate", NotificationManager.IMPORTANCE_LOW);
        ((NotificationManager) getSystemService(NOTIFICATION_SERVICE)).createNotificationChannel(ch);
        startForeground(NOTIFICATION_ID, notification("watching the gateway", "no TollGate found yet"));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (worker == null) {
            worker = new Thread(new Runnable() {
                @Override
                public void run() {
                    while (running) {
                        try {
                            tick();
                        } catch (Throwable t) {
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

        SharedPreferences prefs = getSharedPreferences("tollgate", MODE_PRIVATE);
        String token = prefs.getString("token", "");
        boolean withinCap = described.contains("\"decision\":\"Buy");

        if (!token.isEmpty() && withinCap) {
            // Auto-buy: the note is the payment; there is nothing to confirm.
            String journal = getFilesDir().getAbsolutePath() + "/journal.txt";
            String attempt = "svc-" + (System.currentTimeMillis() / 60000);
            String bought = Core.buy(url, token, attempt, journal);
            if (bought.contains("\"state\":\"granted\"")) {
                prefs.edit().putString("token", "").apply(); // a note is single-use
                post("bought access at " + gw, bought);
                return;
            }
            post("could not buy at " + gw, bought);
            return;
        }
        post("TollGate at " + gw, withinCap ? described : "above your cap: " + described);
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
