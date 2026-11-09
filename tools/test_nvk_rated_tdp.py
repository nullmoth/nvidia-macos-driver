#!/usr/bin/env python3
"""NVRM_610_RATEDTDP caps the card at its rated TDP performance state through the public RATED_TDP control, and does nothing
when it is unset. The block is compiled out of nvk/nvk-rated-tdp.patch with a recording stand-in for the resource manager.

usage: test_nvk_rated_tdp.py [patch]"""
import pathlib, re, shutil, subprocess, sys, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
patch = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'nvk/nvk-rated-tdp.patch'
fails = 0
def check(n, ok, d=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + n + (f' ({d})' if d else '')); fails += 0 if ok else 1
added = '\n'.join(l[1:] for l in patch.read_text().split('\n') if l.startswith('+') and not l.startswith('+++'))
m = re.search(r'   \{\n      int tv = nvrm610_value\("RATEDTDP"\);.*?\n   \}\n', added, re.S)
check('the patch carries the RATEDTDP block', m is not None)
if m and shutil.which('cc'):
    h = r'''
#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
typedef unsigned NV_STATUS; typedef unsigned NvHandle;
typedef enum { NV2080_CTRL_PERF_RATED_TDP_ACTION_DEFAULT, NV2080_CTRL_PERF_RATED_TDP_ACTION_FORCE_EXCEED, NV2080_CTRL_PERF_RATED_TDP_ACTION_FORCE_LIMIT, NV2080_CTRL_PERF_RATED_TDP_ACTION_FORCE_LOCK } NV2080_CTRL_PERF_RATED_TDP_ACTION;
enum { NV2080_CTRL_PERF_RATED_TDP_CLIENT_GLOBAL = 2, NV2080_CTRL_PERF_VPSTATE_RATED_TDP = 0, NV2080_CTRL_CMD_PERF_RATED_TDP_SET_CONTROL = 0x2080206f };
typedef struct { int client; int input; int vPstateType; } NV2080_CTRL_PERF_RATED_TDP_SET_CONTROL_PARAMS;
struct NvRmApi { int fd; };
static int calls, last_client, last_action, last_vp; static unsigned last_cmd;
static NV_STATUS nvRmApiControl(struct NvRmApi *r, NvHandle h, unsigned cmd, void *p, unsigned n)
{ NV2080_CTRL_PERF_RATED_TDP_SET_CONTROL_PARAMS *t = p; calls++; last_cmd = cmd; last_client = t->client; last_action = t->input; last_vp = t->vPstateType; (void)r; (void)h; (void)n; return 0; }
static int nvrm610_value(const char *item) { const char *e = getenv("T"); (void)item; return e ? (unsigned char)e[0] : -1; }
static bool nvkmd_nvrm_diag_on(void) { return false; }
struct P { NvHandle hSubdevice; };
int main(void) { struct NvRmApi rm = {0}; struct P pd = {0}, *pdev = &pd;
''' + m.group(0) + r'''
 printf("%d %u %d %d %d\n", calls, last_cmd, last_client, last_action, last_vp); return 0; }
'''
    with tempfile.TemporaryDirectory() as d:
        pathlib.Path(d, 'h.c').write_text(h)
        r = subprocess.run(['cc', '-w', '-o', d + '/h', d + '/h.c'], capture_output=True, text=True)
        check('the block compiles', r.returncode == 0, r.stderr[-200:])
        if r.returncode == 0:
            def run(v):
                env = {'T': v} if v is not None else {}
                return subprocess.run([d + '/h'], capture_output=True, text=True, env=env).stdout.split()
            check('unset asks for nothing', run(None)[0] == '0')
            check('0 asks for nothing', run('0')[0] == '0')
            lim = run('limit')
            check('limit sends FORCE_LIMIT for the GLOBAL client on the rated TDP vPstate', lim == ['1', '2150531183', '2', '2', '0'] or lim == ['1', str(0x2080206f), '2', '2', '0'], ' '.join(lim))
            check('k sends FORCE_LOCK', run('k')[3] == '3')
            check('exceed sends FORCE_EXCEED', run('x')[3] == '1')
            check('an unknown value asks for nothing', run('banana')[0] == '0')
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
