#!/usr/bin/env python3
"""The D3cold rules (kexts/NVRM/nv_rtd3_policy.h): off unless asked for, only when the GPU is idle with its firmware
suspended, no display lit and no client; never touched while its resource is off. Compiled and run on a table.

usage: test_nv_rtd3_policy.py [repo root]"""
import pathlib, shutil, subprocess, sys, tempfile
root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0
def check(n, ok, d=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + n + (f' ({d})' if d else '')); fails += 0 if ok else 1
hdr = root / 'kexts/NVRM/nv_rtd3_policy.h'
check('the policy header exists', hdr.exists())
if hdr.exists() and shutil.which('cc'):
    src = r'''#include <stdio.h>
#include "nv_rtd3_policy.h"
int main(void) {
 struct nv_rtd3 s = {1, NV_RTD3_ON, 1};
 printf("%d\n", nv_rtd3_may_power_off(&s, 1, 0, 0, 60));
 printf("%d\n", nv_rtd3_may_power_off(&s, 1, 0, 0, 59));
 printf("%d\n", nv_rtd3_may_power_off(&s, 0, 0, 0, 600));
 printf("%d\n", nv_rtd3_may_power_off(&s, 1, 1, 0, 600));
 printf("%d\n", nv_rtd3_may_power_off(&s, 1, 0, 1, 600));
 struct nv_rtd3 off = {0, NV_RTD3_ON, 1}; printf("%d\n", nv_rtd3_may_power_off(&off, 1, 0, 0, 600));
 struct nv_rtd3 nores = {1, NV_RTD3_ON, 0}; printf("%d\n", nv_rtd3_may_power_off(&nores, 1, 0, 0, 600));
 nv_rtd3_off_done(&s, 0); printf("%d\n", s.state == NV_RTD3_ON && nv_rtd3_gpu_reachable(&s));
 nv_rtd3_off_done(&s, 1); printf("%d %d\n", nv_rtd3_gpu_reachable(&s), nv_rtd3_may_power_off(&s, 1, 0, 0, 600));
 nv_rtd3_on_done(&s, 0); printf("%d\n", nv_rtd3_gpu_reachable(&s));
 nv_rtd3_on_done(&s, 1); printf("%d\n", nv_rtd3_gpu_reachable(&s));
 return 0; }
'''
    with tempfile.TemporaryDirectory() as d:
        pathlib.Path(d, 't.c').write_text(src)
        r = subprocess.run(['cc', '-I', str(hdr.parent), '-o', d + '/t', d + '/t.c'], capture_output=True, text=True)
        check('the header compiles', r.returncode == 0, r.stderr[-200:])
        if r.returncode == 0:
            o = subprocess.run([d + '/t'], capture_output=True, text=True).stdout.split('\n')
            check('idle 60 s, firmware suspended, no head, no client: may power off', o[0] == '1')
            check('59 s of idle is not enough', o[1] == '0')
            check('with the firmware not suspended it never powers off', o[2] == '0')
            check('a lit display head blocks it', o[3] == '0')
            check('an open client blocks it', o[4] == '0')
            check('without the nvrtd3 switch it never powers off', o[5] == '0')
            check('without a power resource path it never powers off', o[6] == '0')
            check('a failed _OFF leaves the GPU on and reachable', o[7] == '1')
            check('after _OFF the GPU is unreachable and cannot be powered off again', o[8] == '0 0')
            check('a failed _ON keeps the GPU marked unreachable', o[9] == '0')
            check('after _ON the GPU is reachable again', o[10] == '1')

if hdr.exists() and shutil.which('cc'):
    seq = r"""#include <stdio.h>
#include <string.h>
#include "nv_rtd3_policy.h"
static char log[256]; static int heads, clients, off_ok = 1, on_ok = 1;
static void add(const char *w) { strcat(log, w); strcat(log, " "); }
static int h(void *c) { (void)c; return heads; } static int cl(void *c) { (void)c; return clients; }
static void su(void *c) { (void)c; add("suspend"); } static void re(void *c) { (void)c; add("resume"); }
static int off(void *c) { (void)c; add("acpi_off"); return off_ok; } static int on(void *c) { (void)c; add("acpi_on"); return on_ok; }
int main(void) {
 struct nv_rtd3_ops o = { 0, h, cl, su, re, off, on };
 struct nv_rtd3 s = {1, NV_RTD3_ON, 1}; unsigned idle = 0; int r = 0;
 for (int i = 0; i < 59; i++) r |= nv_rtd3_tick(&s, &o, &idle, 1);
 printf("A %d [%s]\n", r, log);
 r = nv_rtd3_tick(&s, &o, &idle, 1); printf("B %d [%s] state %d\n", r, log, s.state);
 log[0] = 0; r = nv_rtd3_tick(&s, &o, &idle, 1); printf("C %d [%s]\n", r, log);
 clients = 1; r = nv_rtd3_tick(&s, &o, &idle, 1); printf("D %d [%s] state %d\n", r, log, s.state);
 clients = 0; log[0] = 0; for (int i = 0; i < 60; i++) nv_rtd3_tick(&s, &o, &idle, 1); printf("E [%s] state %d\n", log, s.state);
 log[0] = 0; on_ok = 0; heads = 1; r = nv_rtd3_tick(&s, &o, &idle, 1); printf("F %d [%s] state %d\n", r, log, s.state);
 on_ok = 1; log[0] = 0; r = nv_rtd3_tick(&s, &o, &idle, 1); printf("G %d [%s] state %d\n", r, log, s.state);
 heads = 0; off_ok = 0; log[0] = 0; for (int i = 0; i < 60; i++) r = nv_rtd3_tick(&s, &o, &idle, 1); printf("H %d [%s] state %d\n", r, log, s.state);
 struct nv_rtd3 d = {0, NV_RTD3_ON, 1}; unsigned i2 = 0; log[0] = 0; for (int i = 0; i < 600; i++) nv_rtd3_tick(&d, &o, &i2, 1); printf("I [%s]\n", log);
 return 0; }
"""
    with tempfile.TemporaryDirectory() as d:
        pathlib.Path(d, 's.c').write_text(seq)
        r = subprocess.run(['cc', '-I', str(hdr.parent), '-o', d + '/s', d + '/s.c'], capture_output=True, text=True)
        check('the call-sequence harness compiles', r.returncode == 0, r.stderr[-200:])
        if r.returncode == 0:
            o = {l[0]: l[2:] for l in subprocess.run([d + '/s'], capture_output=True, text=True).stdout.strip().split('\n')}
            check('59 idle seconds do nothing', o['A'] == '0 []')
            check('the 60th suspends the driver, then switches the resource off, in that order', o['B'] == '1 [suspend acpi_off ] state 1')
            check('while off and idle nothing is called', o['C'] == '0 []')
            check('a client while off switches the resource on first and resumes after it', o['D'] == '2 [acpi_on resume ] state 0')
            check('after waking, another 60 idle seconds powers off again', o['E'] == '[suspend acpi_off ] state 1')
            check('a failed _ON leaves the GPU off and does not resume it', o['F'] == '-2 [acpi_on ] state 1')
            check('the next try that works resumes', o['G'] == '2 [acpi_on resume ] state 0')
            check('a failed _OFF resumes the driver at once and keeps the GPU on', o['H'].startswith('-1') and 'suspend acpi_off resume' in o['H'] and o['H'].endswith('state 0'))
            check('with the switch off nothing is ever called', o['I'] == '[]')
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
