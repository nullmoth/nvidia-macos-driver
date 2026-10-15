"""Setup turns DisableVariableWrite on for existing Zen 4/5 Ryzen EFIs (a restart from macOS froze an AM5 board, chat
10-12), only with OpenRuntime.efi loaded, and leaves every other CPU alone."""
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
source = (ROOT / "app/Resources/nullmoth-setup.sh").read_text()
start = source.index("DVW_NEW=0\n")
end = source.index("if { nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove", start)
BLOCK = source[start:end]
HARNESS = r'''
set -u
C="$1"; CPU="$2"; EDITS=()
get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }
note() { echo "NOTE $*"; }
sysctl() { echo "$CPU"; }
''' + BLOCK + r'''
printf 'EDIT %s\n' "${EDITS[@]+"${EDITS[@]}"}"
echo "DVW_NEW=$DVW_NEW"
'''


def run(cpu, quirk=None, drivers=("OpenRuntime.efi",), svm=None, devirt=None):
    cfg = {"Booter": {"Quirks": {}}, "UEFI": {"Drivers": [{"Path": d, "Enabled": True} for d in drivers]}}
    if svm is not None: cfg["Booter"]["Quirks"]["SetupVirtualMap"] = svm
    if devirt is not None: cfg["Booter"]["Quirks"]["DevirtualiseMmio"] = devirt
    if quirk is not None: cfg["Booter"]["Quirks"]["DisableVariableWrite"] = quirk
    with tempfile.TemporaryDirectory() as td:
        c = pathlib.Path(td) / "config.plist"; c.write_bytes(plistlib.dumps(cfg))
        p = subprocess.run(["/bin/bash", "-c", HARNESS, "h", str(c), cpu], capture_output=True, text=True, timeout=30)
        return p.stdout


class Zen45VariableWrite(unittest.TestCase):
    def test_a_9800x3d_with_openruntime_gets_the_quirk(self):
        out = run("AMD Ryzen 7 9800X3D 8-Core Processor")
        self.assertIn("EDIT Booter.Quirks.DisableVariableWrite|-bool|true", out)
        self.assertIn("DVW_NEW=1", out)

    def test_a_zen4_laptop_gets_it_too(self):
        self.assertIn("DisableVariableWrite|-bool|true", run("AMD Ryzen 9 7945HX with Radeon Graphics"))

    def test_without_openruntime_nothing_changes_and_it_says_why(self):
        out = run("AMD Ryzen 5 7600 6-Core Processor", drivers=("HfsPlus.efi",))
        self.assertNotIn("DisableVariableWrite|", out)
        self.assertIn("NOTE", out)

    def test_zen3_and_intel_are_left_alone(self):
        for cpu in ("AMD Ryzen 7 5800X 8-Core Processor", "AMD Ryzen 5 5600H with Radeon Graphics",
                    "Intel(R) Core(TM) i5-14400F"):
            out = run(cpu)
            self.assertNotIn("DisableVariableWrite|", out, cpu)
            self.assertIn("DVW_NEW=0", out, cpu)

    def test_a_zen3_board_with_the_memory_map_rewritten_gets_it(self):
        # an AM4 5600X stopped POSTing after macOS on 1.9 (chat 10-10); the risk is the remapped NVRAM write, not Zen 4
        for kw in ({"svm": True}, {"devirt": True}):
            out = run("AMD Ryzen 5 5600X 6-Core Processor", **kw)
            self.assertIn("DisableVariableWrite|-bool|true", out, kw)

    def test_intel_with_the_memory_map_rewritten_is_still_left_alone(self):
        self.assertNotIn("DisableVariableWrite|", run("Intel(R) Core(TM) i5-14400F", svm=True, devirt=True))

    def test_already_on_is_not_edited_again(self):
        self.assertNotIn("DisableVariableWrite|", run("AMD Ryzen 7 9800X3D 8-Core Processor", quirk=True))


if __name__ == "__main__":
    unittest.main()
