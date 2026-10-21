#!/usr/bin/env python3
"""Compile production connector/CEA helpers and check NVRMFB mapping lifecycle."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text(encoding='utf-8')
eld = (root / 'kexts/NVRMFB/nvrm_eld.h').read_text(encoding='utf-8')

def extract(text, marker):
    start = text.index(marker)
    opening = text.index('{', start)
    depth = 0
    for pos in range(opening, len(text)):
        if text[pos] == '{': depth += 1
        elif text[pos] == '}':
            depth -= 1
            if depth == 0: return text[start:pos + 1]
    raise AssertionError(f'unclosed production function: {marker}')

scan = extract(eld, 'static inline void nvrm_scan_cea(')
connector = extract(fb, 'static NvU32 nvrmHdaConnectorType(')
publish = extract(fb, 'void NVRMNVDAFramebuffer::publishDisplayAudioProperties()')
clear = extract(fb, 'void NVRMNVDAFramebuffer::clearDisplayAudioProperties()')
assert 'NVRMFBDisplayAudioCodecInfo' in publish
assert '{ 0x00, 0x01, 0x04, 0x00 }' in publish   # the default when no mapping is configured
assert 'if (!cea.has_audio || !connectorType || !hasPairing)' in publish
assert 'for (size_t e = 1; e <= edid[126]; e++)' in scan
assert all(f'"{key}"' in publish for key in ('connector-type', 'port-number', 'av-signal-type', 'audio-codec-info', 'IODisplayEDID'))
assert all(f'removeProperty("{key}")' in clear for key in ('nvda-eld', 'hda-gfx', 'connector-type', 'port-number', 'av-signal-type', 'audio-codec-info', 'IODisplayEDID'))
assert 'metaCast("IOPCIDevice")' in fb and 'OSDynamicCast(OSData, value)' in fb and 'OSDynamicCast(OSString, value)' in fb
assert 'fb->clearDisplayAudioProperties()' in fb and 'fb->publishDisplayAudioProperties()' in fb

fixture = r'''
#include <cassert>
#include <cstddef>
#include <cstdint>
using NvU32 = uint32_t;
enum { NVKMS_CONNECTOR_SIGNAL_FORMAT_DP = 1, NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS = 2, NVKMS_CONNECTOR_TYPE_HDMI = 3 };
struct NvKmsKapiConnectorInfo { int signalFormat; int type; };
enum { NVRM_CEA_TAG_AUDIO = 1, NVRM_CEA_TAG_SPEAKER = 4, NVRM_CEA_TAG_VENDOR = 3 };
struct nvrm_cea_scan { uint8_t sad[15 * 3]; uint8_t sad_count, speaker_alloc, has_audio, is_hdmi; };
'''
checks = r'''
int main() {
    NvKmsKapiConnectorInfo dp{NVKMS_CONNECTOR_SIGNAL_FORMAT_DP, 0};
    NvKmsKapiConnectorInfo hdmi{NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS, NVKMS_CONNECTOR_TYPE_HDMI};
    NvKmsKapiConnectorInfo dvi{NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS, 0};
    assert(nvrmHdaConnectorType(&dp) == 0x400);
    assert(nvrmHdaConnectorType(&hdmi) == 0x800);
    assert(nvrmHdaConnectorType(&dvi) == 0);
    uint8_t edid[256] = {};
    edid[126] = 1; edid[128] = 2; edid[129] = 3; edid[130] = 8;
    edid[132] = (1 << 5) | 3; edid[133] = 9; edid[134] = 7; edid[135] = 7;
    struct nvrm_cea_scan cea{};
    nvrm_scan_cea(edid, sizeof edid, &cea);
    assert(cea.has_audio && cea.sad_count == 1 && cea.sad[0] == 9);
    edid[132] = (1 << 5) | 2; edid[130] = 7;
    nvrm_scan_cea(edid, sizeof edid, &cea);
    assert(!cea.has_audio && cea.sad_count == 0);
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    test = path / 'check.cpp'
    test.write_text(fixture + scan + connector + checks, encoding='utf-8')
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Wextra', str(test), '-o', str(path / 'check')], check=True)
    subprocess.run([str(path / 'check')], check=True)
print('PASS: production connector and CEA helpers; mapping defaults to pin 4 unless configured, needs SADs, real connector type, and GPU pairing; disconnect clears stale properties')
