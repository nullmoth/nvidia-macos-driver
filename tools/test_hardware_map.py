"""Compile and run the production device map and bounded worker without network or driver changes."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class HardwareMapTests(unittest.TestCase):
    def test_production_functions_and_worker_lifecycle(self):
        with tempfile.TemporaryDirectory() as directory:
            exe = Path(directory) / 'hardware-map-test'
            subprocess.run(['xcrun', 'swiftc', '-O', '-framework', 'IOKit',
                            str(ROOT / 'app/Sources/profile.swift'), str(ROOT / 'app/Sources/HardwareMap.swift'), str(ROOT / 'app/Sources/HardwareMapWorker.swift'),
                            str(ROOT / 'tools/hardware_map_tests/main.swift'), '-o', str(exe)], check=True, capture_output=True)
            result = subprocess.run([str(exe)], check=True, capture_output=True, text=True, timeout=20)
            self.assertIn('Owned worker timeout, crash, output and shape checks: passed', result.stdout)

    def test_consent_priority_and_exact_native_command(self):
        source = (ROOT / 'app/Sources/main.swift').read_text()
        send = source.split('    func sendLogs(sample: String = "") {', 1)[1].split('    func crashReportFromWindow()', 1)[0]
        self.assertLess(send.index('guard a.runModal()'), send.index('HardwareMapWorker.run'))
        self.assertLess(send.index('DispatchQueue.global().async'), send.index('HardwareMapWorker.run'))
        self.assertIn('let important = ["hardware-map.json",', send)
        self.assertIn('if argv == [argv[0], "--hardware-map"]', source)
        self.assertNotIn('.load(as: UInt32.self)', source.split('func u32(', 1)[1].split('func str(', 1)[0])
        self.assertIn('HardwareMap.integer(prop(e, k)', source)


if __name__ == '__main__': unittest.main()
