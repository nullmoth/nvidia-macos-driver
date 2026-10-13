#!/usr/bin/env python3
"""The installer recovers a guard left by an install that was killed, and only that.

Runs recover_stale_guard from app/Resources/nullmoth-install.sh against a temporary guard directory, with die and
rollback_install stubbed, in six cases: a live install stops; a dead owner's guard is cleared; a guard that had started
changing files is rolled back from its recorded backup first; a failed restore still stops; a guard from 1.14.7 or
earlier (no owner record) is cleared only when it is older than this boot.
"""
import os
import pathlib
import re
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "app/Resources/nullmoth-install.sh"


def functions(tmp):
    text = SCRIPT.read_text()
    boot = "\n".join(re.search(r"^%s\(\) \{.*$" % f, text, re.M).group(0) for f in ("boot_sec", "same_boot", "is_installer_pid"))
    body = re.search(r"^recover_stale_guard\(\) \{.*?^\}", text, re.M | re.S).group(0)
    # the guard must be root's on a real Mac; here it is the test user's
    body = body.replace('"$(stat -f \'%u\' "$g" 2>/dev/null)" = 0', '"$(stat -f %u "$g")" = "$(id -u)"')
    body = body.replace("/Library/NullMoth/backup-*", f"{tmp}/backup-*")
    assert "id -u" in body and f"{tmp}/backup-*" in body, "anchors in recover_stale_guard moved"
    return boot + "\n" + body


def run_case(prepare):
    with tempfile.TemporaryDirectory() as tmp:
        guard = os.path.join(tmp, "install.lock")
        os.mkdir(guard)
        os.mkdir(os.path.join(tmp, "backup-1"))
        script = "\n".join([
            "set -u", functions(tmp),
            'die() { echo "STOP: $*"; exit 1; }',
            'rollback_install() { echo "ROLLBACK from $BK"; return 0; }',
            f"LOCKDIR={guard}", f"TMP={tmp}", "B=$(boot_sec)",
            # a stand-in live installer: a process whose command line names install.sh
            f"printf 'sleep 20\\n' > {tmp}/install.sh", f"/bin/bash {tmp}/install.sh >/dev/null 2>&1 & INST=$!", "/bin/sleep 20 >/dev/null 2>&1 & OTHER=$!", "sleep 0.3",
            prepare,
            '( recover_stale_guard; [ -d "$LOCKDIR" ] && echo "guard KEPT" || echo "guard CLEARED" )',
            "kill $INST $OTHER 2>/dev/null",
        ])
        return subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=30).stdout


class StaleGuard(unittest.TestCase):
    def test_a_live_install_keeps_its_guard(self):
        out = run_case("printf 'pid=%s\\nboot=%s\\n' $INST $B > $LOCKDIR/owner")
        self.assertIn("guard KEPT", out, out)

    def test_a_live_install_survives_boot_time_drift(self):
        out = run_case("printf 'pid=%s\\nboot=%s\\n' $INST $((B - 30)) > $LOCKDIR/owner")
        self.assertIn("guard KEPT", out, out)

    def test_a_reused_pid_that_is_not_the_installer_is_stale(self):
        out = run_case("printf 'pid=%s\\nboot=%s\\n' $OTHER $B > $LOCKDIR/owner")
        self.assertIn("guard CLEARED", out, out)

    def test_an_installer_pid_from_an_earlier_boot_is_stale(self):
        out = run_case("printf 'pid=%s\\nboot=%s\\n' $INST $((B - 7200)) > $LOCKDIR/owner")
        self.assertIn("guard CLEARED", out, out)

    def test_a_dead_owner_is_cleared_without_a_rollback(self):
        out = run_case("printf 'pid=99999\\nboot=%s\\n' $B > $LOCKDIR/owner")
        self.assertIn("guard CLEARED", out, out)
        self.assertNotIn("ROLLBACK", out, out)

    def test_a_dead_install_that_changed_files_is_rolled_back_first(self):
        out = run_case("printf 'pid=99999\\nboot=%s\\nbk=%s\\n' $B $TMP/backup-1 > $LOCKDIR/owner; : > $LOCKDIR/installing")
        self.assertIn("ROLLBACK from", out, out)
        self.assertIn("guard CLEARED", out, out)

    def test_a_failed_restore_still_stops(self):
        out = run_case("printf 'pid=99999\\nboot=%s\\n' $B > $LOCKDIR/owner; : > $LOCKDIR/restore-incomplete")
        self.assertIn("STOP: the last install could not restore", out, out)
        self.assertNotIn("guard CLEARED", out, out)

    def test_an_old_format_guard_from_before_this_boot_is_cleared(self):
        out = run_case("touch -t 202001010000 $LOCKDIR")
        self.assertIn("guard CLEARED", out, out)

    def test_an_old_format_guard_from_this_boot_is_kept(self):
        out = run_case("true")
        self.assertIn("guard KEPT", out, out)


if __name__ == "__main__":
    unittest.main()
