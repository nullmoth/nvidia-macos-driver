#!/usr/bin/env python3
"""A refused command buffer and a refused NVKMS ioctl each name their cause.

Two silent refusals ended the same way in the field: WindowServer aborted. The plugin returned nil from
-[MTLCommandQueue commandBuffer] with no cause (the backend had been disabled by an earlier lost device), and the
kernel's NVKMS ioctl returned false with the reply status still "success", which the framebuffer could only show as
"accepted 0, flipResult 0". This replays the decision helpers the log lines are built from and checks that the call
sites use them.
"""
import pathlib
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
plugin = root / 'plugin'
kms = (root / 'kexts/NVRM/os-nvkms-xnu.cpp').read_text()
vk = (plugin / 'nvmtl_vk.c').read_text()
queue = (plugin / 'NVMTLObjects.m').read_text()


def cut(text, start, end):
    i = text.index(start)
    return text[i:text.index(end, i)]


# ---- the kernel helpers, extracted from the source and compiled -------------------------------------------------
helpers = cut(kms, 'enum { kKmsCmdSetMode', 'static NvBool kms_ioctl_common(')
kms_fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
enum { kKmsPmRunning = 0, kKmsPmSuspended = 1, kKmsPmLost = 2 };
''' + helpers + r'''
int main() {
    // the refusals, by stage
    assert(kms_ioctl_refusal(false, false, false, false) == kKmsRefuseNoOpen);
    assert(kms_ioctl_refusal(false, true, true, true) == kKmsRefuseNoOpen);        // no device wins over everything
    assert(kms_ioctl_refusal(true, false, true, false) == kKmsRefusePmTry);        // flips and mode sets while asleep
    assert(kms_ioctl_refusal(true, false, false, false) == kKmsRefusePmWait);      // waited for a resume that never came
    assert(kms_ioctl_refusal(true, true, true, false) == kKmsRefuseIoctl);         // reached NVKMS, which said no
    assert(kms_ioctl_refusal(true, true, false, true) == kKmsRefuseNone);          // success is not a refusal
    // every stage has a name a tester can read
    for (int i = kKmsRefuseNoOpen; i <= kKmsRefuseIoctl; i++) assert(kKmsRefuseName[i] && strlen(kKmsRefuseName[i]) > 3);
    assert(strstr(kKmsRefuseName[kKmsRefuseNoOpen], "no open"));
    assert(strstr(kKmsRefuseName[kKmsRefusePmTry], "try-lock"));
    assert(strstr(kKmsRefuseName[kKmsRefuseIoctl], "ioctl returned false"));
    // the command numbers the log line is documented against
    assert(kKmsCmdSetMode == 9 && kKmsCmdFlip == 15);
    // log volume: the first 8, then every 64th, never zero-th
    int said = 0; for (unsigned n = 1; n <= 1000; n++) said += kms_refusal_logworthy(n);
    assert(said == 8 + 15);                                     // 64, 128 ... 960
    assert(kms_refusal_logworthy(1) && kms_refusal_logworthy(8) && !kms_refusal_logworthy(9) && kms_refusal_logworthy(64));
    printf("kms refusal helpers ok\n");
    return 0;
}
'''

# ---- the plugin header, compiled as C ---------------------------------------------------------------------------
plugin_fixture = r'''
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "nvmtl_cmd_refusal.h"
int main(void) {
    /* every cause has its own text, and the disabled-backend text points at the line that explains it */
    const nvmtl_refusal all[] = { NVMTL_REFUSE_BACKEND_DOWN, NVMTL_REFUSE_POOL, NVMTL_REFUSE_ALLOC, NVMTL_REFUSE_BEGIN };
    for (unsigned i = 0; i < 4; i++) for (unsigned j = i + 1; j < 4; j++)
        assert(strcmp(nvmtl_refusal_text(all[i]), nvmtl_refusal_text(all[j])) != 0);
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_BACKEND_DOWN), "disabling backend"));
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_ALLOC), "vkAllocateCommandBuffers"));
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_BEGIN), "vkBeginCommandBuffer"));
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_NONE), "no refusal"));
    /* the disabled-backend text must not claim a single cause: a failed start and a lost device both land here */
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_BACKEND_DOWN), "failed to start"));
    assert(strstr(nvmtl_refusal_text(NVMTL_REFUSE_BACKEND_DOWN), "completion check"));
    /* the macros the log call sites concatenate are the same strings the function returns */
    assert(strcmp(nvmtl_refusal_text(NVMTL_REFUSE_POOL), NVMTL_REFUSE_TEXT_POOL) == 0);
    assert(strcmp(nvmtl_refusal_text(NVMTL_REFUSE_BEGIN), NVMTL_REFUSE_TEXT_BEGIN) == 0);
    printf("plugin refusal header ok\n");
    return 0;
}
'''


def build_run(lang, source, extra):
    with tempfile.TemporaryDirectory() as d:
        src = pathlib.Path(d) / ('t.cpp' if lang == 'c++' else 't.c')
        src.write_text(source)
        exe = pathlib.Path(d) / 't'
        cc = ['c++', '-std=c++17'] if lang == 'c++' else ['cc', '-std=c11']
        subprocess.run(cc + ['-Wall', '-Wextra', '-Wno-unused-function'] + extra + ['-I', str(plugin), str(src), '-o', str(exe)],
                       check=True)
        return subprocess.run([str(exe)], capture_output=True, text=True)


r = build_run('c++', kms_fixture, [])
assert r.returncode == 0, r.stdout + r.stderr
r = build_run('c', plugin_fixture, [])
assert r.returncode == 0, r.stdout + r.stderr

# ---- negative control: a helper that never reports must FAIL the same suite -------------------------------------
mute = kms_fixture.replace('static bool kms_refusal_logworthy(unsigned n) { return n <= 8 || (n % 64) == 0; }',
                           'static bool kms_refusal_logworthy(unsigned) { return false; }')
assert mute != kms_fixture
assert build_run('c++', mute, []).returncode != 0, 'a silent log gate must fail the kernel arms'
with tempfile.TemporaryDirectory() as blame:
    hdr = (plugin / 'nvmtl_cmd_refusal.h').read_text()
    blamed = hdr.replace("it failed to start, or a submit, a fence wait or the \" \\\n    \"completion check disabled it", "a lost device disabled it")
    assert blamed != hdr
    (pathlib.Path(blame) / 'nvmtl_cmd_refusal.h').write_text(blamed)
    assert build_run('c', plugin_fixture, ['-I', blame]).returncode != 0, 'a text that blames one cause must fail the plugin arms'

# ---- the call sites use the helpers ----------------------------------------------------------------------------
body = cut(vk, 'int nvmtl_vk_cmd_begin(nvk_queue *q, nvk_cmdbuf *out)', '\nstatic pthread_mutex_t g_rpv_lock')
assert 'return -1' not in body, 'nvmtl_vk_cmd_begin must not refuse without a cause'
for why in ('BACKEND_DOWN', 'POOL', 'ALLOC', 'BEGIN'):
    assert body.count('nvmtl_cmd_refused(NVMTL_REFUSE_%s,' % why) == 1, why
assert re.search(r'nvmtl_cmd_refused\(NVMTL_REFUSE_BACKEND_DOWN, 0\)', body)
refused = cut(vk, 'static int nvmtl_cmd_refused(', '\nint nvmtl_vk_cmd_begin(')
# nvlog throttles by format string: one call site per cause, so a flood of one cannot hide the first line of another
assert refused.count('nvlog(') == 4, 'one nvlog call site per cause'
for why in ('BACKEND_DOWN', 'POOL', 'ALLOC', 'BEGIN'):
    assert ('NVMTL_REFUSE_TEXT_%s' % why) in refused, why
assert 'nvmtl_refusal_logworthy' not in vk and 'nvmtl_refusal_logworthy' not in queue, 'nvlog already throttles; a second throttle hides lines'
qbody = cut(queue, '- (id<MTLCommandBuffer>)commandBuffer {', '- (id<MTLCommandBuffer>)commandBufferWithUnretainedReferences')
assert 'nvmtl_vk_cmd_begin(&_q, &cb->_c)' in qbody and 'getprogname()' in qbody and 'return nil' in qbody
cb = cut(kms, 'static NvBool kms_ioctl_common(', '\nextern "C" {')
assert 'kms_ioctl_refusal(haveOpen, pmReady, tryPm, false)' in cb and 'kms_refusal_logworthy(n)' in cb
assert 'if (!ret)' in cb, 'only a refused ioctl is reported'
assert 'kKmsCmdSetMode ? 0 : 1' in cb, 'mode sets count apart from every other command'
print('refusal reasons: 5 arms ok')
