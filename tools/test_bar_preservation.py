#!/usr/bin/env python3
"""Replay the production installation plan with reported BAR and relocation settings."""
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

root = pathlib.Path(__file__).resolve().parents[1]
source = (root/'app/Resources/nullmoth-setup.sh').read_text()
start = source.index('# Existing BAR and kernel-placement settings')
end = source.index('# The installer runs on the firmware framebuffer', start)
plan = source[start:end]

class PreserveBootSettings(unittest.TestCase):
    def check(self, bar, apple, relocation):
        cfg = {'UEFI': {'Quirks': {'ResizeGpuBars': bar}},
               'Booter': {'Quirks': {'ResizeAppleGpuBars': apple,
                'ProvideCustomSlide': True, 'AvoidRuntimeDefrag': True,
                'AllowRelocationBlock': relocation}}}
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory)/'config.plist'
            path.write_bytes(plistlib.dumps(cfg))
            script = '''C=$1
get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }
note() { :; }
EDITS=()
''' + plan + '''
for edit in "${EDITS[@]}"; do
  IFS='|' read -r key type value <<<"$edit"
  plutil -replace "$key" "$type" "$value" "$C" || exit 1
done
'''
            subprocess.run(['/bin/bash','-c',script,'plan',str(path)], check=True, timeout=30)
            self.assertEqual(plistlib.loads(path.read_bytes()),cfg)
    def test_reported_small_bar_configuration_is_preserved(self): self.check(-1,0,False)
    def test_existing_large_bar_configuration_is_preserved(self): self.check(13,-1,True)
    def test_existing_relocation_choice_is_preserved(self): self.check(-1,0,True)

if __name__ == '__main__': unittest.main()
