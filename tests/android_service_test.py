"""Host-JVM lifecycle tests of the actual service (no Android runtime or funds).
Run: python3 tests/android_service_test.py
Requires javac/java on PATH. Android APIs are deliberately minimal test doubles.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
STUBS = {
    "android/content/Context.java": '''package android.content;
public class Context {
 public static final String NOTIFICATION_SERVICE="notification", CONNECTIVITY_SERVICE="connectivity";
 public static final int MODE_PRIVATE=0;
 public Object getSystemService(String name) { return name.equals(NOTIFICATION_SERVICE) ? new android.app.NotificationManager() : new android.net.ConnectivityManager(); }
 public SharedPreferences getSharedPreferences(String n,int m) { return new SharedPreferences(); }
 public java.io.File getFilesDir() { return new java.io.File("."); }
}''',
    "android/content/SharedPreferences.java": '''package android.content;
public class SharedPreferences {
 public String getString(String k,String d) { return "cashuBdummyNeverSpend"; }
 public Editor edit() { return new Editor(); }
 public static class Editor { public Editor putString(String k,String v) { return this; } public void apply() {} }
}''',
    "android/content/Intent.java": '''package android.content;
public class Intent { public Intent(Context c,Class<?> k) {} }''',
    "android/app/Service.java": '''package android.app;
public class Service extends android.content.Context {
 public static final int START_STICKY=1, START_NOT_STICKY=2;
 public RuntimeException failure; public boolean stopped;
 public void onCreate() {} public void onDestroy() {}
 public int onStartCommand(android.content.Intent i,int f,int s) { return 0; }
 public android.os.IBinder onBind(android.content.Intent i) { return null; }
 public void startForeground(int n,Notification v) { if(failure!=null) throw failure; }
 public void stopSelf() { stopped=true; }
}''',
    "android/app/NotificationChannel.java": '''package android.app;
public class NotificationChannel { public NotificationChannel(String a,String b,int c) {} }''',
    "android/app/NotificationManager.java": '''package android.app;
public class NotificationManager { public static final int IMPORTANCE_LOW=2;
 public void createNotificationChannel(NotificationChannel c) {} public void notify(int n,Notification v) {} }''',
    "android/app/Notification.java": '''package android.app;
public class Notification { public static class Builder {
 public Builder(android.content.Context c,String s) {} public Builder setContentTitle(String s) { return this; }
 public Builder setContentText(String s) { return this; } public Builder setSmallIcon(int n) { return this; }
 public Builder setContentIntent(PendingIntent p) { return this; } public Builder setOnlyAlertOnce(boolean b) { return this; }
 public Notification build() { return new Notification(); }
} }''',
    "android/app/PendingIntent.java": '''package android.app;
public class PendingIntent { public static final int FLAG_IMMUTABLE=1,FLAG_UPDATE_CURRENT=2;
 public static PendingIntent getActivity(android.content.Context c,int n,android.content.Intent i,int f) { return new PendingIntent(); } }''',
    "android/os/IBinder.java": 'package android.os; public interface IBinder {}',
    "android/R.java": 'package android; public class R { public static class drawable { public static final int stat_sys_warning=1; } }',
    "android/util/Log.java": '''package android.util; public class Log {
 public static int w(String t,String m) { return 0; }
 public static int w(String t,String m,Throwable e) { return 0; }
}''',
    "android/net/Network.java": 'package android.net; public class Network {}',
    "android/net/ConnectivityManager.java": '''package android.net; public class ConnectivityManager {
 public Network getActiveNetwork() { return new Network(); }
 public LinkProperties getLinkProperties(Network n) { return new LinkProperties(); } }''',
    "android/net/LinkProperties.java": '''package android.net; public class LinkProperties {
 public java.util.List<RouteInfo> getRoutes() { return java.util.Collections.singletonList(new RouteInfo()); } }''',
    "android/net/RouteInfo.java": '''package android.net; public class RouteInfo {
 public boolean isDefaultRoute() { return true; }
 public java.net.InetAddress getGateway() { return java.net.InetAddress.getLoopbackAddress(); } }''',
    "dev/tollgate/client/MainActivity.java": 'package dev.tollgate.client; public class MainActivity {}',
    "dev/tollgate/client/Core.java": '''package dev.tollgate.client; public class Core {
 public static volatile int buys=0, probes=0;
 public static String describe(String u) { probes++; return "{\\"decision\\":\\"Buy\\"}"; }
 public static String buy(String u,String t,String a,String j) { buys++; return "{\\"state\\":\\"outcome-unknown\\"}"; }
}''',
    "dev/tollgate/client/ServiceTest.java": '''package dev.tollgate.client;
public class ServiceTest {
 static Object field(TollgateService s,String n) throws Exception { var f=TollgateService.class.getDeclaredField(n); f.setAccessible(true); return f.get(s); }
 static void check(boolean b,String m) { if(!b) throw new AssertionError(m); }
 public static void main(String[] args) throws Exception {
  for(RuntimeException failure : new RuntimeException[]{new SecurityException("permission"),new IllegalStateException("platform refusal")}) {
   TollgateService s=new TollgateService(); s.failure=failure; s.onCreate();
   check(s.stopped,"refused foreground must stopSelf");
   check(s.onStartCommand(null,0,1)==android.app.Service.START_NOT_STICKY,"no sticky restart after refusal");
   check(field(s,"worker")==null,"no worker after refusal"); s.onDestroy();
  }
  TollgateService s=new TollgateService(); s.onCreate();
  check(!s.stopped,"successful foreground must stay alive");
  s.onStartCommand(null,0,1); Thread worker=(Thread)field(s,"worker");
  check(worker!=null,"successful foreground starts watcher");
  s.onStartCommand(null,0,2); check(field(s,"worker")==worker,"only one watcher");
  s.onDestroy(); worker.join(2000); check(!worker.isAlive(),"destroy stops watcher");
  var tick=TollgateService.class.getDeclaredMethod("tick"); tick.setAccessible(true);
  // Simulate successive ticks, including a new service instance/restart. The
  // stored dummy token and Buy decision must never reach Core.buy.
  for(int i=0;i<3;i++) tick.invoke(new TollgateService());
  check(Core.probes>=3,"ticks really probe"); check(Core.buys==0,"watcher must not spend stored tokens");
  System.out.println("PASS: permission/platform refusal, single watcher, destroy, repeated ticks/restart: zero buys");
 }
}''',
}


class AndroidServiceTests(unittest.TestCase):
    def test_manifest_data_sync_permission(self):
        ns = "{http://schemas.android.com/apk/res/android}"
        root = ET.parse(ROOT / "android/AndroidManifest.xml").getroot()
        permissions = {e.get(ns + "name") for e in root.findall("uses-permission")}
        self.assertIn("android.permission.FOREGROUND_SERVICE", permissions)
        self.assertIn("android.permission.FOREGROUND_SERVICE_DATA_SYNC", permissions)
        service = root.find("application/service")
        assert service is not None
        self.assertEqual(service.get(ns + "foregroundServiceType"), "dataSync")

    def test_real_service_lifecycle_and_no_spend(self):
        with tempfile.TemporaryDirectory(prefix="android-service-", dir=os.environ.get("TMPDIR")) as tmp:
            base = Path(tmp)
            for name, content in STUBS.items():
                p = base / name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(content)
            sources = [str(p) for p in base.rglob("*.java")]
            sources.append(str(ROOT / "android/java/dev/tollgate/client/TollgateService.java"))
            subprocess.run(["javac", "-d", tmp, *sources], check=True)
            subprocess.run(["java", "-cp", tmp, "dev.tollgate.client.ServiceTest"], check=True, timeout=10)


if __name__ == "__main__":
    unittest.main(verbosity=2)
