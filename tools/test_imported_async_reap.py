#!/usr/bin/env python3
"""Grace-completed imported host-pointer buffers unpin off the submit pump without breaking dealloc or grace contracts.

Compiles the production async reaper, buffer-grace, and nvmtl_vk_buffer_destroy_body straight from plugin/nvmtl_vk.c
and plugin/nvmtl_vk.h, and checks:
  1. An imported buffer freed while a submit is still in flight stays alive in g_bg until g_cmd_finished_seq catches up.
  2. Once grace completes, an imported buffer with sync_free == 0 is destroyed on the serial background queue, not the
     caller's thread.
  3. An imported buffer with sync_free == 1 (_hostDealloc != nil in -[NVMTLBuffer dealloc]) is destroyed synchronously
     before nvmtl_vk_buffer_destroy returns.
  4. A non-imported mapped buffer (!imported && map != NULL) is never offloaded and runs pvkUnmapMemory synchronously.
  5. Flooding > NVMTL_IREAP_CAP buffers hits bounded backpressure (sync fallback) and leaves zero leaked bytes/handles."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
vk_h = (root / 'plugin/nvmtl_vk.h').read_text()
vk_c = (root / 'plugin/nvmtl_vk.c').read_text()
objs_m = (root / 'plugin/NVMTLObjects.m').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


dealloc_slice = objs_m[objs_m.index('void nvmtl_vk_buffer_retire(nvk_buffer *b);\n- (void)dealloc {'):]
dealloc_slice = dealloc_slice[:dealloc_slice.index('- (MTLStorageMode)storageMode')]
check('NVMTLBuffer -dealloc marks _b and _shadow sync_free before retire when _hostDealloc is set',
      re.search(r'if \(_hostDealloc\)\s*\{\s*_b\.sync_free\s*=\s*1;\s*_shadow\.sync_free\s*=\s*1;\s*\}\s*\n\s*nvmtl_vk_buffer_retire',
                dealloc_slice) is not None)

nvk_buf_decl = vk_h[vk_h.index('typedef struct { void *buf;'):vk_h.index('} nvk_buffer;') + len('} nvk_buffer;')]
grace_decl = vk_c[vk_c.index('typedef struct nvmtl_bgrace'):vk_c.index('static void nvmtl_live_add(')]
prod_start = vk_c.index('// Imported host-pointer async reaper:')
prod_end = vk_c.index('\nstatic void nvmtl_bpark_put_now(nvk_buffer *b)\n{')
prod_c = vk_c[prod_start:prod_end]

HARNESS = f'''
#include <dispatch/dispatch.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include "{root / 'plugin/nvmtl_page_life.h'}"

typedef void *VkDevice;
typedef void *VkBuffer;
typedef void *VkDeviceMemory;
{nvk_buf_decl}

enum {{ NVMTL_LV_DYING = 1, NVMTL_FX_POOLED = 1, NVMTL_FX_PLACED = 2, NVP_BUFDEL = 3 }};
static VkDevice g_dev = (void *)0x1;
static _Atomic int g_state = 1;
static _Atomic size_t g_alloc_bytes;
static _Atomic uint64_t g_buf_made, g_buf_gone;
static size_t g_bar1_bytes;
static uint64_t g_bind_lost, g_bind_lost_bytes;
static _Atomic uint64_t g_cmd_submitted;
static uint64_t g_cmd_finished_seq;

static void nvlog(const char *fmt, ...) {{ (void)fmt; }}
static uint64_t nvmtl_perf_now(void) {{ return 0; }}
static void nvmtl_perf_note(int what, uint64_t t0, uint64_t bytes) {{ (void)what; (void)t0; (void)bytes; }}
static void nvmtl_fx_note_buffer_gone(nvk_buffer *b, uint32_t where) {{ (void)b; (void)where; }}
static void nvmtl_live_relabel(nvk_buffer *b, uint32_t kind) {{ (void)b; (void)kind; }}
static void nvmtl_live_remove(nvk_buffer *b) {{ (void)b; }}
static void pool_buffer_release(nvk_buffer *b) {{ (void)b; }}
static void nvmtl_vk_budget_discharge(uint64_t bytes) {{ (void)bytes; }}

static _Atomic unsigned g_destroy_buf_n, g_free_mem_n, g_unmap_n;
static _Atomic uint64_t g_last_free_tid, g_last_unmap_tid;
static _Atomic unsigned g_sleep_us;

static uint64_t cur_tid(void) {{
    uint64_t tid = 0;
    pthread_threadid_np(NULL, &tid);
    return tid;
}}
static void mock_DestroyBuffer(VkDevice d, VkBuffer b, const void *a) {{
    (void)d; (void)b; (void)a;
    atomic_fetch_add(&g_destroy_buf_n, 1);
}}
static void mock_FreeMemory(VkDevice d, VkDeviceMemory m, const void *a) {{
    (void)d; (void)m; (void)a;
    unsigned us = atomic_load(&g_sleep_us);
    if (us) usleep(us);
    atomic_store(&g_last_free_tid, cur_tid());
    atomic_fetch_add(&g_free_mem_n, 1);
}}
static void mock_UnmapMemory(VkDevice d, VkDeviceMemory m) {{
    (void)d; (void)m;
    atomic_store(&g_last_unmap_tid, cur_tid());
    atomic_fetch_add(&g_unmap_n, 1);
}}
static void (*pvkDestroyBuffer)(VkDevice, VkBuffer, const void *) = mock_DestroyBuffer;
static void (*pvkFreeMemory)(VkDevice, VkDeviceMemory, const void *) = mock_FreeMemory;
static void (*pvkUnmapMemory)(VkDevice, VkDeviceMemory) = mock_UnmapMemory;

{grace_decl}
{prod_c}
static void nvmtl_bpark_put_now(nvk_buffer *b) {{ (void)b; }}

static void drain_reaper(void) {{
    pthread_mutex_lock(&g_ireap_lock);
    while (g_ireap_n || g_ireap_scheduled) pthread_cond_wait(&g_ireap_cv, &g_ireap_lock);
    pthread_mutex_unlock(&g_ireap_lock);
}}

static nvk_buffer make_buf(uintptr_t tag, size_t sz, int imported, int sync_free) {{
    nvk_buffer b;
    memset(&b, 0, sizeof b);
    b.buf = (void *)(tag * 2 + 1);
    b.mem = (void *)(tag * 2 + 2);
    b.map = (void *)(0x100000 + tag * 0x1000);
    b.size = sz;
    b.alloc = sz;
    b.imported = imported;
    b.sync_free = (uint8_t)sync_free;
    g_alloc_bytes += sz;
    g_buf_made++;
    return b;
}}

int main(void) {{
    const uint64_t main_tid = cur_tid();

    /* 1. In-flight submit holds an imported buffer in g_bg until g_cmd_finished_seq catches up. */
    atomic_store(&g_cmd_submitted, 5);
    __atomic_store_n(&g_cmd_finished_seq, 4, __ATOMIC_RELEASE);
    nvk_buffer b1 = make_buf(1, 65536, 1, 0);
    nvmtl_vk_buffer_destroy(&b1);
    int held_in_grace = (atomic_load(&g_destroy_buf_n) == 0 && atomic_load(&g_free_mem_n) == 0 &&
                         atomic_load(&g_ireap_queued) == 0 && g_alloc_bytes == 65536);

    __atomic_store_n(&g_cmd_finished_seq, 5, __ATOMIC_RELEASE);
    nvmtl_imported_reap_poke();
    nvmtl_buffer_grace_reap();
    drain_reaper();
    int async_after_grace = (atomic_load(&g_destroy_buf_n) == 1 && atomic_load(&g_free_mem_n) == 1 &&
                             atomic_load(&g_ireap_freed) == 1 && atomic_load(&g_last_free_tid) != main_tid &&
                             atomic_load(&g_unmap_n) == 0 && g_alloc_bytes == 0);

    /* 2. sync_free == 1 (_hostDealloc != nil) destroys synchronously before returning. */
    atomic_store(&g_sleep_us, 1000);
    nvk_buffer b2 = make_buf(2, 32768, 1, 1);
    nvmtl_vk_buffer_destroy(&b2);
    int sync_dealloc_ok = (atomic_load(&g_free_mem_n) == 2 && atomic_load(&g_last_free_tid) == main_tid &&
                           atomic_load(&g_ireap_queued) == 1 && g_alloc_bytes == 0);
    atomic_store(&g_sleep_us, 0);

    /* 3. Non-imported mapped buffer runs pvkUnmapMemory + pvkFreeMemory synchronously on caller thread. */
    nvk_buffer b3 = make_buf(3, 16384, 0, 0);
    nvmtl_vk_buffer_destroy(&b3);
    int non_import_ok = (atomic_load(&g_unmap_n) == 1 && atomic_load(&g_last_unmap_tid) == main_tid &&
                         atomic_load(&g_last_free_tid) == main_tid && atomic_load(&g_ireap_queued) == 1 &&
                         g_alloc_bytes == 0);

    /* 4. Flooding 200 idle imported buffers under slow free hits backpressure cleanly and leaks nothing. */
    const uint64_t q0 = atomic_load(&g_ireap_queued);
    atomic_store(&g_sleep_us, 400);
    for (uintptr_t k = 0; k < 200; k++) {{
        nvk_buffer bk = make_buf(100 + k, 4096, 1, 0);
        nvmtl_vk_buffer_destroy(&bk);
    }}
    drain_reaper();
    atomic_store(&g_sleep_us, 0);
    int flood_ok = (atomic_load(&g_destroy_buf_n) == 203 && atomic_load(&g_free_mem_n) == 203 &&
                    atomic_load(&g_ireap_sync_fallback) > 0 &&
                    (atomic_load(&g_ireap_queued) - q0) + atomic_load(&g_ireap_sync_fallback) == 200 &&
                    atomic_load(&g_ireap_queued) == atomic_load(&g_ireap_freed) &&
                    g_alloc_bytes == 0 && g_buf_made == g_buf_gone);

    printf("%d %d %d %d %d %llu %llu\\n",
           held_in_grace, async_after_grace, sync_dealloc_ok, non_import_ok, flood_ok,
           (unsigned long long)atomic_load(&g_ireap_freed),
           (unsigned long long)atomic_load(&g_ireap_sync_fallback));
    return 0;
}}
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-ireap-') as d:
    d = pathlib.Path(d)
    src, exe = d / 't.c', d / 't'
    src.write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-O1', '-Wall', '-Wextra', '-Werror=unguarded-availability-new',
                        '-mmacosx-version-min=15.0', '-o', str(exe), str(src)],
                       capture_output=True, text=True)
    check('production imported reaper + buffer grace slice compiles cleanly (minos 15.0)', r.returncode == 0, r.stderr[-800:])
    if r.returncode == 0:
        out = subprocess.run([str(exe)], capture_output=True, text=True, timeout=30)
        check('harness exited 0', out.returncode == 0, out.stderr[-400:])
        if out.returncode == 0:
            vals = list(map(int, out.stdout.split()))
            held, async_ok, sync_ok, non_imp, flood_ok, freed, fallbacks = vals
            check('in-flight submit holds imported buffer in g_bg before any async reap', held == 1)
            check('grace-completed imported buffer (sync_free=0) frees on background worker without unmap', async_ok == 1)
            check('sync_free=1 (_hostDealloc) frees synchronously before nvmtl_vk_buffer_destroy returns', sync_ok == 1)
            check('non-imported mapped buffer unmaps and frees synchronously on caller thread', non_imp == 1)
            check('200-buffer burst exercises bounded queue backpressure with 0 leaked bytes/handles',
                  flood_ok == 1, f'async_freed={freed}, sync_fallbacks={fallbacks}')

print('PASS' if not fails else f'FAILED {fails}')
sys.exit(1 if fails else 0)
