#!/usr/bin/env python3
"""When a GPU with no display of its own hands its Metal device to apps. On a laptop whose screen is on the other GPU,
NVKMS lists the card's connectors, none connected, about 7 s after start; the plugin name used to come back only at the
60 s deadline, and apps opened before then never saw the NVIDIA device. It now comes back as soon as NVKMS reports no
display on this card AND another GPU's framebuffer shows the desktop. A desktop with a slow monitor keeps the deadline."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


def body(sig):
    i = src.index(sig)
    j = src.index('\n    }\n', i)
    return src[i:j]


check('NVRMFB writes connected-displays on the nub it enumerated', 'setProperty("connected-displays", (unsigned long long)gNDpy' in fb)
check('the deadline is still 60 s', 'kHeadlessDeadlineSec = 60;' in src)
m = re.search(r'kHeadlessPollSec = (\d+);', src)
check('the check polls every few seconds, well under the ~7 s NVKMS takes', m is not None and 1 <= int(m.group(1)) <= 3)
arm = body('void armHeadlessCheck()')
check('arming schedules the first poll, not the deadline', 'clock_interval_to_deadline(kHeadlessPollSec' in arm
      and 'clock_interval_to_deadline(kHeadlessDeadlineSec' not in arm)
check('-nvaccelnoheadless still turns the check off', '-nvaccelnoheadless' in arm)
nd = body('static bool headlessNoDisplayHere()')
check('"no display here" needs at least one enumerated nub and zero displays on all', 'seen && none' in nd
      and 'cd->unsigned32BitValue() > 0' in nd)
own = body('static void headlessDisplayOwner(')
check('another GPU = a framebuffer whose PCI device is not vendor 0x10de', '!= 0x10de' in own and '"vendor-id"' in own)
check('our own heads never count as the other GPU', 'metaCast("NVRMNVDAFramebuffer")) continue' in own)
check('the Intel panel driver (a head of this accelerator) never triggers the early hand-over',
      'metaCast("NMIntelFB")) break' in own and own.index('NMIntelFB') < own.index('vendor-id'))
hc = body('static void headlessCheck(void *p0, void *)')
check('early hand-over needs both conditions', 'const bool early = other && headlessNoDisplayHere();' in hc)
check('without them it polls again until the deadline', 'if (!early && me->fHeadlessWaited < kHeadlessDeadlineSec)' in hc
      and 'thread_call_enter_delayed(me->fHeadlessCall' in hc)
check('the re-poll path releases the registration lock', hc.count('IOLockUnlock(me->fRegLock)') >= 2)
check('a published head still stops the check', 'me->fFB ? "a display head published"' in hc)
check('the hand-over never arms WindowServer', 'NVAccelHeadless' in hc and 'nvaccelfb' not in hc.split('const bool early')[1])
# 2.5.3 field logs: a desktop whose only GPU is this card had the name restored at the deadline, CoreDisplay asked the
# plugin for WindowServer's device, the plugin refused a headless GPU, and WindowServer aborted every ten seconds.
dl = hc.split('if (!early && me->fHeadlessWaited < kHeadlessDeadlineSec)')[1]
withhold = dl.find('if (!other && strcmp(owner, "NMIntelFB") != 0)')
check('at the deadline with no other GPU showing the desktop, the plugin name stays withheld',
      withhold != -1 and withhold < dl.index('setProperty("MetalPluginName"'))
check('and that path releases the registration lock and returns before the hand-over',
      'IOLockUnlock(me->fRegLock);\n                return;' in dl[withhold:dl.index('setProperty("MetalPluginName"')])
check('the Intel panel driver case keeps the hand-over', 'strcmp(owner, "NMIntelFB") != 0' in dl)
sys.exit(1 if fails else 0)
