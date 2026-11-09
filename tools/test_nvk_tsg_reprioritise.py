#!/usr/bin/env python3
"""Host test for the two NVK scheduling changes in nvk/nvk-macos.patch.

1. The in-place NVRM_TSG_Q0_HIGH edit: an UNSET variable must behave exactly as before (no RM call, no priority change),
   '1' must still mean HIGH, and only the new '0' means LOW. Checked on the patch text and on a model of the predicate.
2. nvmtl_nvk_tsg_reprioritise_v1: the hook is extracted from the patch, compiled against fake RM types and run against a fake
   resource manager. Arms: raise, restore to the recorded values, restore with nothing recorded, bad arguments, CE-only
   and TSG-less contexts skipped, a failing RM call is not counted, a second raise does not overwrite the saved original.
"""
import os, re, subprocess, sys, tempfile, textwrap
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
patch = open(os.path.join(ROOT, 'nvk/nvk-macos.patch')).read().split('\n')
fails = []
def arm(name, ok, detail=''):
    print(('ok   ' if ok else 'FAIL ') + name + (('  ' + detail) if detail and not ok else ''))
    if not ok: fails.append(name)

# ---- 1. unset default ---------------------------------------------------------------------------------------------------
cond = [l for l in patch if 'qidx == 0 && hie' in l]
arm('the interleave condition appears exactly once', len(cond) == 1, str(cond))
arm('the condition requires the variable to be set (hie non-NULL) and 0 or 1',
    cond and "hie && (hie[0] == '1' || hie[0] == '0')" in cond[0])
def old(hie, qidx):  return qidx == 0 and hie is not None and hie[:1] == '1'
def new(hie, qidx):
    if qidx == 0 and hie is not None and hie[:1] in ('1', '0'): return 2 if hie[:1] == '1' else 0
    return None
for hie in (None, '', '2', 'x', ' 1'):
    arm('NVRM_TSG_Q0_HIGH=%r makes no RM call (as before)' % hie, new(hie, 0) is None and not old(hie, 0))
arm("'1' still means HIGH on queue 0", new('1', 0) == 2 and old('1', 0))
arm("'0' is the new LOW", new('0', 0) == 0 and not old('0', 0))
arm('other queues are never touched', all(new(h, q) is None for h in ('0', '1') for q in (-1, 1, 2)))

# ---- 2. the hook, compiled against a fake RM -----------------------------------------------------------------------------
start = next(i for i, l in enumerate(patch) if 'nvmtl_tsg_saved[NVRM_CTX_MAX]' in l)
while not patch[start].startswith('+static struct {'): start -= 1
fn_end = next(i for i in range(start, len(patch)) if patch[i] == '+}' and 'return changed' in patch[i - 1])
hook = '\n'.join(l[1:] for l in patch[start:fn_end + 1])
arm('the hook is exported with default visibility and a prior declaration',
    'visibility("default"))) int nvmtl_nvk_tsg_reprioritise_v1(int interleave_level, unsigned long timeslice_us);' in hook)
