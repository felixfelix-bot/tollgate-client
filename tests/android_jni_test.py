"""Real host-JVM JNI/socket regression, no Android runtime or funds.
Run after cargo build --locked --lib --no-default-features --features android.
LIB_DIR may select an android-hardened host build instead.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]


class BoundaryTest(unittest.TestCase):
    def test_legacy_buy_is_fixed_refusal_without_io(self):
        requests = []

        class Gate(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append("GET")
                body = b'{"kind":10021,"tags":[["metric","milliseconds"],["step_size","60000"],["price_per_step","cashu","1","sat","https://mint.invalid","1"]]}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                requests.append("POST")
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                self.send_response(400)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b'{}')

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Gate)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix="jni-boundary-") as tmp:
                harness = Path(tmp) / "Boundary.java"
                harness.write_text('''import dev.tollgate.client.Core;
public class Boundary {
 public static void main(String[] args) {
  System.out.println(Core.describe(args[0]));
  System.out.println(Core.buy(args[0], "cashuAdummyNoValue", "one", args[1]));
  System.out.println(Core.buy(args[0], "secret\\\"\\\\\\n", "two", args[1]));
  System.out.println(Core.buy(null, null, null, null));
  System.out.println(Core.capsFor("sat\\\"\\\\\\n", 2, 100));
 }
}''')
                subprocess.run(["javac", "-d", tmp, str(ROOT / "android/java/dev/tollgate/client/Core.java"), str(harness)], check=True)
                target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
                lib = os.environ.get("LIB_DIR", str(target / "debug"))
                journal = Path(tmp) / "journal"
                result = subprocess.run(["java", "-Djava.library.path=" + lib, "-cp", tmp, "Boundary", f"http://127.0.0.1:{server.server_port}/", str(journal)], text=True, capture_output=True, check=True, timeout=30)
                # This assertion is first: RED must demonstrate a real legacy POST,
                # not merely a response/schema mismatch or short-circuit failure.
                self.assertEqual(requests, ["GET"], f"JNI performed unexpected IO: {requests}")
                rows = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertTrue(rows[0]["offers"])
                self.assertIn("Buy", rows[0]["decision"])
                for row in rows[1:4]:
                    self.assertEqual(row, {"state": "not-attempted", "code": "payment-integration-unavailable"})
                self.assertFalse(journal.exists())
                self.assertEqual(rows[4]["caps"]["unit"], 'sat"\\\n')
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main(verbosity=2)
