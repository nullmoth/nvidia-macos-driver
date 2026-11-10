#!/usr/bin/env python3
"""A release plugin must not act on world-writable /tmp request files, and no file compiled into it may bypass that.

Arms (named by what they prove):
  1. the release access()/fopen() wrappers refuse every /tmp/nvmtl- path even when the file exists, and still serve others
  2. every translation unit of the release build that names a /tmp/nvmtl- trigger is either NVMTLDevice.m or a file it
     includes after the wrappers are defined, so the wrappers apply to it
  3. the trace switch returns 0 in a release build before it looks at any file
"""
import os
import pathlib
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
plugin = root / 'plugin'
device = (plugin / 'NVMTLDevice.m').read_text()

# arm 1: lift the wrapper block and run it
start = device.index('static inline int nvmtl_rel_devpath(const char *p)')
end = device.index('#define fopen(p, m) nvmtl_rel_fopen(p, m)', start) + len('#define fopen(p, m) nvmtl_rel_fopen(p, m)')
block = device[start:end]
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    src = d / 'guard.c'
    exe = d / 'guard'
    src.write_text('#include <fcntl.h>\n#include <stdio.h>\n#include <string.h>\n#include <unistd.h>\n' + block + '''
int main(int argc, char **argv) {
    int a = access(argv[1], F_OK);
    FILE *f = fopen(argv[1], "r");
    printf("%d %d\\n", a, f != NULL);
    return 0;
}
''')
    subprocess.run(['cc', str(src), '-o', str(exe)], check=True)
    planted = pathlib.Path('/tmp/nvmtl-test-release-trigger')
    planted.write_text('x')
    other = d / 'ordinary-file'
    other.write_text('x')
    try:
        for path, want in [(str(planted), '-1 0'), ('/private' + str(planted), '-1 0'),
                           (str(other), '0 1')]:
            got = subprocess.check_output([str(exe), path], text=True).strip()
            assert got == want, (path, got, want)
    finally:
        planted.unlink(missing_ok=True)

# arm 2: which files name a trigger, and are they inside the guarded translation unit
guard_at = device.index('#define access(p, m) nvmtl_rel_access(p, m)')
included = set()
for m in re.finditer(r'^\s*#\s*(?:include|import)\s+"([^"]+)"', device, re.M):
    if device.index(m.group(0)) > guard_at:
        included.add(m.group(1))
for f in sorted(plugin.rglob('*')):
    if f.suffix not in ('.m', '.c', '.h', '.mm') or not f.is_file():
        continue
    text = f.read_text(errors='replace')
    if '/tmp/nvmtl-' not in text and '/private/tmp/nvmtl-' not in text:
        continue
    rel = str(f.relative_to(plugin))
    assert f.name == 'NVMTLDevice.m' or rel in included or f.name in included, \
        f'{rel} names a /tmp/nvmtl- trigger but is compiled outside the guarded translation unit'

# arm 3: the trace switch is off in a release build before it reads anything
objects = (plugin / 'NVMTLObjects.m').read_text()
at = objects.index('static int nvmtl_trace_on(void) {')
head = objects[at:at + 300]
assert '#if NVMTL_RELEASE' in head and 'return 0;' in head, head
print('release dev-trigger guards: ok')
