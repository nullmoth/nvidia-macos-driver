#!/usr/bin/env python3
"""The per-channel gamma scale attributes are answered (1.0 until set), not left unsupported (issue 413, login-window colour).

CoreDisplay's log at the login window, "can't get framebuffer red gamma scaling factor", is the answer to getAttribute('gslr')
being unsupported. The control is rel-29, where none of the three attributes was handled. Checked on the source because the
handler is three lines around IOFramebuffer's attribute switch; what CoreDisplay then does with the answer needs a login
window on real hardware."""
import pathlib
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['attribute == kIOFBRedGammaScaleAttribute || attribute == kIOFBGreenGammaScaleAttribute || attribute == kIOFBBlueGammaScaleAttribute',
             'fGammaScale[3] = { 0x10000, 0x10000, 0x10000 }', 'if (value) *value = fGammaScale[c];']:
    assert need in src, need
assert src.count('kIOFBRedGammaScaleAttribute') >= 4, 'both getAttribute and setAttribute must handle it'
print('PASS: gslr/gslg/gslb answered with 1.0 (16.16) and remembered when set')
