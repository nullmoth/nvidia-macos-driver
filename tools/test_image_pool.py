#!/usr/bin/env python3
"""Exercise the production pool publication order with two encoding threads.

The first thread's simulated GPU clear is held while a second allocation runs.
The second thread must not receive that uncleared block. Also check failed-clear
cleanup and the scope of the WindowServer default, without loading a GPU driver.
"""
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = (ROOT / "plugin/nvmtl_vk.c").read_text()


def function(source, name):
    match = re.search(r"^static [^\n]*\b" + name + r"\([^;]*?\)\n?\{", source, re.M)
    if not match:
        raise RuntimeError(f"cannot find production function {name}")
    start = match.start()
    end = source.index("{", match.start()) + 1
    depth = 1
    while depth:
        depth += {"{": 1, "}": -1}.get(source[end], 0)
        end += 1
    return source[start:end]


HARNESS = r'''
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
typedef struct { size_t off, len; } pool_range;
typedef struct { size_t size; uint32_t memtype; int ready; } nvk_heap;
typedef struct pool_block {
    struct pool_block *next; nvk_heap heap; size_t used; uint32_t nlive;
    int vram; pool_range *fr; uint32_t nfr, cfr;
} pool_block;
static pool_block *image_pool, *private_pool;
static pool_block **pool_head(int vram) { return vram == 3 ? &image_pool : &private_pool; }
static pthread_mutex_t g_pool_lock = PTHREAD_MUTEX_INITIALIZER;
static uint64_t g_pool_blocks, g_pool_live, g_pool_bytes;
static _Atomic int creates, destroys, zeroes, allocation_under_lock;
static int fail_zero, race;
static pthread_mutex_t gate = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t condition = PTHREAD_COND_INITIALIZER;
static int first_zero, release_first, second_done;
#define NVMTL_POOL_BLOCK 64
#define NVMTL_POOL_MAX 16
#define nvlog(...) ((void)0)
static int nvmtl_vk_heap_create(size_t size, int host, nvk_heap *out) {
    (void)host;
    int locked = pthread_mutex_trylock(&g_pool_lock);
    if (!locked) pthread_mutex_unlock(&g_pool_lock);
    else atomic_fetch_add(&allocation_under_lock, 1);
    *out = (nvk_heap){.size = size, .memtype = 0};
    atomic_fetch_add(&creates, 1); return 0;
}
static void nvmtl_vk_heap_destroy(nvk_heap *heap) { (void)heap; atomic_fetch_add(&destroys, 1); }
static int nvmtl_vk_heap_zero(nvk_heap *heap, const char *what) {
    (void)what;
    int n = atomic_fetch_add(&zeroes, 1);
    if (race && n == 0) {
        pthread_mutex_lock(&gate); first_zero = 1; pthread_cond_broadcast(&condition);
        while (!release_first) pthread_cond_wait(&condition, &gate);
        pthread_mutex_unlock(&gate);
    }
    if (fail_zero) return -1;
    heap->ready = 1; return 0;
}
@@TAKE@@
@@POOL@@
typedef struct { pool_block *block; int ready; size_t offset; } result;
static void *worker(void *ptr) {
    result *r = ptr;
    r->block = pool_take_vram(3, 1, 8, &r->offset, "test image");
    r->ready = r->block && r->block->heap.ready;
    return NULL;
}
static void *second_worker(void *ptr) {
    worker(ptr);
    pthread_mutex_lock(&gate); second_done = 1; pthread_cond_broadcast(&condition); pthread_mutex_unlock(&gate);
    return NULL;
}
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (!strcmp(argv[1], "race")) {
        race = 1;
        pthread_t a, b; result ra = {0}, rb = {0};
        if (pthread_create(&a, NULL, worker, &ra)) return 2;
        pthread_mutex_lock(&gate);
        while (!first_zero) pthread_cond_wait(&condition, &gate);
        pthread_mutex_unlock(&gate);
        if (pthread_create(&b, NULL, second_worker, &rb)) return 2;
        pthread_mutex_lock(&gate);
        while (!second_done) pthread_cond_wait(&condition, &gate);
        release_first = 1; pthread_cond_broadcast(&condition); pthread_mutex_unlock(&gate);
        pthread_join(a, NULL); pthread_join(b, NULL);
        int ok = ra.ready && rb.ready && ra.block == rb.block && ra.offset != rb.offset
            && g_pool_blocks == 1 && g_pool_live == 2 && g_pool_bytes == 16
            && creates == 2 && destroys == 1 && allocation_under_lock == 0;
        printf("ready=%d,%d blocks=%llu allocations=%d freed=%d under-lock=%d\n", ra.ready, rb.ready,
            (unsigned long long)g_pool_blocks, creates, destroys, allocation_under_lock);
        return ok ? 0 : 1;
    }
    if (!strcmp(argv[1], "failure")) {
        fail_zero = 1; size_t off = 0;
        pool_block *p = pool_take_vram(3, 1, 8, &off, "failed clear");
        return p == NULL && image_pool == NULL && !g_pool_blocks && !g_pool_live
            && !g_pool_bytes && creates == 1 && destroys == 1 ? 0 : 1;
    }
    if (!strcmp(argv[1], "type")) {
        size_t off = 0;
        pool_block *p = pool_take_vram(3, 2, 8, &off, "wrong memory type");
        return p == NULL && image_pool == NULL && !g_pool_blocks && creates == destroys && !zeroes ? 0 : 1;
    }
    return 2;
}
'''

