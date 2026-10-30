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
        token.setHint("paste a Cashu note (cashuB…) to fund auto-buy");
        token.setText(prefs().getString("token", ""));

        Button save = new Button(this);
        save.setText("Save note");
        save.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                prefs().edit().putString("token", token.getText().toString().trim()).apply();
                out.setText("note saved — the background service will spend it");
            }
        });

        Button service = new Button(this);
        service.setText("Start background watch");
        service.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startForegroundService(new Intent(MainActivity.this, TollgateService.class));
                out.setText("watching in the background (60 s)");
            }
        });

        root.addView(scan);
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
}
