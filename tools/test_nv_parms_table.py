#!/usr/bin/env python3
"""nv_parms is NVIDIA's module-parameter table, and rm_parse_option_string() (the nvregdwords= parser) walks it by name.

It used to be one of the weak stub functions in nv_xnu_stubs.cpp, so the walk read the stub's machine code as string
pointers and any nvregdwords= boot-arg stopped the boot in strcmp (general protection fault under NVRM::go). The arms:
the symbol is data, its last entry is the NULL terminator, and NVIDIA's walk, compiled here against the table lifted
from the source, reaches the end and sends an unknown key to the registry. The negative control runs the same walk
over the old stub's form - a function's bytes - and must not find a clean NULL terminator in the first entry.
An NVRM binary given as the first argument is also checked: _nv_parms must sit in __DATA, not __TEXT."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
stubs = (root / 'kexts/NVRM/nv_xnu_stubs.cpp').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


check('no stub function is named nv_parms', re.search(r'\bnv_parms\s*\(\s*void\s*\)', stubs) is None)
table = re.search(r'struct\s+(\w+)\s*\{\s*const char \*name;\s*NvU32 \*data;\s*\};\s*'
                  r'__attribute__\(\(used\)\)\s*struct\s+\1\s+nv_parms\[\]\s*=\s*(\{.*?\});', stubs, re.S)
check('nv_parms is a {name, data} table', table is not None)
body = table.group(2) if table else ''
check('its last entry is the NULL terminator', re.search(r'\{\s*NULL\s*,\s*NULL\s*\}\s*\}$', body) is not None, body)

# NVIDIA's walk (osapi.c rm_parse_option_string), against the lifted table
program = r'''
#include <stdio.h>
#include <string.h>
typedef unsigned int NvU32;
struct nv_xnu_parm { const char *name; NvU32 *data; };
struct nv_xnu_parm nv_parms[] = %s;
static int registry_writes;
static void rm_write_registry_dword(const char *name, NvU32 data) { (void)name; (void)data; registry_writes++; }
int main(void) {
    const char *names[] = { "RmDisableDisplayGlitchPerfLimit", "PreserveVideoMemoryAllocations" };
    for (int k = 0; k < 2; k++) {
        struct nv_xnu_parm *entry; unsigned i;
        for (i = 0; (entry = &nv_parms[i])->name != NULL; i++)
            if (strcmp(entry->name, names[k]) == 0) break;
        if (!entry->name) rm_write_registry_dword(names[k], 1); else *entry->data = 1;
    }
    printf("registry_writes=%%d\n", registry_writes);
    return 0;
}
''' % (body or '{ }')
with tempfile.TemporaryDirectory() as d:
    src, exe = pathlib.Path(d) / 'walk.c', pathlib.Path(d) / 'walk'
    src.write_text(program)
    built = subprocess.run(['cc', '-O1', '-o', str(exe), str(src)], capture_output=True, text=True)
    check('NVIDIA\'s walk compiles against the table', built.returncode == 0, built.stderr[-300:])
    if built.returncode == 0:
        run = subprocess.run([str(exe)], capture_output=True, text=True, timeout=10)
        check('the walk ends at the terminator and sends both keys to the registry',
              run.returncode == 0 and 'registry_writes=2' in run.stdout, run.stdout + run.stderr)

    # negative control: the old form, a function, read as the table's first entry
    control = r'''
#include <stdio.h>
#include <stdint.h>
#include <string.h>
__attribute__((noinline)) long nv_parms_stub(void) { return 0; }
int main(void) {
    const void *name; memcpy(&name, (const void *)(uintptr_t)&nv_parms_stub, sizeof name);
    printf("first_name_is_null=%d\n", name == NULL);
    return 0;
}
'''
    csrc, cexe = pathlib.Path(d) / 'control.c', pathlib.Path(d) / 'control'
    csrc.write_text(control)
    if subprocess.run(['cc', '-O1', '-o', str(cexe), str(csrc)], capture_output=True).returncode == 0:
        out = subprocess.run([str(cexe)], capture_output=True, text=True, timeout=10).stdout
        check('negative control: a stub function\'s bytes are no NULL-terminated table (the old crash)',
              'first_name_is_null=0' in out, out.strip())

if len(sys.argv) > 1:
    nm = subprocess.run(['nm', '-m', sys.argv[1]], capture_output=True, text=True).stdout
    line = next((l for l in nm.splitlines() if l.endswith(' _nv_parms')), '')
    check('the built NVRM keeps _nv_parms in __DATA', '(__DATA' in line, line.strip())

print(f'{fails} failed')
sys.exit(1 if fails else 0)