POLICY = r'''
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
static const char *process, *override;
static uint16_t vendor;
static const char *test_getprogname(void) { return process; }
static const char *test_getenv(const char *key) { (void)key; return override; }
static uint16_t nvmtl_vk_vendor(void) { return vendor; }
#define getprogname test_getprogname
#define getenv test_getenv
#define NVMTL_POOL_MAX (4u << 20)
#define NVMTL_POOL_BLOCK (32u << 20)
#define nvlog(...) ((void)0)
@@POLICY@@
int main(int argc, char **argv) {
    if (argc != 5) return 2;
    process = strcmp(argv[1], "NULL") ? argv[1] : NULL;
    vendor = (uint16_t)strtoul(argv[2], NULL, 16);
    override = strcmp(argv[3], "NULL") ? argv[3] : NULL;
    return image_pool_on() == atoi(argv[4]) ? 0 : 1;
}
'''


def compile_program(directory, name, source):
    path = directory / (name + ".c")
    path.write_text(source)
    exe = directory / name
    subprocess.run(["xcrun", "clang", "-std=c11", "-O2", "-Wall", "-Wextra", "-pthread", str(path), "-o", str(exe)], check=True)
    return exe


def run(exe, *args):
    return subprocess.run([str(exe), *args], capture_output=True, text=True, timeout=10)


with tempfile.TemporaryDirectory() as temporary:
    directory = pathlib.Path(temporary)
    source = HARNESS.replace("@@TAKE@@", function(SOURCE, "pool_block_take")).replace("@@POOL@@", function(SOURCE, "pool_take_vram"))
    exe = compile_program(directory, "pool", source)
    for case in ("race", "failure", "type"):
        result = run(exe, case)
        assert result.returncode == 0, f"{case}: {result.stdout} {result.stderr}"
        print(f"PASS pool {case}: {result.stdout.strip()}")

    # The publication race and ignored clear failure are both present in this released source.
    old = subprocess.check_output(["git", "-C", str(ROOT), "show", "22d1038:plugin/nvmtl_vk.c"], text=True)
    source = HARNESS.replace("@@TAKE@@", function(old, "pool_block_take")).replace("@@POOL@@", function(old, "pool_take_vram"))
    old_exe = compile_program(directory, "before", source)
    for case in ("race", "failure"):
        result = run(old_exe, case)
        assert result.returncode == 1, f"negative control {case}: {result.returncode} {result.stdout} {result.stderr}"
    print("PASS negative controls expose early publication and ignored clear failure")

    exe = compile_program(directory, "policy", POLICY.replace("@@POLICY@@", function(SOURCE, "image_pool_on")))
    cases = [
        ("WindowServer", "10de", "NULL", "1"),
        ("WindowServer", "1002", "NULL", "0"),
        ("WindowServer", "8086", "NULL", "0"),
        ("Safari", "10de", "NULL", "0"),
        ("WindowServerHelper", "10de", "NULL", "0"),
        ("NULL", "10de", "NULL", "0"),
        ("WindowServer", "10de", "0", "0"),
        ("WindowServer", "10de", "", "0"),
        ("WindowServer", "10de", "invalid", "0"),
        ("Safari", "10de", "1", "1"),
        ("WindowServer", "1002", "1", "1"),
    ]
    for case in cases:
        result = run(exe, *case)
        assert result.returncode == 0, f"policy {case}: {result.returncode} {result.stderr}"
    print("PASS WindowServer scope and explicit overrides (11 cases)")
