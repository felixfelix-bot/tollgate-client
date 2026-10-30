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
 * Discovery-only foreground watcher. Never spends a stored token.
 * Automatic payment is disabled until durable budgets and ambiguous-token
 * reconciliation are implemented; see docs/android-verification.md.
 */
public class TollgateService extends Service {
    static final String CHANNEL = "tollgate";
    static final int NOTIFICATION_ID = 42;
    private static final long INTERVAL_MS = 60_000;

    private volatile boolean running = true;
    private boolean foreground = false;
    private Thread worker;

    @Override
    public void onCreate() {
        super.onCreate();
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
        post("TollGate at " + gw, "discovery only — automatic spending disabled: " + described);
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
