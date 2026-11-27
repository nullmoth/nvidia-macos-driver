#!/usr/bin/env python3
"""Buffer park (NVMTL_BUFPARK_N=256) reuses >32-buffer bursts and never recycles a buffer while a submit still reads it.

Compiles the production bpark + buffer-grace + nvmtl_vk_buffer_retire straight from plugin/nvmtl_vk.c and checks:
  1. Cache128 data-error regression (outputs/Cache128-test-ve-veri-hatasi-20261025.txt): when an ahead-submitted (G-SUB)
     producer command buffer is still in flight (g_cmd_submitted > g_cmd_finished_seq) and the owner retires its buffer,
     an un-graced retire (negative control) hands the same backing memory to the next allocation and corrupts the
     in-flight GPU read when the next caller uploads bytes; production nvmtl_vk_buffer_retire holds the buffer in g_bg
     with 0 corrupted words and parks it for zero-alloc reuse once g_cmd_finished_seq catches up.
  2. 2.9 FIFO thrash fix (driver.log:241-243 '128 parked, 0 reused, 96 evicted'): a 96-buffer cyclic OpenCL step
     (64 small <16 KiB Managed NoCopy buffers + 32 >=1 MiB Private res3 buffers) achieves 96/96 reuse and 0 evictions.
  3. bind_lost buffers never enter g_bpark, and total parked bytes stay <= NVMTL_BUFPARK_BYTES (1024 MiB)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
vk_h = (root / 'plugin/nvmtl_vk.h').read_text()
vk_c = (root / 'plugin/nvmtl_vk.c').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


nvk_buf_decl = vk_h[vk_h.index('typedef struct { void *buf;'):vk_h.index('} nvk_buffer;') + len('} nvk_buffer;')]
grace_decl = vk_c[vk_c.index('typedef struct nvmtl_bgrace'):vk_c.index('static void nvmtl_live_add(')]
bpark_slice = vk_c[vk_c.index('#define NVMTL_BUFPARK_N'):vk_c.index('static int nvmtl_vk_buffer_create_body(')]
retire_slice = vk_c[vk_c.index('static void nvmtl_vk_buffer_destroy_body(nvk_buffer *b);'):
                    vk_c.index('\nvoid nvmtl_vk_image_destroy(nvk_image *i)')]

check('NVMTL_BUFPARK_N is 256', '#define NVMTL_BUFPARK_N     256' in bpark_slice)

HARNESS = f'''
#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "{root / 'plugin/nvmtl_page_life.h'}"

typedef void *VkDevice;
typedef void *VkBuffer;
typedef void *VkDeviceMemory;
{nvk_buf_decl}

enum {{ NVMTL_LV_DYING = 1, NVMTL_LV_PARKED = 2, NVMTL_FX_POOLED = 1, NVMTL_FX_PLACED = 2, NVMTL_FX_PARKED = 4, NVP_BUFDEL = 3 }};
static VkDevice g_dev = (void *)0x1;
static _Atomic int g_state = 1;
static _Atomic size_t g_alloc_bytes;
static _Atomic uint64_t g_buf_made, g_buf_gone;
static size_t g_bar1_bytes;
static int g_bar1_off;
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
static int nvmtl_imported_reap_enqueue(const nvk_buffer *b) {{ (void)b; return 0; }}

static _Atomic unsigned g_destroyed, g_freed;
static void mock_DestroyBuffer(VkDevice d, VkBuffer b, const void *a) {{ (void)d; (void)b; (void)a; atomic_fetch_add(&g_destroyed, 1); }}
static void mock_FreeMemory(VkDevice d, VkDeviceMemory m, const void *a) {{ (void)d; (void)m; (void)a; atomic_fetch_add(&g_freed, 1); }}
static void mock_UnmapMemory(VkDevice d, VkDeviceMemory m) {{ (void)d; (void)m; }}
static void (*pvkDestroyBuffer)(VkDevice, VkBuffer, const void *) = mock_DestroyBuffer;
static void (*pvkFreeMemory)(VkDevice, VkDeviceMemory, const void *) = mock_FreeMemory;
static void (*pvkUnmapMemory)(VkDevice, VkDeviceMemory) = mock_UnmapMemory;

{grace_decl}
{bpark_slice}
{retire_slice}

static uintptr_t g_next_id = 100;
static nvk_buffer fresh_buf(size_t sz, int host_vis, void *backing) {{
    nvk_buffer b;
    memset(&b, 0, sizeof b);
    uintptr_t id = ++g_next_id;
    b.buf = (void *)(id * 2 + 1);
    b.mem = (void *)(id * 2 + 2);
    b.map = host_vis ? (backing ? backing : (void *)(0x200000 + id * 0x1000)) : NULL;
    b.size = sz;
    b.alloc = sz;
    b.fresh = host_vis ? 1 : 0;
    g_alloc_bytes += sz;
    g_buf_made++;
    return b;
}}

/* Negative control: pre-fix retire that parked straight into g_bpark without checking g_cmd_finished_seq. */
static void ungraced_retire(nvk_buffer *b) {{
    nvmtl_bpark_put_now(b);
}}

/* Reproduces the Cache128 ahead-submission hazard across 68 MiB and 118 MiB logical buffer tags (using 65536 words). */
static unsigned run_cache128_scenario(int use_production_grace, int *reused_after_fence) {{
    enum {{ NWORDS = 65536 }};
    uint32_t *mem1 = malloc(NWORDS * sizeof(uint32_t));
    uint32_t *mem2 = malloc(NWORDS * sizeof(uint32_t));
    const size_t logical_sz = 68ull << 20;

    /* Producer encodes & ahead-submits seq 10 (GPU still at seq 9), then releases its buffer handle. */
    atomic_store(&g_cmd_submitted, 10);
    __atomic_store_n(&g_cmd_finished_seq, 9, __ATOMIC_RELEASE);
    nvk_buffer prod = fresh_buf(logical_sz, 1, mem1);
    void *orig_vkbuf = prod.buf;
    for (unsigned i = 0; i < NWORDS; i++) ((uint32_t *)prod.map)[i] = 0xA5A50000u ^ i;

    if (use_production_grace) nvmtl_vk_buffer_retire(&prod);
    else ungraced_retire(&prod);

    /* Next caller immediately allocates same size + placement and uploads consumer bytes BEFORE seq 10 completes! */
    nvk_buffer next;
    memset(&next, 0, sizeof next);
    if (bpark_take(logical_sz, 1, &next) != 0) {{
        next = fresh_buf(logical_sz, 1, mem2);
    }}
    for (unsigned i = 0; i < NWORDS; i++) ((uint32_t *)next.map)[i] = 0x5A5A0000u ^ i;

    /* GPU executes seq 10 reading mem1 (the physical backing bound to orig_vkbuf). Count corrupted words! */
    unsigned wrong = 0;
    for (unsigned i = 0; i < NWORDS; i++) {{
        if (mem1[i] != (0xA5A50000u ^ i)) wrong++;
    }}

    /* Now seq 10 & 11 complete on the GPU. */
    __atomic_store_n(&g_cmd_finished_seq, 10, __ATOMIC_RELEASE);
    nvmtl_buffer_grace_reap();

    nvk_buffer after_fence;
    memset(&after_fence, 0, sizeof after_fence);
    *reused_after_fence = (bpark_take(logical_sz, 1, &after_fence) == 0 && after_fence.buf == orig_vkbuf);

    /* Clean up parked / live test buffers so g_bpark starts clean for the next test. */
    if (after_fence.buf) nvmtl_vk_buffer_destroy(&after_fence);
    if (next.buf) nvmtl_vk_buffer_destroy(&next);
    while (g_bpark_n) {{ bpark_free(&g_bpark[--g_bpark_n]); }}
    g_bpark_bytes = 0;
    free(mem1);
    free(mem2);
    return wrong;
}}

int main(void) {{
    int neg_reused = 0, prod_reused = 0;
    unsigned neg_wrong = run_cache128_scenario(0, &neg_reused);
    unsigned prod_wrong = run_cache128_scenario(1, &prod_reused);

    /* Reset counters for 96-buffer cyclic step test (64 small Managed + 32 >=1 MiB Private res3). */
    atomic_store(&g_bpark_parked, 0);
    atomic_store(&g_bpark_reused, 0);
    atomic_store(&g_bpark_evicted, 0);

    nvk_buffer step[96];
    atomic_store(&g_cmd_submitted, 20);
    __atomic_store_n(&g_cmd_finished_seq, 19, __ATOMIC_RELEASE);
    for (unsigned i = 0; i < 96; i++) {{
        size_t sz = (i < 64) ? (256u + i * 64u) : ((1u << 20) + (i - 64) * 65536u);
        int hv = (i < 64) ? 1 : 0;
        step[i] = fresh_buf(sz, hv, NULL);
        nvmtl_vk_buffer_retire(&step[i]);
    }}
    int held_while_inflight = (g_bpark_n == 0 && g_bg.ncool == 96);
    __atomic_store_n(&g_cmd_finished_seq, 20, __ATOMIC_RELEASE);
    nvmtl_buffer_grace_reap();
    int parked_after_fence = (g_bpark_n == 96 && g_bg.ncool == 0 && atomic_load(&g_bpark_evicted) == 0);

    unsigned hits = 0;
    for (unsigned i = 0; i < 96; i++) {{
        size_t sz = (i < 64) ? (256u + i * 64u) : ((1u << 20) + (i - 64) * 65536u);
        int hv = (i < 64) ? 1 : 0;
        nvk_buffer got;
        if (bpark_take(sz, hv, &got) == 0) {{
            hits++;
            nvmtl_vk_buffer_destroy(&got);
        }}
    }}

    /* bind_lost buffer must NOT be parked. */
    nvk_buffer lost = fresh_buf(1u << 20, 0, NULL);
    lost.bind_lost = 1;
    nvmtl_vk_buffer_retire(&lost);
    int bind_lost_rejected = (g_bpark_n == 0);

    /* Budget cap: 5 buffers * 256 MiB = 1280 MiB > 1024 MiB -> evicts oldest so g_bpark_bytes <= 1024 MiB. */
    for (unsigned i = 0; i < 5; i++) {{
        nvk_buffer big = fresh_buf(256ull << 20, 0, NULL);
        nvmtl_vk_buffer_retire(&big);
    }}
    int budget_ok = (g_bpark_n == 4 && g_bpark_bytes == (1024ull << 20) && atomic_load(&g_bpark_evicted) == 1);
    bpark_drain_private();
    budget_ok = budget_ok && (g_bpark_n == 0 && g_bpark_bytes == 0);

    printf("%u %u %d %d %d %u %d %d\\n",
           neg_wrong, prod_wrong, prod_reused, held_while_inflight, parked_after_fence, hits,
           bind_lost_rejected, budget_ok);
    return 0;
}}
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-bpark-grace-') as d:
    d = pathlib.Path(d)
    src, exe = d / 't.c', d / 't'
    src.write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-O1', '-Wall', '-Wextra', '-Werror=unguarded-availability-new',
                        '-mmacosx-version-min=15.0', '-o', str(exe), str(src)],
                       capture_output=True, text=True)
    check('production bpark + buffer-grace + retire slice compiles cleanly (minos 15.0)',
          r.returncode == 0, r.stderr[-800:])
    if r.returncode == 0:
        out = subprocess.run([str(exe)], capture_output=True, text=True, timeout=30)
        check('harness exited 0', out.returncode == 0, out.stderr[-400:])
        if out.returncode == 0:
            neg_w, prod_w, prod_reused, held96, parked96, hits96, lost_rej, bud_ok = map(int, out.stdout.split())
            check('negative control (Cache128 hazard): un-graced retire corrupts in-flight producer words',
                  neg_w == 65536, f'{neg_w}/65536 corrupted')
            check('grace-gated retire prevents Cache128 in-flight overwrite (0 wrong words) and reuses after fence',
                  prod_w == 0 and prod_reused == 1, f'wrong={prod_w}, reused_after_fence={prod_reused}')
            check('96-buffer cyclic burst cools in g_bg while in flight and parks all 96 with 0 evictions after fence',
                  held96 == 1 and parked96 == 1)
            check('96-buffer second iteration achieves 96/96 reuse hits (was 0/96 with NVMTL_BUFPARK_N=32)',
                  hits96 == 96, f'{hits96}/96 hits')
            check('bind_lost buffer is excluded from g_bpark', lost_rej == 1)
            check('1024 MiB park byte ceiling evicts oldest buffer cleanly', bud_ok == 1)

print('PASS' if not fails else f'FAILED {fails}')
sys.exit(1 if fails else 0)
