#!/usr/bin/env python3
"""mac_parity_probe.sh turns macOS's own reports into feature rows. The system tools are replaced by scripts that print
fixture text (laid out the way a real Mac prints it), so every arm runs anywhere and reads nothing real."""
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
PROBE = HERE / "mac_parity_probe.sh"
FIX = HERE / "mac_parity_fixtures"


def run(profiles, serial="C02ABCDEF8J1", extra=None, args=()):
    """profiles maps a system_profiler data type to a fixture file name (or None for no output)."""
    d = tempfile.mkdtemp()
    cases = "".join(f'  {k}) cat "{FIX / v}" ;;\n' for k, v in profiles.items() if v)
    tools = {
        "system_profiler": 'for a in "$@"; do case "$a" in\n' + cases + '  *) ;;\nesac; done',
        "ioreg": f"echo '    \"IOPlatformSerialNumber\" = \"{serial}\"'",
        "fdesetup": "echo 'FileVault is On.'",
        "csrutil": "echo 'System Integrity Protection status: enabled.'",
        "pmset": 'if [ "$2" = batt ]; then echo "Now drawing from AC Power"; else printf "AC Power:\\n sleep 1\\n powernap 1\\n"; fi',
    }
    tools.update(extra or {})
    for name, body in tools.items():
        p = os.path.join(d, name)
        with open(p, "w") as f:
            f.write("#!/bin/bash\n" + body + "\n")
        os.chmod(p, os.stat(p).st_mode | stat.S_IXUSR)
    out = subprocess.run(["/bin/bash", str(PROBE), *args], capture_output=True, text=True, env={"PATH": d + ":/usr/bin:/bin"}).stdout
    return {line.split("\t")[0]: line.split("\t")[1] for line in out.splitlines() if "\t" in line}


class Probe(unittest.TestCase):
    def test_a_real_macs_wifi_report_gives_every_continuity_row_its_macos_statement(self):
        r = run({"SPAirPortDataType": "real_mac_airport.txt"})
        self.assertEqual((r["wifi.awdl0"], r["wifi.airdrop"], r["wifi.auto_unlock"]), ("yes", "yes", "yes"))

    def test_a_pc_wifi_report_without_awdl0_shows_the_gap(self):
        r = run({"SPAirPortDataType": "pc_airport.txt"})
        self.assertEqual((r["wifi.interface"], r["wifi.awdl0"], r["wifi.airdrop"]), ("yes", "no", "no"))

    def test_no_report_is_unknown_not_no(self):
        r = run({"SPAirPortDataType": None})
        self.assertEqual((r["wifi.awdl0"], r["wifi.airdrop"]), ("unknown", "unknown"))

    def test_bluetooth_le_comes_from_the_supported_services(self):
        r = run({"SPBluetoothDataType": "bluetooth_le.txt"})
        self.assertEqual((r["bluetooth.controller"], r["bluetooth.le"]), ("yes", "yes"))

    def test_metal_family_is_read_from_the_display_report(self):
        r = run({"SPDisplaysDataType": "displays_metal.txt"})
        self.assertEqual((r["gpu.metal"], r["gpu.metal4"]), ("yes", "no"))

    def test_a_serial_that_is_not_apple_shaped_is_reported_no(self):
        self.assertEqual(run({}, serial="A0000000009")["identity.serial"], "yes")  # shape only; the identity check names placeholders
        self.assertEqual(run({}, serial="short")["identity.serial"], "no")

    def test_only_limits_the_rows(self):
        r = run({"SPAirPortDataType": "real_mac_airport.txt"}, args=("--only", "wifi"))
        self.assertTrue(r and all(k.startswith("wifi") for k in r))


if __name__ == "__main__":
    unittest.main()
