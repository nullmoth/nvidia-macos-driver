#!/usr/bin/env python3
"""Which processes hand video decode to VTDecoderXPCService instead of registering the decoder in-process. Only Apple's
own sandboxed apps do (Safari's WebKit GPU process, QuickTime Player, Preview); a sandboxed Chromium or Electron GPU
process (Discord) must keep its in-process hardware decoder, or its videos do not play. The plugin's own functions are
compiled into two small programs, one carrying an Apple bundle identifier and one Discord's, and run inside and outside
a sandbox."""
import pathlib
import platform
import shutil
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLDevice.m').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


def function(signature):
    i = src.index(signature)
    return src[i:src.index('\n}\n', i) + 3]


if platform.system() != 'Darwin' or not shutil.which('clang') or not pathlib.Path('/usr/bin/sandbox-exec').exists():
    print('skip: needs macOS with clang and sandbox-exec')
    sys.exit(0)

body = ''.join(function(s) for s in ('static bool nvmtl_is_sandboxed(void)',
                                       'static bool nvmtl_is_decode_service(const char *me)',
                                       'static bool nvmtl_decode_is_delegated(const char *me)'))
harness = ('#include <CoreFoundation/CoreFoundation.h>\n#include <dispatch/dispatch.h>\n#include <dlfcn.h>\n'
           '#include <stdbool.h>\n#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n#include <unistd.h>\n'
           + body + 'int main(void) { printf("%d\\n", nvmtl_decode_is_delegated(getprogname())); return 0; }\n')
plist = ('<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key>'
         '<string>{}</string></dict></plist>')

with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / 'h.c').write_text(harness)
    progs = {}
    for name, ident in (('apple', 'com.apple.Preview'), ('discord', 'com.hnc.Discord')):
        (d / f'{name}.plist').write_text(plist.format(ident))
        r = subprocess.run(['clang', '-o', str(d / name), str(d / 'h.c'), '-framework', 'CoreFoundation',
                            '-Wl,-sectcreate,__TEXT,__info_plist,' + str(d / f'{name}.plist')],
                           capture_output=True, text=True, timeout=120)
        check(f'the {name} probe builds from the plugin source', r.returncode == 0, r.stderr.strip()[-300:])
        progs[name] = d / name

    def run(prog, sandboxed):
        cmd = ['/usr/bin/sandbox-exec', '-p', '(version 1)(allow default)', str(prog)] if sandboxed else [str(prog)]
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        return r.stdout.strip()

    if all(p.exists() for p in progs.values()):
        check('a sandboxed Apple app delegates decode to VTDecoderXPCService', run(progs['apple'], True) == '1')
        check('a sandboxed Discord keeps its in-process hardware decoder', run(progs['discord'], True) == '0')
        check('control: an unsandboxed Apple app registers in-process', run(progs['apple'], False) == '0')
        check('control: an unsandboxed Discord registers in-process', run(progs['discord'], False) == '0')
sys.exit(1 if fails else 0)
