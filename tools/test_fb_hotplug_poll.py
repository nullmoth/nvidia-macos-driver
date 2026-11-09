#!/usr/bin/env python3
"""Run-time plug and unplug can be noticed by polling (nvfbhotplugpoll=<seconds>, off by default), for GPUs that deliver no display-change event.

Issue 327: a GTX 1660 Ti laptop with mini-DP and HDMI on the GPU never sees a display added or removed while running. The poll re-arms a
thread call that kicks the same scan the event runs. Period validation is compiled from nvrm-fb-safety.h; the wiring is checked in
nvrm-fb.cpp. The control is rel-29, where only the event starts the scan."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['PE_parse_boot_argn("nvfbhotplugpoll"', 'void NVRMNVDAFramebuffer::hotplugPollFire', 'thread_call_enter_delayed(gHotplugPoll, d)', 'hotplugKick();']:
    assert need in src, need
fixture = '#include <cassert>\n#include <cstdio>\n#include "nvrm-fb-safety.h"\nint main(){ assert(!nvrmHotplugPollValid(0) && nvrmHotplugPollValid(1) && nvrmHotplugPollValid(60) && !nvrmHotplugPollValid(61)); puts("PASS: poll period 1..60 s, wiring present"); }\n'
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
