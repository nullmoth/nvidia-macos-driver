"""nvkmd_nvrm_mem_unmap must drop the mapping from dev->mappings before freeing it.

Reported and validated on a TU104 (RTX 2070 Super) by Exop63: the unmap freed the NvRmApiMapping but never
removed its entry from dev->mappings, so a second unmap of the same address looked the entry up again and freed
the same pointer twice. When nvk_CreateDevice failed (e.g. an OS33 system-memory map returning 0x56) the device
teardown walked the still-populated table and corrupted the heap; every process that enumerated Metal devices then
aborted in malloc (SafariPlatformSupport.Helper, System Information).

This extracts nvkmd_nvrm_mem_unmap from nvk/nvk-macos.patch, compiles it with a tiny hash table and a free()
that aborts on a double free, and unmaps the same address twice. The fixed function (with
_mesa_hash_table_remove) frees once; the pre-fix function (that line removed) is the negative control and
double-frees.
"""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
patch = (root / 'nvk/nvk-macos.patch').read_text().splitlines()

# pull the unmap function's post-image ('+' lines) out of the new-file diff for nvkmd_nvrm_mem.c
plus = [l[1:] for l in patch if l.startswith('+') and not l.startswith('+++')]
src = '\n'.join(plus)
m = re.search(r'\nnvkmd_nvrm_mem_unmap\(struct nvkmd_mem \*_mem,.*?\n\}\n', src, re.S)
assert m, 'nvkmd_nvrm_mem_unmap not found in patch'
unmap = 'static void\n' + m.group(0).strip() + '\n'
assert '_mesa_hash_table_remove(dev->mappings, ent)' in unmap, 'fix line missing from extracted function'
unmap_old = '\n'.join(l for l in unmap.splitlines() if '_mesa_hash_table_remove(dev->mappings, ent)' not in l) + '\n'

HARNESS = r'''
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* free() that aborts on a double free, so the bug is a hard, detectable failure */
#define MAXP 64
static void *freed[MAXP]; static int nfreed;
static void tracked_free(void *p) {
    if (!p) return;
    for (int i = 0; i < nfreed; i++) if (freed[i] == p) { fprintf(stderr, "DOUBLE FREE %p\n", p); abort(); }
    freed[nfreed++] = p; free(p);
}
#define free(p) tracked_free(p)

/* minimal mesa hash table: search / insert / remove by key pointer */
struct hash_entry { const void *key; void *data; };
struct hash_table { struct hash_entry e[MAXP]; int n; };
static struct hash_entry *_mesa_hash_table_search(struct hash_table *t, const void *key) {
    for (int i = 0; i < t->n; i++) if (t->e[i].key == key && t->e[i].data) return &t->e[i];
    return NULL;
}
static struct hash_entry *_mesa_hash_table_insert(struct hash_table *t, const void *key, void *data) {
    t->e[t->n].key = key; t->e[t->n].data = data; return &t->e[t->n++];
}
static void _mesa_hash_table_remove(struct hash_table *t, struct hash_entry *ent) { ent->key = NULL; ent->data = NULL; }

/* the slice of NVK types the function touches */
typedef int NV_STATUS; enum { NV_OK = 0 };
enum nvkmd_mem_map_flags { MF = 0 };
struct NvRmApi { int _; };
typedef struct { void *address; } NvRmApiMapping;
struct nvkmd_nvrm_pdev { int hSubdevice; };
struct nvkmd_nvrm_dev { struct { struct nvkmd_nvrm_pdev *pdev; } base; struct hash_table *mappings; };
struct nvkmd_mem { struct nvkmd_nvrm_dev *dev; };
struct nvkmd_nvrm_mem { struct { struct nvkmd_mem *_self; } base; void *hostPtr; int hMemoryPhys; struct nvkmd_mem m; };
static struct nvkmd_nvrm_mem *nvkmd_nvrm_mem(struct nvkmd_mem *m) { return (struct nvkmd_nvrm_mem *)((char *)m - __builtin_offsetof(struct nvkmd_nvrm_mem, m)); }
static struct nvkmd_nvrm_dev *nvkmd_nvrm_dev(struct nvkmd_nvrm_dev *d) { return d; }
static struct nvkmd_nvrm_pdev *nvkmd_nvrm_pdev(struct nvkmd_nvrm_pdev *p) { return p; }
static void nvkmd_nvrm_dev_api_ctl(struct nvkmd_nvrm_pdev *p, struct NvRmApi *rm) { (void)p; (void)rm; }
static NV_STATUS nvRmApiUnmapMemory(struct NvRmApi *rm, int sub, int phys, int off, NvRmApiMapping *map) { (void)rm;(void)sub;(void)phys;(void)off;(void)map; return NV_OK; }

/* _mem->dev is reached as nvkmd_nvrm_dev(_mem->dev); our stub takes a dev*, so store the dev there */
'''

FUNC_PATCH = 'nvkmd_nvrm_dev(_mem->dev)'   # the real code; our struct nvkmd_mem.dev is already a nvkmd_nvrm_dev*

MAIN = r'''
int main(int argc, char **argv) {
    struct nvkmd_nvrm_pdev pdev = { .hSubdevice = 1 };
    struct hash_table tbl = {0};
    struct nvkmd_nvrm_dev dev = { .base = { .pdev = &pdev }, .mappings = &tbl };
    struct nvkmd_nvrm_mem mem = {0};
    mem.m.dev = &dev; mem.hMemoryPhys = 7; mem.hostPtr = NULL;

    /* map: one mapping inserted at address A */
    NvRmApiMapping *mapping = calloc(1, sizeof *mapping);
    void *A = (void *)0x4000;
    mapping->address = A;
    _mesa_hash_table_insert(&tbl, A, mapping);

    nvkmd_nvrm_mem_unmap(&mem.m, MF, A);   /* first unmap */
    nvkmd_nvrm_mem_unmap(&mem.m, MF, A);   /* second unmap of the same address */
    puts("OK");
    return 0;
}
'''

fails = 0
with tempfile.TemporaryDirectory() as d:
    tmp = pathlib.Path(d)
    for name, fn in (('newv', unmap), ('oldv', unmap_old)):
        (tmp / (name + '.c')).write_text(HARNESS + fn + MAIN)
        subprocess.run(['clang', '-std=c11', str(tmp / (name + '.c')), '-o', str(tmp / name)], check=True)
    r_new = subprocess.run([str(tmp / 'newv')], capture_output=True, text=True)
    ok_new = r_new.returncode == 0 and r_new.stdout.strip() == 'OK'
    print(('ok   ' if ok_new else 'FAIL ') + f'fixed unmap frees once across a repeated unmap (rc={r_new.returncode})')
    fails += not ok_new
    r_old = subprocess.run([str(tmp / 'oldv')], capture_output=True, text=True)
    ok_old = r_old.returncode != 0 and 'DOUBLE FREE' in r_old.stderr
    print(('ok   ' if ok_old else 'FAIL ') + f'NEGATIVE CONTROL: pre-fix unmap double-frees the mapping (rc={r_old.returncode})')
    fails += not ok_old

print('PASS' if not fails else f'{fails} FAILED')
sys.exit(1 if fails else 0)
