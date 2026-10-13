#!/usr/bin/env python3
"""Every script setup runs with [ -x ] reaches the app bundle executable.

nullmoth-setup.sh refuses the installer unless it is executable. A source file committed without its execute bit made
every install through the app stop ("the audited installer is missing from the app"). This checks both layers: the mode
git records for the scripts, and that app/build.sh sets 755 on each of them after copying the resources.
"""
import pathlib
import subprocess
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPTS = ["nullmoth-setup.sh", "nullmoth-install.sh", "nullmoth-uninstall.sh", "nullmoth-runtime-check.sh"]


class ScriptsExecutable(unittest.TestCase):
    def test_build_sets_755_on_every_script_setup_runs(self):
        build = (ROOT / "app/build.sh").read_text()
        for name in SCRIPTS:
            self.assertRegex(build, r'chmod 755 [^\n]*"\$APP/Contents/Resources/' + name.replace(".", r"\.") + '"',
                             f"app/build.sh does not chmod 755 {name}")

    def test_the_installers_are_recorded_executable(self):
        for rel in ["app/Resources/nullmoth-install.sh", "app/Resources/nullmoth-uninstall.sh", "package/install.sh"]:
            mode = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-s", rel], capture_output=True, text=True).stdout.split()
            self.assertTrue(mode, f"{rel} is not tracked")
            self.assertEqual(mode[0], "100755", f"{rel} is tracked as {mode[0]}, not 100755")


if __name__ == "__main__":
    unittest.main()
