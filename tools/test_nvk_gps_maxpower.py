#!/usr/bin/env python3
"""NVRM_610_MAXPOWER_MW sends the public GPS_CONTROL SYS_SET_MAX_POWER command (and reads it back) and sends nothing when unset
or out of range. The block is compiled out of nvk/nvk-gps-maxpower.patch against a recording stand-in for the resource manager.
usage: test_nvk_gps_maxpower.py [patch]"""
import pathlib, re, shutil, subprocess, sys, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
patch = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'nvk/nvk-gps-maxpower.patch'
fails = 0
def check(n, ok, d=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + n + (f' ({d})' if d else '')); fails += 0 if ok else 1
added = '\n'.join(l[1:] for l in patch.read_text().split('\n') if l.startswith('+') and not l.startswith('+++'))
m = re.search(r'   \{\n      const char \*mw = nvmtl_cfg_getenv.*?\n   \}\n', added, re.S)
check('the patch carries the MAXPOWER block', m is not None)
if m and shutil.which('cc'):
    h = r'''
#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
typedef unsigned NV_STATUS, NvHandle, NvU32; typedef unsigned short NvU16;
enum { NV_OK = 0, NV0000_CTRL_CMD_SYSTEM_GPS_CONTROL = 0x122, NV0000_CTRL_CMD_SYSTEM_GPS_SYS_SET_MAX_POWER = 0x100, NV0000_CTRL_CMD_SYSTEM_GPS_SYS_GET_MAX_POWER = 0x101 };
typedef struct { NvU16 command; NvU16 locale; NvU32 data; } NV0000_CTRL_SYSTEM_GPS_CONTROL_PARAMS;
struct NvRmApi { int fd; };
static int calls, setv; static unsigned cmds[4]; static unsigned store;
static NV_STATUS nvRmApiControl(struct NvRmApi *r, NvHandle h, unsigned cmd, void *p, unsigned n)
{ NV0000_CTRL_SYSTEM_GPS_CONTROL_PARAMS *g = p; (void)r; (void)h; (void)n; if (cmd != NV0000_CTRL_CMD_SYSTEM_GPS_CONTROL) return 1;
  if (calls < 4) cmds[calls] = g->command; calls++; if (g->command == 0x100) { store = g->data; setv = (int)g->data; } else if (g->command == 0x101) g->data = store; return 0; }
static const char *nvmtl_cfg_getenv(const char *k) { (void)k; return getenv("T"); }
static bool nvkmd_nvrm_diag_on(void) { return false; }
struct P { NvHandle hClient; };
int main(void) { struct NvRmApi rm = {0}; struct P pd = {0}, *pdev = &pd;
''' + m.group(0) + r'''
 printf("%d %u %u %d\n", calls, cmds[0], cmds[1], setv); return 0; }
'''
    with tempfile.TemporaryDirectory() as d:
        pathlib.Path(d, 'h.c').write_text(h)
        r = subprocess.run(['cc', '-w', '-o', d + '/h', d + '/h.c'], capture_output=True, text=True)
        check('the block compiles', r.returncode == 0, r.stderr[-200:])
        if r.returncode == 0:
            def run(v):
                return subprocess.run([d + '/h'], capture_output=True, text=True, env={'T': v} if v is not None else {}).stdout.split()
            check('unset sends nothing', run(None)[0] == '0')
            ok = run('80000')
            check('80000 mW sends SET (0x100) then GET (0x101) with the value', ok == ['2', '256', '257', '80000'], ' '.join(ok))
            check('a value under 1 W is refused', run('500')[0] == '0')
            check('a value over 1 kW is refused', run('2000000')[0] == '0')
            check('text is refused', run('lots')[0] == '0')
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
