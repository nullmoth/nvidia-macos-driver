#!/usr/bin/env python3
"""Ensure Tahoe preparation cannot park an already upgraded installation."""
from pathlib import Path
import subprocess
import unittest

root = Path(__file__).resolve().parents[1]
source = (root / 'app/Resources/nullmoth-setup.sh').read_text()
start = source.index('if [ "$UPD" = prepare ]; then\n  PREPARE_MAJOR=')
end = source.index('\n# "--update prepare"', start)
guard = source[start:end]
wait_start = source.index('    if [ "$UPD" = finish ] && [ "${FROM:-}" = "$MAJ" ]')
wait_condition = source[wait_start:source.index('; then', wait_start)]

class UpgradeGuard(unittest.TestCase):
    def run_guard(self, os_version, operation):
        script = 'sw_vers() { echo "$TEST_VERSION"; }\nstop() { echo "$*"; exit 1; }\n' + guard + '\necho allowed\n'
        return subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True,
                              env={'TEST_VERSION': os_version, 'UPD': operation})

    def test_prepare_allows_only_mac_os_15(self):
        self.assertEqual(self.run_guard('15.8.1', 'prepare').returncode, 0)
        for version in ['26.0.1', '14.8', '27.0', '']:
            result = self.run_guard(version, 'prepare')
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('allowed', result.stdout)

    def test_cancel_and_finish_remain_available(self):
        for operation in ['cancel', 'finish']:
            self.assertEqual(self.run_guard('26.0.1', operation).returncode, 0)

    def test_guard_precedes_installer_and_efi_discovery(self):
        self.assertLess(start, source.index('INSTALL_THEN_PREPARE=0'))
        self.assertLess(start, source.index('step "Finding the OpenCore partition"'))

    def test_existing_tahoe_parking_is_restored_instead_of_waiting(self):
        for major, previous, waiting in [('15', '15', True), ('26', '15', False), ('26', '26', False)]:
            script = wait_condition + '; then echo wait; else echo finish; fi'
            result = subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True,
                                    env={'UPD': 'finish', 'MAJ': major, 'FROM': previous})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), 'wait' if waiting else 'finish')

    # Prepare parks the driver in one config; detection on a later run can pick another (a 1401 stick next to the
    # internal EFI). Cancel and finish must change back the config prepare recorded.
    def test_prepare_records_the_config_it_changed(self):
        prep = source[source.index('  do_prepare() {'):source.index('  case "$UPD" in\n  prepare)')]
        self.assertIn('CFGPART=%s\\nCFGREL=%s', prep)
        self.assertIn('"$cpart" "$OCREL" > "$PEND"', prep)

    def back_block(self):
        a = source.index('    if [ -n "${CFGPART:-}" ] && [ -n "${CFGREL:-}" ]; then')
        return source[a:source.index('    if [ "$UPD" = finish ] && [ "${FROM:-}" = "$MAJ" ]', a)]

    def run_back(self, cfgpart, mounted):
        import tempfile
        with tempfile.TemporaryDirectory() as t:
            (Path(t) / 'EFI/BOOT').mkdir(parents=True); (Path(t) / 'EFI/BOOT/config.plist').write_text('x')
            script = ('note() { echo "NOTE $*"; }; stop() { echo "STOP $*"; exit 1; }\n'
                      f'mount_efi() {{ [ "$1" = PART-INTERNAL ] && [ {int(mounted)} = 1 ] && MOUNT_POINT="{t}"; }}\n'
                      f'C=/Volumes/1401/EFI/OC/config.plist; CFGPART={cfgpart}; CFGREL=EFI/BOOT\n'
                      + self.back_block() + 'echo "C=$C"\n')
            r = subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True)
            return r.returncode, r.stdout.replace(t, '<internal>')

    def test_cancel_uses_the_recorded_config_not_detection(self):
        rc, out = self.run_back('PART-INTERNAL', True)
        self.assertEqual(rc, 0, out)
        self.assertIn('C=<internal>/EFI/BOOT/config.plist', out)
        self.assertIn('not /Volumes/1401/EFI/OC/config.plist', out)

    def test_cancel_stops_when_that_partition_is_gone(self):
        rc, out = self.run_back('PART-INTERNAL', False)
        self.assertNotEqual(rc, 0)
        self.assertIn('STOP the OpenCore partition the Tahoe preparation changed', out)
        self.assertNotIn('C=', out)

    def test_an_old_record_without_the_partition_keeps_detection(self):
        rc, out = self.run_back('', True)
        self.assertEqual(rc, 0)
        self.assertIn('C=/Volumes/1401/EFI/OC/config.plist', out)

if __name__ == '__main__':
    unittest.main()
