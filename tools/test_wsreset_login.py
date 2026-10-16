#!/usr/bin/env python3
"""The flicker stopgap restarts WindowServer only at the login window. 1.9.0 (10-10): with auto-login the session was
already up when it fired, so the restart dropped the user to the password screen. Runs the real script with stubs."""
import os
import pathlib
import shlex
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
START = "cat > /Library/NullMoth/nullmoth-wsreset.sh <<'WS'\n"
END = "\nWS\n"


def run(console, autologin="", restart_enabled=True, opted_out=False, marked=False):
    s = SETUP.read_text()
    assert s.count(START) == 1, "anchor moved in nullmoth-setup.sh"
    body = s[s.index(START) + len(START):]
    body = body[:body.index(END)]
    directory = tempfile.TemporaryDirectory()
    t = directory.name
    log, mark = os.path.join(t, "wsreset.log"), os.path.join(t, "done")
    body = body.replace("/Library/NullMoth/wsreset.log", log).replace("/var/run/nullmoth-wsreset.done", mark)
    body = body.replace("/Library/NullMoth/no-wsreset", os.path.join(t, "no-wsreset"))
    body = body.replace("PATH=/usr/bin:/bin:/usr/sbin:/sbin", f"PATH={t}/bin:/usr/bin:/bin")
    os.makedirs(os.path.join(t, "bin"))
    body = body.replace("/Library/NullMoth/enable-wsreset", os.path.join(t, "enable-wsreset"))
    if restart_enabled:
        pathlib.Path(t, "enable-wsreset").touch()
    if opted_out:
        pathlib.Path(t, "no-wsreset").touch()
    if marked:
        pathlib.Path(mark).touch()
    kills = os.path.join(t, "kills")
    # kill is a shell builtin: override it so tests never signal a real process.
    body = body.replace("PATH=", f"kill() {{ printf '%s\\n' \"$*\" >> {shlex.quote(kills)}; }}\nPATH=", 1)
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
    result = (r.returncode, pathlib.Path(log).read_text() if os.path.exists(log) else "",
              pathlib.Path(kills).read_text().splitlines() if os.path.exists(kills) else [])
    directory.cleanup()
    return result


class LoginWindowOnly(unittest.TestCase):
    def test_at_the_login_window_it_restarts_once(self):
        rc, log, kills = run("root")
        self.assertEqual(rc, 0)
        self.assertIn("restarted WindowServer once", log)
        self.assertEqual(kills, ["-TERM 999999", "-9 999999"])

    def test_auto_login_is_never_interrupted(self):
        rc, log, kills = run("root", autologin="jake")
        self.assertIn("skipped the restart: auto-login is on", log)
        self.assertNotIn("restarted WindowServer once", log)
        self.assertEqual(kills, [])

    def test_a_logged_in_user_is_never_logged_out(self):
        rc, log, kills = run("jake")
        self.assertIn("skipped the restart: jake is already logged in", log)
        self.assertNotIn("restarted WindowServer once", log)
        self.assertEqual(kills, [])

    def test_restart_requires_explicit_opt_in(self):
        for console in ("root", "_windowserver", "jake"):
            with self.subTest(console=console):
                rc, log, kills = run(console, restart_enabled=False)
                self.assertEqual(rc, 0)
                self.assertEqual(log, "")
                self.assertEqual(kills, [])

    def test_opt_out_overrides_opt_in(self):
        rc, log, kills = run("root", restart_enabled=True, opted_out=True)
        self.assertEqual((rc, log, kills), (0, "", []))

    def test_marker_still_prevents_restart(self):
        rc, log, kills = run("root", restart_enabled=True, marked=True)
        self.assertEqual((rc, log, kills), (0, "", []))


if __name__ == "__main__":
    unittest.main()
