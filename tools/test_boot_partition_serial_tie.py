#!/usr/bin/env python3
"""Two OpenCore partitions whose configs both set this Mac's serial: the one on the macOS disk wins only when it is the
only match there. Issue #81: an EFI partition and a FAT partition labelled EFI, both on disk0 - the ESP (disk0s1) was
picked although disk0s2 started the Mac; the boot-args test must decide instead. Runs the setup's real lines."""
import pathlib
import subprocess
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
START = "        esp=$(boot_esp); edisk="
END = '        [ "$ondisk" -gt 1 ] && found=${match# }\n'


def run(match, esp, found="disk0s1 disk0s2 disk2s1"):
    s = SETUP.read_text()
    assert s.count(START) == 1 and s.count(END) == 1, "anchors moved in nullmoth-setup.sh"
    block = s[s.index(START):s.index(END) + len(END)]
    script = (f'ok() {{ :; }}\nboot_esp() {{ echo {esp}; }}\nmatch=" {match}"; found="{found}"; BOOT_BOUND=0\n'
              + block + '\necho "RESULT $BOOT_BOUND $found"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip().splitlines()[-1]


class SerialTie(unittest.TestCase):
    def test_the_stick_and_the_macos_disk_tie_goes_to_the_macos_disk(self):
        self.assertEqual(run("disk0s1 disk2s1", "disk0s1"), "RESULT 1 disk0s1")

    def test_two_matches_on_the_macos_disk_are_not_decided_by_the_esp(self):
        # issue #81: the ESP must NOT be picked; both go on to the boot-args test
        self.assertEqual(run("disk0s1 disk0s2", "disk0s1"), "RESULT 0 disk0s1 disk0s2")

    def test_two_digit_disk_numbers_are_not_confused_with_one_digit_ones(self):
        self.assertEqual(run("disk1s1 disk12s1", "disk1s1"), "RESULT 1 disk1s1")


if __name__ == "__main__":
    unittest.main()