harness = r'''
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <pthread.h>
typedef uint32_t NvU32; typedef uint64_t NvU64; typedef uint32_t NvHandle; typedef uint32_t NV_STATUS;
#define NV_OK 0
#define NVRM_CTX_MAX 32
struct NvRmApi { int x; };
struct nvkmd_dev { int id; };
struct nvkmd_pdev { int id; };
struct base_t { struct nvkmd_dev *dev; };
struct nvkmd_nvrm_exec_ctx { struct base_t base; NvHandle hTsg; bool ceOnly; };
struct nvkmd_nvrm_dev { struct { struct nvkmd_pdev *pdev; } base; };
struct nvkmd_nvrm_pdev { int x; };
struct nvkmd_nvrm_slot { struct nvkmd_nvrm_exec_ctx *ctx; };
struct nvkmd_nvrm_slot nvkmd_nvrm_slots[NVRM_CTX_MAX];
pthread_mutex_t nvkmd_nvrm_gsub_mtx = PTHREAD_MUTEX_INITIALIZER;
static struct nvkmd_nvrm_dev g_dev; static struct nvkmd_nvrm_pdev g_pdev;
static struct nvkmd_nvrm_dev *nvkmd_nvrm_dev(struct nvkmd_dev *d) { (void)d; return &g_dev; }
static struct nvkmd_nvrm_pdev *nvkmd_nvrm_pdev(struct nvkmd_pdev *p) { (void)p; return &g_pdev; }
static void nvkmd_nvrm_dev_api_ctl(struct nvkmd_nvrm_pdev *p, struct NvRmApi *rm) { (void)p; rm->x = 1; }

/* fake RM: per handle interleave + timeslice, and a switch that makes SET fail */
static NvU32 rm_level[64]; static NvU64 rm_slice[64]; static int rm_fail_set, rm_calls;
static NV_STATUS nvRmApiControl(struct NvRmApi *rm, NvHandle h, NvU32 cmd, void *p, NvU32 sz) {
   (void)rm; (void)sz; rm_calls++;
   switch (cmd) {
   case 0xa06c0107: if (rm_fail_set) return 0x55; rm_level[h] = *(NvU32 *)p; return NV_OK;
   case 0xa06c0108: *(NvU32 *)p = rm_level[h]; return NV_OK;
   case 0xa06c0103: if (rm_fail_set) return 0x55; rm_slice[h] = *(NvU64 *)p; return NV_OK;
   case 0xa06c0104: *(NvU64 *)p = rm_slice[h]; return NV_OK;
   }
   return 0xff;
}
HOOK_HERE

static int fails;
#define ARM(n, c) do { int ok_ = !!(c); printf("%s %s\n", ok_ ? "ok  " : "FAIL", n); if (!ok_) fails++; } while (0)
static struct nvkmd_nvrm_exec_ctx A = { {0}, 5, false }, B = { {0}, 6, false }, CE = { {0}, 7, true }, BARE = { {0}, 0, false };
int main(void) {
   nvkmd_nvrm_slots[0].ctx = &A; nvkmd_nvrm_slots[1].ctx = &B; nvkmd_nvrm_slots[2].ctx = &CE; nvkmd_nvrm_slots[3].ctx = &BARE;
   rm_level[5] = 1; rm_slice[5] = 2000; rm_level[6] = 1; rm_slice[6] = 3000; rm_level[7] = 1;
   ARM("nothing recorded: restore changes nothing and makes no RM call", nvmtl_nvk_tsg_reprioritise_v1(-1, 0) == 0 && rm_calls == 0);
   ARM("level 3 is refused", nvmtl_nvk_tsg_reprioritise_v1(3, 0) == -1);
   ARM("level -2 is refused", nvmtl_nvk_tsg_reprioritise_v1(-2, 0) == -1);
   ARM("a timeslice over one second is refused", nvmtl_nvk_tsg_reprioritise_v1(2, 2000000UL) == -1 && rm_calls == 0);
   ARM("raise changes the two graphics groups only (CE-only and TSG-less skipped)", nvmtl_nvk_tsg_reprioritise_v1(2, 8000) == 2);
   ARM("both groups are HIGH with the new timeslice", rm_level[5] == 2 && rm_level[6] == 2 && rm_slice[5] == 8000 && rm_slice[6] == 8000);
   ARM("the CE-only group was never touched", rm_level[7] == 1 && rm_slice[7] == 0);
   ARM("a second raise keeps the ORIGINAL saved values", nvmtl_nvk_tsg_reprioritise_v1(0, 1000) == 2 && rm_level[5] == 0 && rm_slice[5] == 1000);
   ARM("restore returns each group to what RM had", nvmtl_nvk_tsg_reprioritise_v1(-1, 0) == 2 && rm_level[5] == 1 && rm_level[6] == 1 && rm_slice[5] == 2000 && rm_slice[6] == 3000);
   ARM("a second restore is a no-op", nvmtl_nvk_tsg_reprioritise_v1(-1, 0) == 0 && rm_level[5] == 1);
   rm_fail_set = 1;
   ARM("a failing RM call is not counted as changed", nvmtl_nvk_tsg_reprioritise_v1(2, 0) == 0);
   rm_fail_set = 0;
   ARM("a group replaced in its slot is recorded afresh", (A.hTsg = 9, rm_level[9] = 0, rm_slice[9] = 500, nvmtl_nvk_tsg_reprioritise_v1(2, 0) == 2) && (nvmtl_nvk_tsg_reprioritise_v1(-1, 0) == 2) && rm_level[9] == 0 && rm_slice[9] == 500);
   ARM("an empty registry changes nothing", (memset(nvkmd_nvrm_slots, 0, sizeof nvkmd_nvrm_slots), nvmtl_nvk_tsg_reprioritise_v1(2, 0) == 0));
   return fails ? 1 : 0;
}
'''.replace('HOOK_HERE', hook)
with tempfile.TemporaryDirectory() as d:
    src = os.path.join(d, 'h.c'); exe = os.path.join(d, 'h')
    open(src, 'w').write(harness)
    r = subprocess.run(['cc', '-Wall', '-Wextra', '-Wno-unused-function', '-o', exe, src], capture_output=True, text=True)
    arm('the hook compiles clean against the fake RM types', r.returncode == 0 and 'warning' not in r.stderr, r.stderr)
    if r.returncode == 0:
        r = subprocess.run([exe], capture_output=True, text=True)
        sys.stdout.write(r.stdout)
        arm('every hook arm passes', r.returncode == 0)
print('\n%s' % ('ALL OK' if not fails else 'FAILED: %d' % len(fails)))
sys.exit(1 if fails else 0)
