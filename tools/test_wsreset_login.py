#!/usr/bin/env python3
"""The flicker stopgap restarts WindowServer only at the login window. 1.9.0 (10-10): with auto-login the session was
already up when it fired, so the restart dropped the user to the password screen. Runs the real script with stubs."""
import os
import pathlib
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
START = "cat > /Library/NullMoth/nullmoth-wsreset.sh <<'WS'\n"
END = "\nWS\n"


def run(console, autologin=""):
    s = SETUP.read_text()
    assert s.count(START) == 1, "anchor moved in nullmoth-setup.sh"
    body = s[s.index(START) + len(START):]
    body = body[:body.index(END)]
    t = tempfile.mkdtemp()
    log, mark = os.path.join(t, "wsreset.log"), os.path.join(t, "done")
    body = body.replace("/Library/NullMoth/wsreset.log", log).replace("/var/run/nullmoth-wsreset.done", mark)
    body = body.replace("/Library/NullMoth/no-wsreset", os.path.join(t, "no-wsreset"))
    body = body.replace("PATH=/usr/bin:/bin:/usr/sbin:/sbin", f"PATH={t}/bin:/usr/bin:/bin")
    os.makedirs(os.path.join(t, "bin"))
    stubs = {
        "kextstat": "echo com.nullmoth.NVAccel", "nvram": "exit 1", "pgrep": "echo 999999", "ps": "echo 00:10",
        "ioreg": "exit 0", "log": "exit 0", "sleep": "exit 0", "sysctl": "echo '{ sec = 1700000000, usec = 0 }'",
        "stat": f"echo {console}",
        "defaults": f"{'echo ' + autologin if autologin else 'exit 1'}",
    }
    for name, code in stubs.items():
        p = os.path.join(t, "bin", name)
        with open(p, "w") as f:
            f.write("#!/bin/bash\n" + code + "\n")
        os.chmod(p, 0o755)
    script = os.path.join(t, "ws.sh")
    with open(script, "w") as f:
        f.write(body)
    r = subprocess.run(["/bin/bash", script], capture_output=True, text=True, timeout=60)
    return r.returncode, open(log).read() if os.path.exists(log) else ""


class LoginWindowOnly(unittest.TestCase):
    def test_at_the_login_window_it_restarts_once(self):
        rc, log = run("root")
        self.assertEqual(rc, 0)
        self.assertIn("restarted WindowServer once", log)

    def test_auto_login_is_never_interrupted(self):
        rc, log = run("root", autologin="user1")
        self.assertIn("skipped the restart: auto-login is on", log)
        self.assertNotIn("restarted WindowServer once", log)

    def test_a_logged_in_user_is_never_logged_out(self):
        rc, log = run("user1")
        self.assertIn("skipped the restart: user1 is already logged in", log)
        self.assertNotIn("restarted WindowServer once", log)


if __name__ == "__main__":
    unittest.main()
