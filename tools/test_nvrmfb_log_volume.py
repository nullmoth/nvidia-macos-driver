#!/usr/bin/env python3
"""The framebuffer driver logs start-up, mode changes, errors and warnings to the kernel log; the lines that fire for
every query WindowServer makes or every video memory grant go there only with the boot argument nvfblog=1.

Before, a plain desktop wrote a kernel log line for each getAttribute, setAttribute, getAFC, timing and pixel-format
query and for each video memory grant. The macro is compiled out of the source and run with the switch off and on,
and the query functions are checked to carry no always-on log call.

usage: test_nvrmfb_log_volume.py [repo root]"""
import pathlib, platform, re, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


def function(signature):
    i = src.find(signature)
    if i < 0:
        return ''
    j = src.index('\n}\n', i)
    return src[i:j + 3]


check('a verbose-only log macro exists, switched by the nvfblog boot argument', '#define FBLOGV(' in src and '"nvfblog"' in src)

queries = ['IOReturn NVRMNVDAFramebuffer::getAttribute(', 'IOReturn NVRMNVDAFramebuffer::getAttributeForConnection(',
           'IOReturn NVRMNVDAFramebuffer::setAttribute(', 'IOReturn NVRMNVDAFramebuffer::setAttributeForConnection(',
           'IOReturn NVRMNVDAFramebuffer::getTimingInfoForDisplayMode(', 'IOReturn NVRMNVDAFramebuffer::getPixelInformation(']
for q in queries:
    body = function(q)
    name = q.split('::')[1].rstrip('(')
    check(f'{name} carries no always-on kernel log call', body != '' and 'FBLOG(' not in body,
          f'{len(re.findall(r"FBLOG[(]", body))} left' if body else 'function not found')

check('a successful video memory grant is logged only when verbose',
      'FBLOGV("nvAllocVram #%u' in src and 'FBLOG("nvAllocVram #%u' not in src)
for keep in ('FBLOG("ENUMERATED:', 'FBLOG("kapiInit failed', 'FBLOG("no valid mode")'):
    check(f'start-up and error line kept always on: {keep}', keep in src)

# the macro itself, run
m = re.search(r'#define FBTRACE.*?#define FBLOGV\(.*?while \(0\)\n', src, re.S)
if m and shutil.which('cc') and platform.system() == 'Darwin':
    harness = r'''
#include <stdio.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
static int io, kp, tr, boot_has;
static void IOLog(const char *f, ...) { (void)f; io++; }
static void kprintf(const char *f, ...) { (void)f; kp++; }
static void trace(const char *b) { (void)b; tr++; }
static int PE_parse_boot_argn(const char *n, void *v, int sz) { (void)sz; if (strcmp(n, "nvfblog")) return 0; if (boot_has) *(uint32_t *)v = 1; return boot_has; }
''' + m.group(0) + r'''
int main(void)
{
    FBLOG("always %d", 1);
    printf("log: io %d kprintf %d trace %d\n", io, kp, tr);
    io = kp = tr = 0;
    FBLOGV("per query %d", 2);
    printf("quiet: io %d kprintf %d trace %d\n", io, kp, tr);
    return 0;
}
'''
    harness_on = harness.replace('static int io, kp, tr, boot_has;', 'static int io, kp, tr, boot_has = 1;')
    with tempfile.TemporaryDirectory() as d:
        out = {}
        for tag, h in (('off', harness), ('on', harness_on)):
            pathlib.Path(d, f'{tag}.c').write_text(h)
            r = subprocess.run(['cc', '-w', '-o', f'{d}/{tag}', f'{d}/{tag}.c'], capture_output=True, text=True)
            check(f'the macro compiles ({tag})', r.returncode == 0, r.stderr.strip()[-200:])
            if r.returncode == 0:
                out[tag] = subprocess.run([f'{d}/{tag}'], capture_output=True, text=True).stdout
        if len(out) == 2:
            check('FBLOG always reaches the kernel log and the trace', 'log: io 1 kprintf 1 trace 1' in out['off'], out['off'].split('\n')[0])
            check('FBLOGV without nvfblog stays out of the kernel log but still reaches the trace', 'quiet: io 0 kprintf 0 trace 1' in out['off'], out['off'].split('\n')[1])
            check('FBLOGV with nvfblog=1 reaches the kernel log as well', 'quiet: io 1 kprintf 1 trace 1' in out['on'], out['on'].split('\n')[1])
elif not m:
    check('the macro block can be extracted from the source', False)

print(f'{fails} failed' if fails else 'all passed')
sys.exit(1 if fails else 0)
