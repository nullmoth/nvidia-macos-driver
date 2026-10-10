/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOService.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/IOLocks.h>
#include <kern/thread.h>
#include <kern/thread_call.h>
#include <kern/clock.h>
#include <kern/task.h>
#include <sys/vnode.h>
#include <sys/fcntl.h>
#include <sys/uio.h>
#include <IOKit/IOSubMemoryDescriptor.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <sys/proc.h>
#include <sys/kauth.h>

#define NV_FIRMWARE_FOR_NAME(name) "/Users/Shared/nvfw/nvidia/610.57.04/" name ".bin"
extern "C" {
#include "nv-firmware.h"
}
static const char *nv_xnu_fw_path(nv_firmware_type_t t, nv_firmware_chip_family_t f)
{
    bool ga = (f == NV_FIRMWARE_CHIP_FAMILY_GA10X || f == NV_FIRMWARE_CHIP_FAMILY_AD10X || f == NV_FIRMWARE_CHIP_FAMILY_GH100 ||
               f == NV_FIRMWARE_CHIP_FAMILY_GB10X || f == NV_FIRMWARE_CHIP_FAMILY_GB20X || f == NV_FIRMWARE_CHIP_FAMILY_GB10Y ||
               f == NV_FIRMWARE_CHIP_FAMILY_GB20Y || f == NV_FIRMWARE_CHIP_FAMILY_GR10X);
    bool tu = (f == NV_FIRMWARE_CHIP_FAMILY_TU10X || f == NV_FIRMWARE_CHIP_FAMILY_TU11X || f == NV_FIRMWARE_CHIP_FAMILY_GA100);
    if (ga) { if (t == NV_FIRMWARE_TYPE_GSP) return NV_FIRMWARE_FOR_NAME("gsp_ga10x"); if (t == NV_FIRMWARE_TYPE_GSP_LOG) return NV_FIRMWARE_FOR_NAME("gsp_log_ga10x"); if (t == NV_FIRMWARE_TYPE_UCODES) return NV_FIRMWARE_FOR_NAME("ucodes_ga10x"); }
    if (tu) { if (t == NV_FIRMWARE_TYPE_GSP) return NV_FIRMWARE_FOR_NAME("gsp_tu10x"); if (t == NV_FIRMWARE_TYPE_GSP_LOG) return NV_FIRMWARE_FOR_NAME("gsp_log_tu10x"); if (t == NV_FIRMWARE_TYPE_UCODES) return NV_FIRMWARE_FOR_NAME("ucodes_tu10x"); }
    return "";
}

#define TRC do { static bool _t = false; if (!_t) { _t = true; kprintf("NVRM-xnu: >%s\n", __func__); } } while (0)
static inline void nvx2_memset(void *d, int c, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory"); }
static inline void nvx2_memcpy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }

extern "C" {

NvBool NV_API_CALL os_check_access(RsAccessRight accessRight) { TRC; return NV_TRUE; }
NvU32  NV_API_CALL os_get_current_process_flags(void) { TRC; return 0; }
NvBool NV_API_CALL nv_requires_dma_remap(nv_state_t *nv) { TRC; return NV_FALSE; }
static NvU32 nv_xnu_dma_bits = 64;
void   NV_API_CALL nv_set_dma_address_size(nv_state_t *nv, NvU32 bits) { TRC; nv_xnu_dma_bits = bits; kprintf("NVRM-xnu:   dma address size %u bits\n", bits); }

struct nv_xnu_alloc { IOBufferMemoryDescriptor *md; void *va; NvU64 page_size; NvU64 bytes; NvU32 page_count; NvBool contig; NvU32 cache;
                      NvU64 *ptes; NvU32 nptes; SInt32 kmaps; NvBool freed, pinned; struct nv_xnu_alloc *prev, *next; };
static nv_xnu_alloc *nv_xnu_live[512]; static int nv_xnu_live_n = 0;
extern "C" int nv_xnu_kmap_overlap(NvU64 phys, NvU64 len);
extern "C" bool nv_xnu_chat(void);
extern "C" int nv_xnu_pmap_owner(uint64_t pm, char *name, size_t cap, uint64_t *closed_ms);

#include <sys/sysctl.h>
static const NvU64 kNvWatchOff = 0x132b000ULL, kNvWatchSlot = 0x40000000ULL;
static char     nv_watch_text[3072]; static size_t nv_watch_len = 0;
static NvU64    nv_watch_allocs = 0, nv_watch_frees = 0, nv_watch_bytes_seen = 0;
static const char *nv_watch_selftest = "not run";
static IOSimpleLock *nv_watch_lock = NULL;

static NvU64 nv_watch_hit(NvU64 p, NvU64 len)
{
    if (!len) return 0;
    NvU64 c = (p & ~(kNvWatchSlot - 1)) + kNvWatchOff;
    if (c + PAGE_SIZE <= p) c += kNvWatchSlot;
    return (c < p + len) ? c : 0;
}

static void nv_watch_append(const char *line, size_t L);
static void nv_watch_note(const char *what, NvU64 hit, NvU64 base, NvU64 bytes, NvU32 cache, NvBool contig)
{
    if (!nv_watch_lock) return;
    NvU64 ns = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &ns);
    char line[200];
    int w = snprintf(line, sizeof line, "%s t=%llu.%03llus page 0x%llx in alloc 0x%llx+%llu cache %u contig %u\n",
                     what, ns / 1000000000ULL, (ns / 1000000ULL) % 1000ULL, hit, base, bytes, cache, (unsigned)contig);
    if (w <= 0) return;
    size_t L = (size_t)w < sizeof line ? (size_t)w : sizeof line - 1;
    nv_watch_append(line, L);
}

static void nv_watch_append(const char *line, size_t L)
{
    if (!nv_watch_lock) return;
    IOSimpleLockLock(nv_watch_lock);
    if (nv_watch_len + L >= sizeof nv_watch_text) {
        size_t drop = nv_watch_len / 2;
        while (drop < nv_watch_len && nv_watch_text[drop - 1] != '\n') drop++;
        memmove(nv_watch_text, nv_watch_text + drop, nv_watch_len - drop); nv_watch_len -= drop;
    }
    memcpy(nv_watch_text + nv_watch_len, line, L); nv_watch_len += L;
    IOSimpleLockUnlock(nv_watch_lock);
    kprintf("NVRM-xnu: WATCHPAGE %.*s", (int)L, line);
}

static const NvU64 kNvWinLo = 0x101300000ULL, kNvWinHi = 0x101340000ULL;
struct nv_win_ev { NvU64 t_ms, phys, a, b; uint32_t kind; int pid; NvU64 closed_ms; char name[20]; };
static nv_win_ev nv_win_ring[48]; static volatile UInt32 nv_win_n = 0;
static const char *const nv_win_kind[] = { "?", "RM-ALLOC", "RM-FREE", "MAP", "UNMAP", "REMAP", "STALE", "VA-ONLY" };
static volatile SInt32 nv_win_stale_n = 0;
static UInt32 nv_win_note(uint32_t kind, NvU64 phys, NvU64 a, NvU64 b)
{
    NvU64 ns = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &ns);
    const UInt32 i = (UInt32)OSIncrementAtomic((volatile SInt32 *)&nv_win_n);
    nv_win_ev &e = nv_win_ring[i % 48]; e.t_ms = ns / 1000000ULL; e.phys = phys; e.a = a; e.b = b; e.kind = kind;
    e.pid = 0; e.closed_ms = 0; e.name[0] = 0;
    return i;
}
static void nv_win_stamp(UInt32 slot, NvU64 pm, bool kern)
{
    if (!pm || kern) return;
    nv_win_ev &e = nv_win_ring[slot % 48];
    e.pid = nv_xnu_pmap_owner(pm, e.name, sizeof e.name, &e.closed_ms);
}

static void nv_watch_range(const char *what, NvU64 p, NvU64 len, NvU64 base, NvU64 bytes, NvU32 cache, NvBool contig)
{
    if (p < kNvWinHi && p + len > kNvWinLo) nv_win_note(what[0] == 'A' ? 1 : 2, p, len, bytes);
    const NvU64 end = p + len;
    for (NvU64 hit = nv_watch_hit(p, len); hit; ) {
        nv_watch_note(what, hit, base, bytes, cache, contig);
        NvU64 nx = hit + PAGE_SIZE;
        hit = (nx < end) ? nv_watch_hit(nx, end - nx) : 0;
    }
}

static const NvU64 kNvQHoldNs = 2000000000ULL, kNvQMaxBytes = 256ULL << 20, kNvQWhole = 64ULL << 10;
static const int   kNvQMax = 4096;
struct nv_q_ent { nv_xnu_alloc *a; NvU64 t_ns; NvU64 phys0; NvU64 *snap; NvU64 nsnap; };
static nv_q_ent nv_q[kNvQMax]; static int nv_q_head = 0, nv_q_n = 0; static NvU64 nv_q_bytes = 0;
static IOLock  *nv_q_lock = NULL;
static NvU64    nv_q_checked = 0, nv_q_dirty = 0, nv_q_early = 0, nv_q_nova = 0;
static NvU64    nv_q_min_early_ms = ~0ULL;
static const char *nv_q_selftest = "not run";

static inline bool nv_q_sampled(NvU64 off, NvU64 bytes) { return bytes <= kNvQWhole || (off & 0x3ff) < 64; }

static NvU64 nv_q_nsamples(NvU64 bytes) { return bytes <= kNvQWhole ? bytes / 8 : (bytes / 1024) * 8 + ((bytes % 1024) >= 64 ? 8 : (bytes % 1024) / 8); }
static NvU64 nv_q_snap(volatile const NvU64 *w, NvU64 bytes, NvU64 *snap)
{
    NvU64 k = 0;
    for (NvU64 off = 0; off + 8 <= bytes; off += 8) {
        if (!nv_q_sampled(off, bytes)) { off = (off | 0x3ff) - 7; continue; }
        snap[k++] = w[off >> 3];
    }
    return k;
}
static NvU64 nv_q_check(volatile const NvU64 *w, NvU64 bytes, const NvU64 *snap, NvU64 phys0, NvU32 cache, NvBool contig, NvU64 age_ms, bool log)
{
    NvU64 bad = 0, k = 0;
    for (NvU64 off = 0; off + 8 <= bytes; off += 8) {
        if (!nv_q_sampled(off, bytes)) { off = (off | 0x3ff) - 7; continue; }
        NvU64 v = w[off >> 3], want = snap[k++];
        if (v == want) continue;
        if (log && bad < 3) {
            char line[220];
            int n = snprintf(line, sizeof line, "LATE WRITE buf 0x%llx+%llu cache %u contig %u freed %llu ms ago: +0x%llx = 0x%016llx (was 0x%016llx)\n",
                             phys0, bytes, cache, (unsigned)contig, age_ms, off, v, want);
            if (n > 0) nv_watch_append(line, (size_t)n < sizeof line ? (size_t)n : sizeof line - 1);
        }
        bad++;
    }
    return bad;
}
static bool nv_pg_still_mapped(NvU64 phys, NvU64 *pm_out);
static const NvU64 kNvRetainMaxPages = 1024;
static NvU64 nv_q_retained = 0, nv_q_retained_pages = 0, nv_q_retain_over = 0, nv_q_retain_seen = 0;
static NvU64 nv_q_kmap_after_complete = 0;
static NvU64 nv_q_kmap_chained = 0, nv_q_kmap_solo = 0, nv_q_chain_unknown = 0;
static const char *nv_q_retain_selftest = "not run";
static NvU64 nv_q_retain_ring[8], nv_q_retain_pm[8]; static unsigned nv_q_retain_n = 0;
static nv_xnu_alloc *nv_q_retain_head = NULL, *nv_q_retain_tail = NULL;
static NvU64 nv_q_retain_released = 0, nv_q_retain_untracked = 0;
static NvU64 nv_q_allocation_pages(const nv_xnu_alloc *a)
{
    return a->contig ? (a->bytes + PAGE_SIZE - 1) / PAGE_SIZE : a->nptes;
}
static bool nv_q_has_mapper(nv_xnu_alloc *a, NvU64 *hit, NvU64 *pm)
{
    *hit = *pm = 0;
    if (!a->ptes || !a->nptes) return false;
    const NvU64 npg = nv_q_allocation_pages(a);
    for (NvU64 i = 0; i < npg; i++) {
        const NvU64 p = a->contig ? a->ptes[0] + i * PAGE_SIZE : a->ptes[i];
        if (nv_pg_still_mapped(p, pm)) { *hit = p; return true; }
    }
    return false;
}
static void nv_q_retain_append(nv_xnu_alloc *a)
{
    a->next = NULL;
    if (nv_q_retain_tail) nv_q_retain_tail->next = a;
    else nv_q_retain_head = a;
    nv_q_retain_tail = a;
}
static bool nv_q_retain_check(nv_xnu_alloc *a)
{
    NvU64 pm = 0, hit = 0;
    if (!nv_q_has_mapper(a, &hit, &pm)) return false;
    const NvU64 npg = nv_q_allocation_pages(a);
    if (nv_q_lock) IOLockLock(nv_q_lock);
    nv_q_retain_seen++;
    const bool over = npg > kNvRetainMaxPages || nv_q_retained_pages > kNvRetainMaxPages - npg;
    if (over) nv_q_retain_over++;
    if (nv_q_retain_n < 8) { nv_q_retain_ring[nv_q_retain_n] = hit; nv_q_retain_pm[nv_q_retain_n] = pm; }
    nv_q_retain_n++; nv_q_retained++; nv_q_retained_pages += npg;
    if (nv_q_lock) { nv_q_retain_append(a); IOLockUnlock(nv_q_lock); }
    else nv_q_retain_untracked++;
    if (over && nv_q_retain_over < 8)
        kprintf("NVRM-xnu: RETAIN alarm %llu pages reached - keeping STILL-MAPPED buffer for retry (page 0x%llx pmap 0x%llx)\n",
                kNvRetainMaxPages, hit, pm);
    return true;
}
static void nv_q_reap_retained(void)
{
    if (!nv_q_lock) return;
    nv_xnu_alloc *batch[16]; unsigned n = 0;
    IOLockLock(nv_q_lock);
    while (nv_q_retain_head && n < 16) {
        nv_xnu_alloc *a = nv_q_retain_head;
        nv_q_retain_head = a->next;
        if (!nv_q_retain_head) nv_q_retain_tail = NULL;
        a->next = NULL; batch[n++] = a;
    }
    IOLockUnlock(nv_q_lock);
    for (unsigned j = 0; j < n; j++) {
        nv_xnu_alloc *a = batch[j]; NvU64 hit = 0, pm = 0;
        if (nv_q_has_mapper(a, &hit, &pm)) {
            IOLockLock(nv_q_lock); nv_q_retain_append(a); IOLockUnlock(nv_q_lock);
            continue;
        }
        IOLockLock(nv_q_lock);
        nv_q_retained--; nv_q_retained_pages -= nv_q_allocation_pages(a); nv_q_retain_released++;
        IOLockUnlock(nv_q_lock);
        a->md->release(); if (a->ptes) kern_os_free(a->ptes); kern_os_free(a);
    }
}
static void nv_q_release(nv_xnu_alloc *a)
{
    a->md->complete();
    if (nv_q_retain_check(a)) return;
    a->md->release(); if (a->ptes) kern_os_free(a->ptes); kern_os_free(a);
}
static void nv_q_free_snap(NvU64 *snap, NvU64 n) { if (snap) IOFree(snap, (vm_size_t)(n * 8)); }

static void nv_q_put(nv_xnu_alloc *a)
{
    if (!nv_q_lock || !a->va) { if (a->va == NULL) OSIncrementAtomic64((volatile SInt64 *)&nv_q_nova); nv_q_release(a); return; }
    NvU64 now = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &now);
    NvU64 phys0 = a->md->getPhysicalSegment(0, NULL, kIOMemoryMapperNone);
    NvU64 nsnap = nv_q_nsamples(a->bytes);
    NvU64 *snap = nsnap ? (NvU64 *)IOMalloc((vm_size_t)(nsnap * 8)) : NULL;
    if (!snap) { OSIncrementAtomic64((volatile SInt64 *)&nv_q_nova); nv_q_release(a); return; }
    NvU64 got = nv_q_snap((volatile const NvU64 *)a->va, a->bytes, snap);
    if (got != nsnap) { OSIncrementAtomic64((volatile SInt64 *)&nv_q_nova); nv_q_free_snap(snap, nsnap); nv_q_release(a); return; }
    nv_xnu_alloc *out[8]; NvU64 outT[8], outP[8], *outS[8], outN[8]; int nout = 0; bool early[8];
    IOLockLock(nv_q_lock);
    while (nv_q_n && nout < 8) {
        nv_q_ent &e = nv_q[nv_q_head];
        bool due = now - e.t_ns >= kNvQHoldNs;
        bool full = nv_q_n >= kNvQMax || nv_q_bytes + a->bytes > kNvQMaxBytes;
        if (!due && !full) break;
        early[nout] = !due; out[nout] = e.a; outT[nout] = e.t_ns; outP[nout] = e.phys0; outS[nout] = e.snap; outN[nout] = e.nsnap; nout++;
        nv_q_bytes -= e.a->bytes; nv_q_head = (nv_q_head + 1) % kNvQMax; nv_q_n--;
    }
    bool held = nv_q_n < kNvQMax && nv_q_bytes + a->bytes <= kNvQMaxBytes;
    if (held) {
        nv_q_ent &e = nv_q[(nv_q_head + nv_q_n) % kNvQMax];
        e.a = a; e.t_ns = now; e.phys0 = phys0; e.snap = snap; e.nsnap = nsnap; nv_q_n++; nv_q_bytes += a->bytes;
    }
    IOLockUnlock(nv_q_lock);
    for (int i = 0; i < nout; i++) {
        nv_xnu_alloc *o = out[i];
        NvU64 bad = nv_q_check((volatile const NvU64 *)o->va, o->bytes, outS[i], outP[i], o->cache, o->contig, (now - outT[i]) / 1000000ULL, true);
        nv_q_free_snap(outS[i], outN[i]);
        OSIncrementAtomic64((volatile SInt64 *)&nv_q_checked);
        if (bad) OSIncrementAtomic64((volatile SInt64 *)&nv_q_dirty);
        if (early[i]) { OSIncrementAtomic64((volatile SInt64 *)&nv_q_early);
                        NvU64 ms = (now - outT[i]) / 1000000ULL; if (ms < nv_q_min_early_ms) nv_q_min_early_ms = ms; }
        nv_q_release(o);
    }
    if (!held) {
        NvU64 bad = nv_q_check((volatile const NvU64 *)a->va, a->bytes, snap, phys0, a->cache, a->contig, 0, true);
        nv_q_free_snap(snap, nsnap);
        OSIncrementAtomic64((volatile SInt64 *)&nv_q_checked); OSIncrementAtomic64((volatile SInt64 *)&nv_q_early);
        if (bad) OSIncrementAtomic64((volatile SInt64 *)&nv_q_dirty);
        nv_q_release(a);
    }
}

static void nv_q_init(void)
{
    if (nv_q_lock) return;
    IOLock *l = IOLockAlloc();
    if (!l) return;
    if (!OSCompareAndSwapPtr(NULL, l, (void * volatile *)&nv_q_lock)) { IOLockFree(l); return; }
    bool ok = true;
    const NvU64 sizes[2] = { 8192, 131072 };
    for (int k = 0; k < 2 && ok; k++) {
        IOBufferMemoryDescriptor *md = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut, sizes[k], PAGE_SIZE);
        if (!md) { ok = false; break; }
        volatile NvU64 *w = (volatile NvU64 *)md->getBytesNoCopy();
        NvU64 ns = nv_q_nsamples(sizes[k]);
        NvU64 *sn = (NvU64 *)IOMalloc((vm_size_t)(ns * 8));
        if (!w || !sn) { if (sn) IOFree(sn, (vm_size_t)(ns * 8)); md->release(); ok = false; break; }
        for (NvU64 j = 0; j < sizes[k] / 8; j++) w[j] = 0x5AFE0DD500000000ULL ^ j;
        bool counted = nv_q_snap(w, sizes[k], sn) == ns;
        bool clean = nv_q_check(w, sizes[k], sn, 0, 0, 0, 0, false) == 0;
        w[(sizes[k] / 2) >> 3] ^= 1;
        bool found = nv_q_check(w, sizes[k], sn, 0, 0, 0, 0, false) == 1;
        w[(sizes[k] / 2 + 512) >> 3] ^= 1;
        bool blind = sizes[k] <= kNvQWhole || nv_q_check(w, sizes[k], sn, 0, 0, 0, 0, false) == 1;
        ok = counted && clean && found && blind;
        IOFree(sn, (vm_size_t)(ns * 8)); md->release();
    }
    nv_q_selftest = ok ? "ok (count/clean/found/blind, whole+sampled)" : "FAILED — LATE WRITE counts below are not trustworthy";
    kprintf("NVRM-xnu: QUARANTINE armed, selftest %s\n", nv_q_selftest);
}

static int nv_watch_sysctl SYSCTL_HANDLER_ARGS
{
    char *buf = (char *)IOMalloc(sizeof nv_watch_text + 512);
    if (!buf) return ENOMEM;
    IOSimpleLockLock(nv_watch_lock);
    int h = snprintf(buf, 512, "selftest %s | allocs %llu frees %llu bytes-seen %llu\n"
                     "quarantine %s | checked %llu DIRTY %llu early %llu (shortest hold %lld ms) no-va %llu held %d (%llu KB) | events:\n",
                     nv_watch_selftest, nv_watch_allocs, nv_watch_frees, nv_watch_bytes_seen,
                     nv_q_selftest, nv_q_checked, nv_q_dirty, nv_q_early, (long long)(nv_q_min_early_ms == ~0ULL ? -1 : (long long)nv_q_min_early_ms), nv_q_nova, nv_q_n, nv_q_bytes >> 10);
    size_t H = (h > 0 && h < 512) ? (size_t)h : 0;
    memcpy(buf + H, nv_watch_text, nv_watch_len); buf[H + nv_watch_len] = 0;
    IOSimpleLockUnlock(nv_watch_lock);
    int err = SYSCTL_OUT(req, buf, H + nv_watch_len + 1);
    IOFree(buf, sizeof nv_watch_text + 512);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_watchpage, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_watch_sysctl, "A",
            "RM allocations/frees that covered the 2026-09-23 panic page (phys == 0x132b000 mod 1 GB)");

static void nv_watch_init(void)
{
    if (nv_watch_lock) return;
    IOSimpleLock *l = IOSimpleLockAlloc();
    if (!l) return;
    if (!OSCompareAndSwapPtr(NULL, l, (void * volatile *)&nv_watch_lock)) { IOSimpleLockFree(l); return; }
    bool pos = nv_watch_hit(0x4132a000ULL, 0x3000ULL) == 0x4132b000ULL;
    bool neg = nv_watch_hit(0x4132a000ULL, 0x1000ULL) == 0;
    bool two = nv_watch_hit(0x0132b000ULL + PAGE_SIZE, kNvWatchSlot) == 0x4132b000ULL;
    nv_watch_selftest = (pos && neg && two) ? "ok (hit/miss/next-slot)" : "FAILED — events below are not trustworthy";
    sysctl_register_oid(&sysctl__debug_nvrm_watchpage);
    kprintf("NVRM-xnu: WATCHPAGE armed, selftest %s\n", nv_watch_selftest);
}
extern "C" void nv_xnu_dump_live_allocs(void)
{
    kprintf("NVRM-xnu: DMA buffers alive: %d\n", nv_xnu_live_n);
    for (int i = 0; i < nv_xnu_live_n; i++) {
        nv_xnu_alloc *a = nv_xnu_live[i]; if (!a || !a->va) continue;
        const unsigned char *q = (const unsigned char *)a->va; NvU64 nz = 0, first = (NvU64)-1;
        for (NvU64 k = 0; k < a->bytes; k++) if (q[k]) { nz++; if (first == (NvU64)-1) first = k; }
        kprintf("NVRM-xnu:   #%d %llu KB contig=%u nonzero=%llu first@0x%llx head=%02x%02x%02x%02x %02x%02x%02x%02x\n",
                i, a->bytes >> 10, a->contig, nz, first, q[0], q[1], q[2], q[3], q[4], q[5], q[6], q[7]);
    }
}

extern "C" nv_state_t *nv_xnu_gpu_state(void);
static IOSimpleLock *nv_pm_lock = NULL;
static nv_xnu_alloc *nv_pm_head = NULL;
static SInt64 nv_pm_alias_n = 0, nv_pm_alias_live = 0, nv_pm_foreign_n = 0, nv_pm_deferred_n = 0, nv_pm_deferred_rel = 0,
              nv_pm_pinned_n = 0, nv_pm_pinned_bytes = 0, nv_pm_dev_alias_n = 0, nv_pm_dev_refused_n = 0, nv_pm_stale_n = 0,
              nv_pm_nolist_n = 0, nv_pm_chat_n = 0, nv_pm_chat_dropped = 0;
static const char *nv_pm_selftest = "not run";
struct nv_pm_ev { NvU64 t_s, phys, size, extra; NvU32 kind; };
static nv_pm_ev nv_pm_ring[48]; static unsigned nv_pm_ring_n = 0;
static const char *const nv_pm_kind[] = { "?", "ALIAS(first 8)", "FOREIGN-KMAP", "FREE-DEFERRED", "FREE-PINNED(leaked)",
                                          "DEVMAP-RAM-ALIASED", "DEVMAP-REFUSED", "STALE-CTX-REFUSED", "DEFERRED-RELEASED" };
static void nv_pm_note(NvU32 kind, NvU64 phys, NvU64 size, NvU64 extra)
{
    NvU64 ns = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &ns);
    if (nv_pm_lock) IOSimpleLockLock(nv_pm_lock);
    nv_pm_ev &e = nv_pm_ring[nv_pm_ring_n % 48]; nv_pm_ring_n++;
    e.t_s = ns / 1000000000ULL; e.phys = phys; e.size = size; e.extra = extra; e.kind = kind;
    if (nv_pm_lock) IOSimpleLockUnlock(nv_pm_lock);
    kprintf("NVRM-xnu: PHYSMAP %s phys 0x%llx +0x%llx extra 0x%llx\n", kind < 9 ? nv_pm_kind[kind] : "?", phys, size, extra);
}
static bool nv_pm_match(const nv_xnu_alloc *a, NvU64 start, NvU64 size, NvU64 *off)
{
    if (!a->ptes || !a->nptes || !size) return false;
    if (a->contig || a->nptes == 1) {
        NvU64 p0 = a->ptes[0];
        if (start < p0 || start + size > p0 + a->bytes || start + size < start) return false;
        *off = start - p0; return true;
    }
    const NvU64 pg = start & ~(NvU64)PAGE_MASK, last = (start + size - 1) & ~(NvU64)PAGE_MASK;
    for (NvU32 i = 0; i < a->nptes; i++) {
        if (a->ptes[i] != pg) continue;
        NvU32 k = 1; bool ok = true;
        for (NvU64 p = pg + PAGE_SIZE; p <= last; p += PAGE_SIZE, k++)
            if (i + k >= a->nptes || a->ptes[i + k] != p) { ok = false; break; }
        if (ok) { *off = (NvU64)i * PAGE_SIZE + (start & PAGE_MASK); return true; }
    }
    return false;
}
static void nv_pm_init(void)
{
    if (nv_pm_lock) return;
    IOSimpleLock *l = IOSimpleLockAlloc(); if (!l) return;
    if (!OSCompareAndSwapPtr(NULL, l, (void * volatile *)&nv_pm_lock)) { IOSimpleLockFree(l); return; }
    NvU64 pa[2] = { 0x5000, 0x9000 }, pc[1] = { 0x100000 }, off = 0;
    nv_xnu_alloc A; nvx2_memset(&A, 0, sizeof A); A.ptes = pa; A.nptes = 2; A.bytes = 0x2000; A.contig = NV_FALSE;
    nv_xnu_alloc C; nvx2_memset(&C, 0, sizeof C); C.ptes = pc; C.nptes = 1; C.bytes = 0x3000; C.contig = NV_TRUE;
    bool ok = nv_pm_match(&A, 0x9010, 0x20, &off) && off == 0x1010
           && !nv_pm_match(&A, 0x5ff0, 0x20, &off)
           && !nv_pm_match(&A, 0x7000, 0x10, &off)
           && nv_pm_match(&C, 0x102000, 0x1000, &off) && off == 0x2000
           && !nv_pm_match(&C, 0x102800, 0x1000, &off);
    nv_pm_selftest = ok ? "ok (2 hit, 3 miss)" : "FAILED — physmap aliasing is OFF";
    kprintf("NVRM-xnu: PHYSMAP armed, selftest %s\n", nv_pm_selftest);
}
static bool nv_pm_ok(void) { return nv_pm_lock && nv_pm_selftest[0] == 'o'; }
static void nv_pm_link(nv_xnu_alloc *a)
{
    if (!nv_pm_lock) return;
    IOSimpleLockLock(nv_pm_lock);
    a->prev = NULL; a->next = nv_pm_head; if (nv_pm_head) nv_pm_head->prev = a; nv_pm_head = a;
    IOSimpleLockUnlock(nv_pm_lock);
}
static bool nv_pm_retire(nv_xnu_alloc *a, bool pinned)
{
    if (!nv_pm_lock) return pinned;
    IOSimpleLockLock(nv_pm_lock);
    bool listed = a->prev || a->next || nv_pm_head == a;
    if (listed) {
        if (a->prev) a->prev->next = a->next; else nv_pm_head = a->next;
        if (a->next) a->next->prev = a->prev;
        a->prev = a->next = NULL;
    }
    a->freed = NV_TRUE; if (pinned) a->pinned = NV_TRUE;
    bool wait = a->kmaps > 0 || a->pinned;
    IOSimpleLockUnlock(nv_pm_lock);
    if (!listed) OSIncrementAtomic64((volatile SInt64 *)&nv_pm_nolist_n);
    return wait;
}
static nv_xnu_alloc *nv_pm_find(NvU64 start, NvU64 size, NvU64 *off)
{
    for (nv_xnu_alloc *a = nv_pm_head; a; a = a->next) if (!a->freed && nv_pm_match(a, start, size, off)) return a;
    return NULL;
}
static bool nv_pm_is_live(const nv_xnu_alloc *x)
{
    for (nv_xnu_alloc *a = nv_pm_head; a; a = a->next) if (a == x) return !a->freed;
    return false;
}
extern "C" void *nv_pm_alias(NvU64 start, NvU64 size, void **cookie)
{
    if (!nv_pm_ok()) return NULL;
    NvU64 off = 0; void *va = NULL; nv_xnu_alloc *a;
    IOSimpleLockLock(nv_pm_lock);
    a = nv_pm_find(start, size, &off);
    if (a && a->va) { a->kmaps++; va = (char *)a->va + off; *cookie = a; }
    IOSimpleLockUnlock(nv_pm_lock);
    if (!va) return NULL;
    SInt64 n = OSIncrementAtomic64((volatile SInt64 *)&nv_pm_alias_n); OSIncrementAtomic64((volatile SInt64 *)&nv_pm_alias_live);
    if (n < 8) nv_pm_note(1, start, size, a->bytes);
    return va;
}
extern "C" void nv_pm_alias_drop(void *cookie)
{
    nv_xnu_alloc *a = (nv_xnu_alloc *)cookie; if (!a || !nv_pm_lock) return;
    IOSimpleLockLock(nv_pm_lock);
    bool release = (--a->kmaps == 0) && a->freed && !a->pinned;
    IOSimpleLockUnlock(nv_pm_lock);
    OSDecrementAtomic64((volatile SInt64 *)&nv_pm_alias_live);
    if (release) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_deferred_rel); nv_pm_note(8, a->ptes ? a->ptes[0] : 0, a->bytes, 0); nv_q_put(a); }
}
extern "C" bool nv_pm_is_gpu(NvU64 start, NvU64 size)
{
    nv_state_t *nv = nv_xnu_gpu_state(); if (!nv) return false;
    for (int i = 0; i < NV_GPU_NUM_BARS; i++) {
        NvU64 b = nv->bars[i].cpu_address, s = nv->bars[i].size;
        if (b && s && start >= b && start + size <= b + s && start + size >= start) return true;
    }
    return false;
}
extern "C" void nv_pm_foreign(NvU64 start, NvU64 size, NvU32 mode, void *caller)
{
    OSIncrementAtomic64((volatile SInt64 *)&nv_pm_foreign_n);
    nv_pm_note(2, start, size, (NvU64)((uintptr_t)caller - (uintptr_t)&nv_pm_foreign));
}
extern "C" IOMemoryDescriptor *nv_pm_ram_submd(NvU64 start, NvU64 size)
{
    if (!nv_pm_ok()) return NULL;
    NvU64 off = 0; IOBufferMemoryDescriptor *md = NULL;
    IOSimpleLockLock(nv_pm_lock);
    nv_xnu_alloc *a = nv_pm_find(start, size, &off);
    if (a) { md = a->md; md->retain(); }
    IOSimpleLockUnlock(nv_pm_lock);
    if (!md) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_dev_refused_n); nv_pm_note(6, start, size, 0); return NULL; }
    IOMemoryDescriptor *s = IOSubMemoryDescriptor::withSubRange(md, (IOByteCount)off, (IOByteCount)size, kIODirectionInOut);
    md->release();
    OSIncrementAtomic64((volatile SInt64 *)&nv_pm_dev_alias_n); nv_pm_note(5, start, size, off);
    return s;
}
extern "C" void nv_pm_dev_refused(NvU64 start, NvU64 size) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_dev_refused_n); nv_pm_note(6, start, size, 1); }
extern "C" bool nv_xnu_chat(void)
{
    if (OSIncrementAtomic64((volatile SInt64 *)&nv_pm_chat_n) < 1500) return true;
    OSIncrementAtomic64((volatile SInt64 *)&nv_pm_chat_dropped); return false;
}
static int nv_pm_sysctl SYSCTL_HANDLER_ARGS
{
    const size_t cap = 8192; char *buf = (char *)IOMalloc(cap); if (!buf) return ENOMEM;
    size_t h = 0; int w;
    w = snprintf(buf, cap, "selftest %s\nkernel-alias %lld (live %lld) | FOREIGN-KMAP %lld | FREE-DEFERRED %lld (released %lld) | "
                 "FREE-PINNED %lld (%lld KB leaked) | DEVMAP-RAM-ALIASED %lld | DEVMAP-REFUSED %lld | STALE-CTX %lld | unlisted-free %lld | "
                 "kprintf dropped %lld\nevents (%u):\n", nv_pm_selftest, nv_pm_alias_n, nv_pm_alias_live, nv_pm_foreign_n, nv_pm_deferred_n,
                 nv_pm_deferred_rel, nv_pm_pinned_n, nv_pm_pinned_bytes >> 10, nv_pm_dev_alias_n, nv_pm_dev_refused_n, nv_pm_stale_n,
                 nv_pm_nolist_n, nv_pm_chat_dropped, nv_pm_ring_n);
    if (w > 0) h = (size_t)w < cap ? (size_t)w : cap - 1;
    if (nv_pm_lock) IOSimpleLockLock(nv_pm_lock);
    unsigned n = nv_pm_ring_n < 48 ? nv_pm_ring_n : 48;
    for (unsigned i = 0; i < n && h < cap - 160; i++) {
        const nv_pm_ev &e = nv_pm_ring[(nv_pm_ring_n - n + i) % 48];
        w = snprintf(buf + h, cap - h, "  t+%llus %s phys 0x%llx +0x%llx extra 0x%llx\n", e.t_s, e.kind < 9 ? nv_pm_kind[e.kind] : "?", e.phys, e.size, e.extra);
        if (w > 0) h += (size_t)w;
    }
    if (nv_pm_lock) IOSimpleLockUnlock(nv_pm_lock);
    int err = SYSCTL_OUT(req, buf, h + 1);
    IOFree(buf, cap);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_physmap, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_pm_sysctl, "A",
            "batch 47: RAM mapped by physical address — aliased, deferred, pinned, foreign, refused");
static bool nv_pm_registered = false;

#include <pexpert/pexpert.h>
static const uint64_t kKdkKprintf = 0xffffff8000b94da0ULL, kKdkPvHead = 0xffffff80011bd8c8ULL, kKdkAttr = 0xffffff80012de470ULL,
                      kKdkLastManaged = 0xffffff80012de468ULL, kKdkPmapInit = 0xffffff80011bd358ULL,
                      kKdkKernelPmap = 0xffffff8000266460ULL, kKdkVerifyFree = 0xffffff8000512880ULL;
static const uint8_t  kKdkVerifyBytes[16] = { 0x83,0x3d,0xd1,0xaa,0xca,0x00,0x00,0x0f,0x94,0xc0,0x83,0xff,0xfe,0x0f,0x94,0xc1 };
static const char *nv_pv_state = "not started";
static volatile uint8_t *nv_pv_head = NULL, *nv_pv_attr = NULL;
static uint32_t nv_pv_last = 0; static uint64_t nv_pv_kpmap = 0; static uint8_t *nv_pv_managed = NULL;
static NvU64 nv_pv_ppn0 = 0, nv_pv_above_last = 0, nv_pv_at_last = 0;
static bool nv_pv_chained(NvU64 ppn)
{
    const volatile uint8_t *head = nv_pv_head + ppn * 32;
    const NvU64 nxt = *(const volatile uint64_t *)head;
    if (nxt == (NvU64)(uintptr_t)head) return false;
    if (!nxt || (nxt & 7) || nxt < 0xffffff8000000000ULL) { nv_q_chain_unknown++; return false; }
    return true;
}
static bool nv_pg_still_mapped(NvU64 phys, NvU64 *pm_out)
{
    if (pm_out) *pm_out = 0;
    if (!nv_pv_head || !nv_pv_attr || !nv_pv_last) return false;
    const NvU64 ppn = phys >> 12;
    if (!ppn) { nv_pv_ppn0++; return false; }
    if (ppn > nv_pv_last) { nv_pv_above_last++; if (pm_out) *pm_out = ~1ULL; return false; }
    if (ppn == nv_pv_last) nv_pv_at_last++;
    if (!(nv_pv_attr[ppn] & 1)) { if (pm_out) *pm_out = ~0ULL; return true; }
    const NvU64 pm = *(const volatile uint64_t *)(nv_pv_head + ppn * 32 + 0x18);
    if (pm_out) *pm_out = pm;
    if (!pm) return false;
    if (pm == nv_pv_kpmap) {
        nv_q_kmap_after_complete++;
        if (nv_pv_chained(ppn)) {
            nv_q_kmap_chained++;
            if (pm_out) *pm_out = nv_pv_kpmap;
            return true;
        }
        nv_q_kmap_solo++;
        return false;
    }
    return true;
}
static bool nv_pv_slide2m = false; static volatile SInt32 nv_pv_started = 0;
static NvU64 nv_pv_passes, nv_pv_managed_n, nv_pv_n[4], nv_pv_klog;
static uint32_t nv_pv_lo[4] = { ~0u, ~0u, ~0u, ~0u }, nv_pv_hi[4];
static const char *const nv_pv_kind[] = { "?", "BAD-PMAP", "MANAGED-LOST", "MANAGED-GAINED" };
struct nv_pv_ev { NvU64 t_ms, val, phys; uint32_t ppn, kind; uint8_t raw[32]; };
static nv_pv_ev nv_pv_first, nv_pv_ring[16]; static unsigned nv_pv_ring_n;

static bool nv_pv_pmap_ok(uint64_t v) { return v == 0 || (v >= 0xffffff8000000000ULL && !(v & 7)); }

static void nv_pv_record(uint32_t kind, uint32_t ppn, uint64_t val, volatile const uint8_t *at)
{
    NvU64 ns = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &ns);
    nv_pv_ev e; nvx2_memset(&e, 0, sizeof e);
    e.t_ms = ns / 1000000ULL; e.val = val; e.ppn = ppn; e.kind = kind;
    volatile const uint8_t *row = (volatile const uint8_t *)((uintptr_t)at & ~(uintptr_t)31);
    for (int i = 0; i < 32; i++) e.raw[i] = row[i];
    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddressRange((mach_vm_address_t)(uintptr_t)at, 8, kIODirectionIn, kernel_task);
    if (md) { e.phys = md->getPhysicalSegment(0, NULL, kIOMemoryMapperNone); md->release(); }
    if (!nv_pv_first.kind) nv_pv_first = e;
    nv_pv_n[kind]++; if (ppn < nv_pv_lo[kind]) nv_pv_lo[kind] = ppn; if (ppn > nv_pv_hi[kind]) nv_pv_hi[kind] = ppn;
    nv_pv_ring[nv_pv_ring_n++ % 16] = e;
    if (nv_pv_klog++ < 64)
        kprintf("NVRM-xnu: PV-SENTINEL %s t=%llums page 0x%llx val 0x%llx corrupted bytes at phys 0x%llx\n",
                nv_pv_kind[kind], e.t_ms, (NvU64)ppn << 12, val, e.phys);
}

static void nv_pv_scan(uint32_t lo, uint32_t hi)
{
    for (uint32_t ppn = lo; ppn < hi; ppn++) {
        const bool was = (nv_pv_managed[ppn >> 3] >> (ppn & 7)) & 1;
        volatile uint8_t *ap = nv_pv_attr + ppn; const uint8_t av = *ap;
        if (was && !(av & 1)) {
            nv_pv_record(2, ppn, av, ap);
        } else if (!was && (av & 1)) {
            nv_pv_record(3, ppn, av, ap); nv_pv_managed[ppn >> 3] |= (uint8_t)(1u << (ppn & 7));
        }
        if (!was) continue;
        volatile UInt64 *pm = (volatile UInt64 *)(nv_pv_head + (uint64_t)ppn * 32 + 0x18); const uint64_t v = *pm;
        if (!nv_pv_pmap_ok(v)) {
            nv_pv_record(1, ppn, v, (volatile const uint8_t *)pm);
        }
    }
}

static NvU64 nv_win_pm[(0x101340000ULL - 0x101300000ULL) >> 12], nv_win_va[(0x101340000ULL - 0x101300000ULL) >> 12];
static NvU64 nv_win_stale_pm[(0x101340000ULL - 0x101300000ULL) >> 12];
static const NvU64 kNvWinPages = (0x101340000ULL - 0x101300000ULL) >> 12;
static uint8_t nv_wm_boot_attr[kNvWinPages];
static NvU64   nv_wm_boot_pm[kNvWinPages], nv_wm_boot_va[kNvWinPages];
static bool    nv_wm_boot_taken = false;
static void nv_win_scan(void)
{
    for (NvU64 i = 0; i < ((kNvWinHi - kNvWinLo) >> 12); i++) {
        const NvU64 ppn = (kNvWinLo >> 12) + i;
        if (ppn >= nv_pv_last) break;
        volatile const uint64_t *h = (volatile const uint64_t *)(nv_pv_head + ppn * 32);
        const NvU64 pm = h[3], va = h[2];
        if (pm && pm != nv_pv_kpmap && pm != nv_win_stale_pm[i]) {
            char nm[20]; NvU64 cm = 0;
            if (nv_xnu_pmap_owner(pm, nm, sizeof nm, &cm) > 0 && cm) {
                nv_win_stale_pm[i] = pm; OSIncrementAtomic(&nv_win_stale_n);
                nv_win_stamp(nv_win_note(6, ppn << 12, 2, va), pm, false);
            }
        }
        if (pm == nv_win_pm[i] && va == nv_win_va[i]) continue;
        const uint32_t knd = (!nv_win_pm[i] && !pm) ? 7 : !nv_win_pm[i] ? 3 : !pm ? 4 : 5;
        nv_win_stamp(nv_win_note(knd, ppn << 12, pm == nv_pv_kpmap ? 1 : (pm ? 2 : 0), va), pm, pm == nv_pv_kpmap);
        nv_win_pm[i] = pm; nv_win_va[i] = va;
    }
}

static void nv_pv_thread(void *, wait_result_t)
{
    const uint32_t slices = 20, step = (nv_pv_last + slices - 1) / slices;
    for (;;) {
        for (uint32_t s = 0; s < slices; s++) {
            const uint32_t lo = s * step, hi = (lo + step < nv_pv_last) ? lo + step : nv_pv_last;
            if (lo < hi) nv_pv_scan(lo, hi);
            IOSleep(250);
            nv_win_scan();
            nv_q_reap_retained();
        }
        nv_pv_passes++;
    }
}

static const char *nv_pv_arm(void)
{
    const uint64_t slide = (uint64_t)(uintptr_t)&kprintf - kKdkKprintf;
    nv_pv_slide2m = (slide & 0x1fffffULL) == 0;
    const volatile uint8_t *vf = (const volatile uint8_t *)(uintptr_t)(kKdkVerifyFree + slide);
    for (int i = 0; i < 16; i++) if (vf[i] != kKdkVerifyBytes[i]) return "REFUSED: slid _pmap_verify_free text != BootKC 24G830 bytes (kernel or KC differs)";
    if (*(const volatile int *)(uintptr_t)(kKdkPmapInit + slide) != 1) return "REFUSED: pmap_initialized != 1";
    nv_pv_head = *(uint8_t *const volatile *)(uintptr_t)(kKdkPvHead + slide);
    nv_pv_attr = *(uint8_t *const volatile *)(uintptr_t)(kKdkAttr + slide);
    nv_pv_last = *(const volatile uint32_t *)(uintptr_t)(kKdkLastManaged + slide);
    nv_pv_kpmap = *(const volatile uint64_t *)(uintptr_t)(kKdkKernelPmap + slide);
    if (!nv_pv_head || !nv_pv_attr || !nv_pv_pmap_ok(nv_pv_kpmap) || !nv_pv_kpmap) return "REFUSED: table pointers implausible";
    if (nv_pv_last < 0x100000u || nv_pv_last > 0x4000000u) return "REFUSED: last_managed_page implausible (not 4 GB..256 GB)";
    IOBufferMemoryDescriptor *probe = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut, PAGE_SIZE, PAGE_SIZE);
    if (!probe) return "REFUSED: probe alloc failed";
    ((volatile uint8_t *)probe->getBytesNoCopy())[0] = 1;
    const uint64_t pp = probe->getPhysicalSegment(0, NULL, kIOMemoryMapperNone) >> 12;
    const bool own = pp && pp < nv_pv_last && (nv_pv_attr[pp] & 1) && *(const volatile uint64_t *)(nv_pv_head + pp * 32 + 0x18) == nv_pv_kpmap;
    {
        IOBufferMemoryDescriptor *rt = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut, PAGE_SIZE, PAGE_SIZE);
        if (!rt) { nv_q_retain_selftest = "REFUSED: probe alloc failed"; }
        else {
            ((volatile uint8_t *)rt->getBytesNoCopy())[0] = 1;
            if (rt->prepare() != kIOReturnSuccess) { nv_q_retain_selftest = "REFUSED: prepare failed"; rt->release(); }
            else {
                const NvU64 pp = rt->getPhysicalSegment(0, NULL, kIOMemoryMapperNone);
                NvU64 pm1 = 0, pm2 = 0;
                const bool wired_reads_mapped = pp && (nv_pg_still_mapped(pp, &pm1) || pm1 == nv_pv_kpmap);
                rt->complete();
                const bool completed_is_free = pp && !nv_pg_still_mapped(pp, &pm2);
                const NvU64 ppn_c = pp >> 12;
                const bool solo_before   = !nv_pv_chained(ppn_c) && !nv_pg_still_mapped(pp, &pm2);
                IOMemoryMap *m2 = rt->map(kIOMapAnywhere | kIOMapUnique);
                bool chained_fires = false, released_after = false, arm_ran = (m2 != NULL && m2->getVirtualAddress() != (uintptr_t)rt->getBytesNoCopy());
                if (m2) {
                    ((volatile uint8_t *)m2->getVirtualAddress())[0] = 2;
                    NvU64 pm3 = 0;
                    chained_fires = nv_pv_chained(ppn_c) && nv_pg_still_mapped(pp, &pm3);
                    m2->release();
                    NvU64 pm4 = 0;
                    released_after = !nv_pv_chained(ppn_c) && !nv_pg_still_mapped(pp, &pm4);
                }
                nv_q_retain_selftest = !pp                  ? "FAIL: no physical segment"
                                     : !wired_reads_mapped  ? "FAIL: a WIRED page did not read as mapped (predicate blind)"
                                     : !completed_is_free   ? "FAIL: a COMPLETED page would still be retained (this is the bug)"
                                     : !arm_ran             ? "REFUSED: could not make a second mapping (chain arm did not run)"
                                     : !solo_before         ? "FAIL: a SOLO page read as chained (would retain every healthy free = b52)"
                                     : !chained_fires       ? "FAIL: a DOUBLE-MAPPED page read as safe to free (this is the class-F bug)"
                                     : !released_after      ? "FAIL: page still read as chained after the 2nd mapping went away (cannot let go)"
                                                            : "ok (solo releases; 2nd mapping RETAINS; release lets go)";
                rt->release();
            }
        }
    }
    probe->release();
    if (!own) return "REFUSED: own kernel page's pv head is not kernel_pmap (layout differs)";
    if (nv_pv_pmap_ok(0xff371322ff271322ULL) || !nv_pv_pmap_ok(0) || !nv_pv_pmap_ok(nv_pv_kpmap)) return "REFUSED: detector selftest";
    const size_t mb = (nv_pv_last + 7) / 8;
    nv_pv_managed = (uint8_t *)IOMalloc(mb); if (!nv_pv_managed) return "REFUSED: no memory for the MANAGED snapshot";
    nvx2_memset(nv_pv_managed, 0, mb);
    for (uint32_t p = 0; p < nv_pv_last; p++) if (nv_pv_attr[p] & 1) { nv_pv_managed[p >> 3] |= (uint8_t)(1u << (p & 7)); nv_pv_managed_n++; }
    for (NvU64 i = 0; i < kNvWinPages; i++) {
        const NvU64 ppn = (kNvWinLo >> 12) + i;
        if (ppn >= nv_pv_last) break;
        nv_wm_boot_attr[i] = nv_pv_attr[ppn];
        nv_wm_boot_pm[i]   = *(const volatile uint64_t *)(nv_pv_head + ppn * 32 + 0x18);
        nv_wm_boot_va[i]   = *(const volatile uint64_t *)(nv_pv_head + ppn * 32 + 0x10);
    }
    nv_wm_boot_taken = true;
    return NULL;
}

static int nv_pv_sysctl SYSCTL_HANDLER_ARGS
{
    const size_t cap = 16384; char *buf = (char *)IOMalloc(cap); if (!buf) return ENOMEM;
    size_t h = 0; int w;
    w = snprintf(buf, cap, "state %s | pages 0x%x managed %llu | passes %llu (full every 5 s) | repair %s | slide 2MB-aligned %s\n",
                 nv_pv_state, nv_pv_last, nv_pv_managed_n, nv_pv_passes, "REMOVED (detect only)", nv_pv_slide2m ? "yes" : "NO");
    if (w > 0) h = (size_t)w < cap ? (size_t)w : cap - 1;
    for (int k = 1; k < 4 && h < cap - 200; k++) {
        w = snprintf(buf + h, cap - h, "%s %llu%s", nv_pv_kind[k], nv_pv_n[k], k < 3 ? " | " : "\n");
        if (w > 0) h += (size_t)w;
    }
    for (int k = 1; k < 4 && h < cap - 200; k++) if (nv_pv_n[k]) {
        w = snprintf(buf + h, cap - h, "  %s pages 0x%llx..0x%llx\n", nv_pv_kind[k], (NvU64)nv_pv_lo[k] << 12, (NvU64)nv_pv_hi[k] << 12);
        if (w > 0) h += (size_t)w;
    }
    w = snprintf(buf + h, cap - h, "RETAIN selftest %s | seen %llu | held %llu buffers / %llu pages (alarm %llu) | OVER-CAP %llu | RETRY-RELEASED %llu UNTRACKED %llu | kernel-map-after-complete %llu (CHAINED %llu, solo %llu, chain-unknown %llu) | first:",
                 nv_q_retain_selftest, nv_q_retain_seen, nv_q_retained, nv_q_retained_pages, kNvRetainMaxPages,
                 nv_q_retain_over, nv_q_retain_released, nv_q_retain_untracked, nv_q_kmap_after_complete,
                  nv_q_kmap_chained, nv_q_kmap_solo, nv_q_chain_unknown);
    if (w > 0) h += (size_t)w;
    for (unsigned i = 0; i < (nv_q_retain_n < 8 ? nv_q_retain_n : 8) && h < cap - 300; i++) {
        w = snprintf(buf + h, cap - h, " 0x%llx@%s0x%llx", nv_q_retain_ring[i],
                     nv_q_retain_pm[i] == ~0ULL ? "UNMANAGED" : "pm", nv_q_retain_pm[i]);
        if (w > 0) h += (size_t)w;
    }
    if (h < cap - 2) buf[h++] = '\n';
    w = snprintf(buf + h, cap - h, "WINDOW 0x%llx-0x%llx events %u STALE %d | held now:", kNvWinLo, kNvWinHi, (unsigned)nv_win_n, (int)nv_win_stale_n);
    if (w > 0) h += (size_t)w;
    for (NvU64 i = 0; i < ((kNvWinHi - kNvWinLo) >> 12) && h < cap - 400; i++) if (nv_win_pm[i]) {
        char nm[20]; NvU64 cm = 0;
        const int pid = nv_win_pm[i] == nv_pv_kpmap ? 0 : nv_xnu_pmap_owner(nv_win_pm[i], nm, sizeof nm, &cm);
        if (pid > 0) w = snprintf(buf + h, cap - h, " %llx:U(%d %s%s)@0x%llx", (kNvWinLo >> 12) + i, pid, nm, cm ? " CLOSED" : "", nv_win_va[i] & ~0xfffULL);
        else w = snprintf(buf + h, cap - h, " %llx:%s@0x%llx", (kNvWinLo >> 12) + i, nv_win_pm[i] == nv_pv_kpmap ? "K" : "U(-)", nv_win_va[i] & ~0xfffULL);
        if (w > 0) h += (size_t)w;
    }
    if (h < cap - 2) buf[h++] = '\n';
    {
        const unsigned wn = nv_win_n < 48 ? (unsigned)nv_win_n : 48;
        for (unsigned i = 0; i < wn && h < cap - 400; i++) {
            const nv_win_ev &e = nv_win_ring[(nv_win_n - wn + i) % 48];
            if (e.kind == 1 || e.kind == 2) w = snprintf(buf + h, cap - h, "  w t=%llums %s page 0x%llx len 0x%llx of alloc %llu B\n", e.t_ms, nv_win_kind[e.kind], e.phys, e.a, e.b);
            else w = snprintf(buf + h, cap - h, "  w t=%llums %s page 0x%llx by %s va 0x%llx pid %d %s%s\n", e.t_ms, e.kind < 7 ? nv_win_kind[e.kind] : "?", e.phys,
                              e.a == 1 ? "kernel_pmap" : e.a == 2 ? "other-pmap" : "none", e.b & ~0xfffULL,
                              e.pid, e.name, e.closed_ms ? " CLOSED" : "");
            if (w > 0) h += (size_t)w;
        }
    }
    const unsigned n = nv_pv_ring_n < 16 ? nv_pv_ring_n : 16;
    for (unsigned i = 0; i <= n && h < cap - 260; i++) {
        const nv_pv_ev &e = i == 0 ? nv_pv_first : nv_pv_ring[(nv_pv_ring_n - n + i - 1) % 16];
        if (!e.kind) continue;
        w = snprintf(buf + h, cap - h, "  %s t=%llums %s page 0x%llx val 0x%llx at phys 0x%llx raw", i == 0 ? "FIRST" : "ev",
                     e.t_ms, nv_pv_kind[e.kind < 4 ? e.kind : 0], (NvU64)e.ppn << 12, e.val, e.phys);
        if (w > 0) h += (size_t)w;
        for (int b = 0; b < 32 && h < cap - 8; b++) { w = snprintf(buf + h, cap - h, "%s%02x", (b & 7) ? "" : " ", e.raw[b]); if (w > 0) h += (size_t)w; }
        if (h < cap - 2) buf[h++] = '\n';
    }
    int err = SYSCTL_OUT(req, buf, h + 1);
    IOFree(buf, cap);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_pvsentinel, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_pv_sysctl, "A",
            "batch 48: pv_head_table / pmap_phys_attributes corruption — detected and located (read-only)");

static size_t nv_wm_unarmed(char *buf, size_t cap)
{
    const int w = snprintf(buf, cap, "UNKNOWN: pv sentinel not armed, so attr/pv cannot be read (%s)\n", nv_pv_state);
    return (w > 0 && (size_t)w < cap) ? (size_t)w : 0;
}

static int nv_wm_why_sysctl SYSCTL_HANDLER_ARGS
{
    const size_t cap = 160 * 1024;
    char *buf = (char *)IOMalloc(cap);
    if (!buf) return ENOMEM;
    size_t h = 0; int w; int err;
    if (!nv_pv_head || !nv_pv_attr || !nv_pv_last) {
        h = nv_wm_unarmed(buf, cap);
        err = SYSCTL_OUT(req, buf, h + 1); IOFree(buf, cap); return err;
    }

    w = snprintf(buf + h, cap - h,
                 "PREDICATE _vm_page_free_prepare_object: panic if (1) ppn > last_managed_page, (2) attr&1 == 0, (3) pv.pmap != 0\n"
                 "last_managed_page 0x%x | window ppn 0x%llx-0x%llx -> BRANCH 1 EXCLUDED BY ARITHMETIC\n"
                 "kernel_pmap 0x%llx | arm-time snapshot %s\n"
                 "A. WINDOW PAGES (attr bit0 = PHYS_MANAGED)\n"
                 "   %-12s %-22s %-22s\n",
                 nv_pv_last, kNvWinLo >> 12, kNvWinHi >> 12, nv_pv_kpmap,
                 nv_wm_boot_taken ? "taken" : "NOT TAKEN (arm failed)",
                 "page", "AT ARM TIME", "NOW");
    if (w > 0) h += (size_t)w;

    NvU64 un_now = 0, un_boot = 0, pm_now = 0, pm_boot = 0;
    for (NvU64 i = 0; i < kNvWinPages && h < cap - 1024; i++) {
        const NvU64 ppn = (kNvWinLo >> 12) + i;
        if (ppn >= nv_pv_last) break;
        const uint8_t at = ((const volatile uint8_t *)nv_pv_attr)[ppn];
        const NvU64 pm = *(const volatile uint64_t *)(nv_pv_head + ppn * 32 + 0x18);
        if (!(at & 1)) un_now++;
        if (pm) pm_now++;
        if (!(nv_wm_boot_attr[i] & 1)) un_boot++;
        if (nv_wm_boot_pm[i]) pm_boot++;
        w = snprintf(buf + h, cap - h, "   0x%-10llx attr 0x%02x %-9s pm %-3s | attr 0x%02x %-9s pm %-3s%s\n",
                     ppn << 12,
                     nv_wm_boot_attr[i], (nv_wm_boot_attr[i] & 1) ? "MANAGED" : "UNMANAGED", nv_wm_boot_pm[i] ? "set" : "0",
                     at, (at & 1) ? "MANAGED" : "UNMANAGED", pm ? "set" : "0",
                     (!(at & 1) && !(nv_wm_boot_attr[i] & 1)) ? "  <== fenced since boot (no vm_page exists)"
                     : (!(at & 1)) ? "  <== BRANCH 2 RISK: was MANAGED at arm, UNMANAGED now"
                     : (!(nv_wm_boot_attr[i] & 1)) ? "  <== was UNMANAGED at arm, MANAGED now"
                     : "");
        if (w > 0) h += (size_t)w;
    }
    const bool fenced_all = (un_now == kNvWinPages && un_boot == kNvWinPages);
    const bool became_un  = (un_now > un_boot);
    const char *verdict =
        fenced_all ? "FENCED — whole window is outside XNU managed RAM (EFI-reserved at boot); no vm_page exists, "
                     "so branches 2 and 3 are unreachable by any free path. This is the reservation, not a fault."
      : became_un  ? "BRANCH 2 — pages became UNMANAGED since arm time; a vm_page_free of one panics"
      : un_now     ? "PARTIAL — some window pages unmanaged, unchanged since arm: the reservation boundary"
      : pm_now     ? "BRANCH 3 — a pv entry is live on a window page right now"
                   : "NEITHER right now: window reads managed and unmapped at this instant";
    w = snprintf(buf + h, cap - h,
                 "   totals: UNMANAGED arm %llu now %llu of %llu | pv.pmap set arm %llu now %llu\n"
                 "   branch1 seen: ppn0 %llu, above_last %llu, at_last %llu (report-only)\n"
                 "   VERDICT: %s\n",
                 un_boot, un_now, kNvWinPages, pm_boot, pm_now,
                 nv_pv_ppn0, nv_pv_above_last, nv_pv_at_last,
                 verdict);
    if (w > 0) h += (size_t)w;

    w = snprintf(buf + h, cap - h, "B. UNMANAGED RUNS in [0,0x%x)  (attr bit0 clear)\n", nv_pv_last);
    if (w > 0) h += (size_t)w;
    const NvU64 wlo = kNvWinLo >> 12, whi = kNvWinHi >> 12;
    NvU64 runs = 0, total = 0, start = 0; bool in_run = false; NvU64 shown = 0;
    const volatile uint8_t *attr = (const volatile uint8_t *)nv_pv_attr;
    for (NvU64 p = 0; p <= (NvU64)nv_pv_last; p++) {
        const bool um = (p < (NvU64)nv_pv_last) && !(attr[p] & 1);
        if (um) { total++; if (!in_run) { in_run = true; start = p; } }
        else if (in_run) {
            in_run = false; runs++;
            const bool hits = (start < whi) && (p > wlo);
            if ((hits || shown < 32) && h < cap - 1024) {
                shown++;
                w = snprintf(buf + h, cap - h, "   %s0x%llx-0x%llx  %llu pages (%llu KB)\n",
                             hits ? "*** CONTAINS THE PANIC WINDOW *** " : "",
                             start << 12, p << 12, p - start, ((p - start) << 12) >> 10);
                if (w > 0) h += (size_t)w;
            }
        }
    }
    w = snprintf(buf + h, cap - h, "   %llu runs, %llu unmanaged pages (%llu MB) of 0x%x; managed %llu\n",
                 runs, total, (total << 12) >> 20, nv_pv_last, nv_pv_managed_n);
    if (w > 0) h += (size_t)w;

    err = SYSCTL_OUT(req, buf, h + 1);
    IOFree(buf, cap);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_winwhy, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_wm_why_sysctl, "A",
            "batch 54: which _vm_page_free_prepare_object branch the panic window trips (read-only)");

static const NvU64 kPvcMaxPages = 262144;
static int nv_pvchain_sysctl SYSCTL_HANDLER_ARGS
{
    const size_t cap = 64 * 1024;
    char *buf = (char *)IOMalloc(cap);
    if (!buf) return ENOMEM;
    size_t h = 0; int w; int err;
    if (!nv_pv_head || !nv_pv_attr || !nv_pv_last) {
        w = snprintf(buf, cap, "UNARMED: the pv sentinel never armed on this boot, so NOTHING was measured.\n"
                               "  This is not evidence for or against the class-F diagnosis.\n");
        h = (w > 0) ? (size_t)w : 0;
        err = SYSCTL_OUT(req, buf, h + 1); IOFree(buf, cap); return err;
    }
    NvU64 allocs = 0, pages = 0, solo = 0, chained = 0, foreign = 0, unmapped = 0, unmanaged = 0, outside = 0;
    NvU64 shown = 0; bool truncated = false;
    w = snprintf(buf + h, cap - h,
                 "PVCHAIN b59b — do our live RM buffers have a SECOND mapper? (read-only; chain never dereferenced)\n"
                 "  pv_head_table %p  kernel_pmap 0x%llx  last_managed_page 0x%x\n"
                 "  layout from this kernel: +0x00 qlink.next, +0x10 va_and_flags, +0x18 pmap, stride 32\n"
                 "  ONE mapper <=> qlink.next == &pv_head_table[ppn]\n\n",
                 (void *)nv_pv_head, nv_pv_kpmap, nv_pv_last);
    if (w > 0) h += (size_t)w;
    for (int i = 0; i < nv_xnu_live_n && !truncated; i++) {
        nv_xnu_alloc *a = nv_xnu_live[i];
        if (!a || !a->ptes || !a->nptes || a->freed) continue;
        allocs++;
        const NvU64 npg = a->contig ? (a->bytes + PAGE_SIZE - 1) / PAGE_SIZE : a->nptes;
        NvU64 a_chained = 0;
        for (NvU64 k = 0; k < npg; k++) {
            if (pages >= kPvcMaxPages) { truncated = true; break; }
            pages++;
            const NvU64 phys = a->contig ? a->ptes[0] + k * PAGE_SIZE : a->ptes[k];
            const NvU64 ppn = phys >> 12;
            if (!ppn || ppn > nv_pv_last) { outside++; continue; }
            if (!(nv_pv_attr[ppn] & 1)) { unmanaged++; continue; }
            const volatile uint8_t *head = nv_pv_head + ppn * 32;
            const NvU64 pm  = *(const volatile uint64_t *)(head + 0x18);
            const NvU64 nxt = *(const volatile uint64_t *)head;
            if (!pm) { unmapped++; continue; }
            if (pm != nv_pv_kpmap) { foreign++; continue; }
            if (nxt == (NvU64)(uintptr_t)head) { solo++; continue; }
            chained++; a_chained++;
            if (shown < 12 && h < cap - 200) {
                w = snprintf(buf + h, cap - h, "  CHAINED page 0x%llx  qlink.next 0x%llx  (alloc %d, %llu KB, %s)\n",
                             phys, nxt, i, a->bytes >> 10, a->contig ? "contig" : "scattered");
                if (w > 0) h += (size_t)w; shown++;
            }
        }
        if (a_chained && h < cap - 200) {
            w = snprintf(buf + h, cap - h, "  -> alloc %d: %llu of %llu pages chained\n", i, a_chained, npg);
            if (w > 0) h += (size_t)w;
        }
    }
    w = snprintf(buf + h, cap - h,
                 "\n  allocs %llu | pages examined %llu%s\n"
                 "  SOLO %llu (us only, safe to free) | CHAINED %llu (us + another mapper = the class-F shape)\n"
                 "  FOREIGN-ROOTED %llu | unmapped %llu | unmanaged %llu | outside managed RAM %llu\n"
                 "  VERDICT: %s\n",
                 allocs, pages, truncated ? " (TRUNCATED at the cap — this is a LOWER BOUND, not a total)" : "",
                 solo, chained, foreign, unmapped, unmanaged, outside,
                 chained  ? "CONFIRMED — live RM buffers carry a second mapper; released these and XNU panicked"
                 : foreign ? "FOREIGN-ROOTED pages exist — the earlier own branch would have caught these"
                 : pages   ? "NOT SEEN AT THIS INSTANT — no live page is chained right now. A free-time race is not "
                             "excluded by a snapshot; only the kmap_chained counter can see that."
                           : "NO PAGES EXAMINED — no live allocations; this measures nothing");
    if (w > 0) h += (size_t)w;
    err = SYSCTL_OUT(req, buf, h + 1);
    IOFree(buf, cap);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_pvchain, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_pvchain_sysctl, "A",
            "batch 59b: do live RM buffers have a second pv mapper chained on qlink (read-only)");

static int nv_wm_raw_sysctl SYSCTL_HANDLER_ARGS
{
    const size_t cap = 96 * 1024;
    char *buf = (char *)IOMalloc(cap);
    if (!buf) return ENOMEM;
    uint8_t *pg = (uint8_t *)IOMalloc(PAGE_SIZE);
    if (!pg) { IOFree(buf, cap); return ENOMEM; }
    size_t h = 0; int w;
    w = snprintf(buf, cap, "C. WINDOW BYTES 0x%llx-0x%llx via physical readBytes (no mapping, no pv entry)\n",
                 kNvWinLo, kNvWinHi);
    if (w > 0) h = (size_t)w;
    NvU64 zero_pages = 0, read_fail = 0;
    for (NvU64 i = 0; i < kNvWinPages && h < cap - 512; i++) {
        const NvU64 pa = kNvWinLo + (i << 12);
        IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress(pa, PAGE_SIZE, kIODirectionOut);
        if (!md) { read_fail++; w = snprintf(buf + h, cap - h, "   0x%llx  REFUSED: no descriptor\n", pa); if (w > 0) h += (size_t)w; continue; }
        const IOByteCount got = md->readBytes(0, pg, PAGE_SIZE);
        md->release();
        if (got != PAGE_SIZE) { read_fail++; w = snprintf(buf + h, cap - h, "   0x%llx  REFUSED: readBytes got %llu\n", pa, (NvU64)got); if (w > 0) h += (size_t)w; continue; }
        NvU64 nz = 0; for (int b = 0; b < PAGE_SIZE; b++) if (pg[b]) nz++;
        if (!nz) zero_pages++;
        char sig[9]; int sn = 0;
        for (int b = 0; b < 8; b++) sig[sn++] = (pg[b] >= 0x20 && pg[b] < 0x7f) ? (char)pg[b] : '.';
        sig[sn] = 0;
        w = snprintf(buf + h, cap - h, "   0x%llx  nonzero %4llu/4096  head %02x%02x%02x%02x%02x%02x%02x%02x  \"%s\"\n",
                     pa, nz, pg[0], pg[1], pg[2], pg[3], pg[4], pg[5], pg[6], pg[7], sig);
        if (w > 0) h += (size_t)w;
    }
    w = snprintf(buf + h, cap - h, "   %llu pages all-zero, %llu unreadable, of %llu\n", zero_pages, read_fail, kNvWinPages);
    if (w > 0) h += (size_t)w;
    const int err = SYSCTL_OUT(req, buf, h + 1);
    IOFree(pg, PAGE_SIZE); IOFree(buf, cap);
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrm_winraw, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, NULL, 0, nv_wm_raw_sysctl, "A",
            "batch 54: the panic window's bytes, read through the physical aperture (read-only)");

static void nv_pv_start(void)
{
    if (!OSCompareAndSwap(0, 1, &nv_pv_started)) return;
    const char *why = nv_pv_arm();
    sysctl_register_oid(&sysctl__debug_nvrm_pvsentinel);
    sysctl_register_oid(&sysctl__debug_nvrm_winwhy);
    sysctl_register_oid(&sysctl__debug_nvrm_winraw);
    sysctl_register_oid(&sysctl__debug_nvrm_pvchain);
    if (why) { nv_pv_state = why; kprintf("NVRM-xnu: PV-SENTINEL %s\n", why); return; }
    thread_t t = NULL;
    if (kernel_thread_start(nv_pv_thread, NULL, &t) != KERN_SUCCESS) { nv_pv_state = "REFUSED: thread start failed"; return; }
    thread_deallocate(t);
    nv_pv_state = "ok (text bytes, own page -> kernel_pmap, detector fires on pixel)";
    kprintf("NVRM-xnu: PV-SENTINEL armed: pages 0x%x managed %llu detect-only\n", nv_pv_last, nv_pv_managed_n);
}

NV_STATUS NV_API_CALL nv_alloc_pages(nv_state_t *nv, NvU32 page_count, NvU64 page_size, NvBool contiguous, NvU32 cache_type,
                                     NvBool zeroed, NvBool unencrypted, NvS32 node_id, NvU64 *pte_array, void **priv_data)
{
    NvU64 bytes = (NvU64)page_count * PAGE_SIZE;
    if (nv_xnu_chat()) kprintf("NVRM-xnu: >nv_alloc_pages count=%u page_size=0x%llx contig=%u cache=%u zeroed=%u (%llu KB)\n",
            page_count, page_size, contiguous, cache_type, zeroed, bytes >> 10);
    IOOptionBits opts = kIODirectionInOut | kIOMemoryKernelUserShared;
    if (contiguous || page_size > PAGE_SIZE) opts |= kIOMemoryPhysicallyContiguous;
    if (cache_type == NV_MEMORY_UNCACHED) opts |= kIOMapInhibitCache;
    else if (cache_type == NV_MEMORY_WRITECOMBINED) opts |= kIOMapWriteCombineCache;
    uint64_t mask = (nv_xnu_dma_bits >= 64) ? 0xFFFFFFFFFFFFF000ULL : ((1ULL << nv_xnu_dma_bits) - 1) & ~0xFFFULL;
    IOBufferMemoryDescriptor *md = IOBufferMemoryDescriptor::inTaskWithPhysicalMask(kernel_task, opts, (mach_vm_size_t)bytes, mask);
    if (!md) { kprintf("NVRM-xnu:   inTaskWithPhysicalMask FAILED (%llu KB)\n", bytes >> 10); return NV_ERR_NO_MEMORY; }
    if (md->prepare() != kIOReturnSuccess) { kprintf("NVRM-xnu:   prepare FAILED\n"); md->release(); return NV_ERR_NO_MEMORY; }
    void *va = md->getBytesNoCopy();
    if (zeroed && va) nvx2_memset(va, 0, (size_t)bytes);
    NvU32 n = contiguous ? 1 : page_count;
    for (NvU32 i = 0; i < n; i++) {
        IOByteCount len = 0;
        addr64_t phys = md->getPhysicalSegment((IOByteCount)((NvU64)i * PAGE_SIZE), &len, kIOMemoryMapperNone);
        if (phys == 0) { kprintf("NVRM-xnu:   getPhysicalSegment(page %u) FAILED\n", i); md->complete(); md->release(); return NV_ERR_NO_MEMORY; }
        pte_array[i] = phys;
    }
    nv_xnu_alloc *a = (nv_xnu_alloc *)kern_os_malloc(sizeof *a);
    if (!a) { md->complete(); md->release(); return NV_ERR_NO_MEMORY; }
    nvx2_memset(a, 0, sizeof *a);
    a->md = md; a->va = va; a->page_size = page_size; a->bytes = bytes; a->page_count = page_count; a->contig = contiguous; a->cache = cache_type;
    a->ptes = (NvU64 *)kern_os_malloc((size_t)n * sizeof(NvU64));
    if (a->ptes) { for (NvU32 i = 0; i < n; i++) a->ptes[i] = pte_array[i]; a->nptes = n; }
    nv_pm_init();
    if (!nv_pm_registered && nv_pm_lock) { nv_pm_registered = true; sysctl_register_oid(&sysctl__debug_nvrm_physmap); }
    nv_pm_link(a);
    nv_pv_start();
    *priv_data = a;
    if (nv_xnu_live_n < 512) nv_xnu_live[nv_xnu_live_n++] = a;
    nv_watch_init(); nv_q_init(); OSIncrementAtomic64((volatile SInt64 *)&nv_watch_allocs); OSAddAtomic64((SInt64)bytes, (volatile SInt64 *)&nv_watch_bytes_seen);
    if (contiguous) nv_watch_range("ALLOC", pte_array[0], bytes, pte_array[0], bytes, cache_type, contiguous);
    else for (NvU32 i = 0; i < n; i++) nv_watch_range("ALLOC", pte_array[i], PAGE_SIZE, pte_array[0], bytes, cache_type, contiguous);
    if (nv_xnu_chat()) kprintf("NVRM-xnu:   pages ok va=0x%llx pte[0]=0x%llx\n", (unsigned long long)(uintptr_t)va, pte_array[0]);
    return NV_OK;
}
NV_STATUS NV_API_CALL nv_free_pages(nv_state_t *nv, NvU32 page_count, NvBool contiguous, NvU32 cache_type, void *priv_data)
{
    TRC; nv_xnu_alloc *a = (nv_xnu_alloc *)priv_data; if (!a) return NV_OK;
    for (int i = 0; i < nv_xnu_live_n; i++) if (nv_xnu_live[i] == a) nv_xnu_live[i] = NULL;
    OSIncrementAtomic64((volatile SInt64 *)&nv_watch_frees);
    bool pinned = false;
    for (NvU64 off = 0; off < a->bytes; ) {
        IOByteCount len = 0; addr64_t p = a->md->getPhysicalSegment((IOByteCount)off, &len, kIOMemoryMapperNone);
        if (!p || !len) break;
        if (off + len > a->bytes) len = (IOByteCount)(a->bytes - off);
        nv_watch_range("FREE", p, len, a->md->getPhysicalSegment(0, NULL, kIOMemoryMapperNone), a->bytes, a->cache, a->contig);
        if (nv_xnu_kmap_overlap(p, len)) pinned = true;
        off += len;
    }
    if (pinned) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_pinned_n); OSAddAtomic64((SInt64)a->bytes, (volatile SInt64 *)&nv_pm_pinned_bytes);
                  nv_pm_note(4, a->ptes ? a->ptes[0] : 0, a->bytes, 0); }
    const NvU64 pm0 = a->ptes ? a->ptes[0] : 0, pmb = a->bytes;
    if (nv_pm_retire(a, pinned)) {
        if (!pinned) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_deferred_n); nv_pm_note(3, pm0, pmb, 0); }
        return NV_OK;
    }
    nv_q_put(a); return NV_OK;
}
void *NV_API_CALL nv_alloc_kernel_mapping(nv_state_t *nv, void *pAllocPrivate, NvU64 pageIndex, NvU32 pageOffset, NvU64 size, void **pPrivate)
{
    TRC;
    if (!pPrivate) return NULL;
    *pPrivate = NULL;
    if (!nv_pm_ok() || !pAllocPrivate || !size || pageOffset >= PAGE_SIZE ||
        pageIndex > (~0ULL - pageOffset) / PAGE_SIZE) return NULL;
    const NvU64 off = pageIndex * PAGE_SIZE + pageOffset;
    nv_xnu_alloc *a = (nv_xnu_alloc *)pAllocPrivate;
    void *va = NULL;
    IOSimpleLockLock(nv_pm_lock);
    if (nv_pm_is_live(a) && a->va && off < a->bytes && size <= a->bytes - off) {
        a->kmaps++;
        va = (char *)a->va + off;
        *pPrivate = a;
    }
    IOSimpleLockUnlock(nv_pm_lock);
    if (va) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_alias_n); OSIncrementAtomic64((volatile SInt64 *)&nv_pm_alias_live); }
    return va;
}
void NV_API_CALL nv_free_kernel_mapping(nv_state_t *nv, void *pAllocPrivate, void *address, void *pPrivate)
{
    TRC;
    if (pPrivate) nv_pm_alias_drop(pPrivate);
}

NV_STATUS NV_API_CALL nv_dma_map_alloc(nv_dma_device_t *dev, NvU64 page_count, NvU64 *va_array, NvBool contig, NvBool cache_type, void **priv)
{
    kprintf("NVRM-xnu: >nv_dma_map_alloc pages=%llu contig=%u first=0x%llx (identity)\n", page_count, contig, va_array ? va_array[0] : 0);
    *priv = NULL; return NV_OK;
}
NV_STATUS NV_API_CALL nv_dma_unmap_alloc(nv_dma_device_t *dev, NvU64 page_count, NvU64 *va_array, void **priv) { TRC; return NV_OK; }

const void *NV_API_CALL nv_get_firmware(nv_state_t *nv, nv_firmware_type_t fw_type, nv_firmware_chip_family_t fw_chip_family, const void **fw_buf, NvU32 *fw_size)
{
    const char *path = nv_xnu_fw_path(fw_type, fw_chip_family);
    kprintf("NVRM-xnu: >nv_get_firmware type=%u family=%u -> %s\n", fw_type, fw_chip_family, path);
    if (!path || !*path) return NULL;
    vfs_context_t ctx = vfs_context_current();
    vnode_t vp = NULL;
    int err = vnode_open(path, FREAD, 0, 0, &vp, ctx);
    if (err) { kprintf("NVRM-xnu:   vnode_open failed %d\n", err); return NULL; }
    struct vnode_attr va; VATTR_INIT(&va); VATTR_WANTED(&va, va_data_size);
    if (vnode_getattr(vp, &va, ctx) != 0) { kprintf("NVRM-xnu:   vnode_getattr failed\n"); vnode_close(vp, FREAD, ctx); return NULL; }
    size_t size = (size_t)va.va_data_size;
    void *buf = kern_os_malloc(size);
    if (!buf) { kprintf("NVRM-xnu:   no memory for %zu bytes\n", size); vnode_close(vp, FREAD, ctx); return NULL; }
    int resid = 0;
    err = vn_rdwr(UIO_READ, vp, (caddr_t)buf, (int)size, 0, UIO_SYSSPACE, IO_NODELOCKED, vfs_context_ucred(ctx), &resid, vfs_context_proc(ctx));
    vnode_close(vp, FREAD, ctx);
    if (err || resid != 0) { kprintf("NVRM-xnu:   vn_rdwr err=%d resid=%d\n", err, resid); kern_os_free(buf); return NULL; }
    *fw_buf = buf; *fw_size = (NvU32)size;
    kprintf("NVRM-xnu:   firmware loaded: %zu bytes\n", size);
    return buf;
}
void NV_API_CALL nv_put_firmware(const void *fw_handle) { TRC; if (fw_handle) kern_os_free((void *)fw_handle); }

struct os_wait_queue { IOLock *lock; };
NV_STATUS NV_API_CALL os_alloc_wait_queue(os_wait_queue **wq)
{
    TRC; os_wait_queue *q = (os_wait_queue *)kern_os_malloc(sizeof *q); if (!q) return NV_ERR_NO_MEMORY;
    q->lock = IOLockAlloc(); if (!q->lock) { kern_os_free(q); return NV_ERR_NO_MEMORY; }
    *wq = q; return NV_OK;
}
void NV_API_CALL os_free_wait_queue(os_wait_queue *wq) { TRC; if (wq) { IOLockFree(wq->lock); kern_os_free(wq); } }
void NV_API_CALL os_wait_uninterruptible(os_wait_queue *wq) { TRC; IOLockLock(wq->lock); IOLockSleep(wq->lock, wq, THREAD_UNINT); IOLockUnlock(wq->lock); }
void NV_API_CALL os_wait_interruptible(os_wait_queue *wq)   { TRC; IOLockLock(wq->lock); IOLockSleep(wq->lock, wq, THREAD_INTERRUPTIBLE); IOLockUnlock(wq->lock); }
void NV_API_CALL os_wake_up(os_wait_queue *wq)              { TRC; IOLockLock(wq->lock); IOLockWakeup(wq->lock, wq, false); IOLockUnlock(wq->lock); }

static void nv_xnu_work_thread(void *item, wait_result_t)
{
    kprintf("NVRM-xnu: work item %p running\n", item);
    rm_execute_work_item(NULL, item);
    thread_terminate(current_thread());
}
NV_STATUS NV_API_CALL os_queue_work_item(struct os_work_queue *queue, void *item)
{
    TRC; thread_t t;
    if (kernel_thread_start(nv_xnu_work_thread, item, &t) != KERN_SUCCESS) return NV_ERR_NO_MEMORY;
    thread_deallocate(t); return NV_OK;
}
NV_STATUS NV_API_CALL os_schedule(void) { IOSleep(1); return NV_OK; }

NvU64 NV_API_CALL os_get_monotonic_tick_resolution_ns(void) { TRC; return 1; }
NvU64 NV_API_CALL os_get_max_user_va(void) { TRC; return 0x00007FFFFFFFFFFFULL; }
NvU32 NV_API_CALL os_get_grid_csp_support(void) { TRC; return 0; }
void  NV_API_CALL os_add_record_for_crashLog(void *p, NvU32 n) { TRC; }
void  NV_API_CALL os_delete_record_for_crashLog(void *p) { TRC; }
void  NV_API_CALL nv_get_disp_smmu_stream_ids(nv_state_t *nv, NvU32 *a, NvU32 *b) { TRC; if (a) *a = 0; if (b) *b = 0; }
void  NV_API_CALL nv_acpi_methods_init(NvU32 *handlesPresent) { TRC; if (handlesPresent) *handlesPresent = 0; }
void  NV_API_CALL nv_acpi_methods_uninit(void) { TRC; }

// Laptop panel backlight. RM only finds an internal panel's backlight when it can read the panel's
// backlight tables from the firmware: the NBCI _DSM functions GETOBJBYTYPE (0x10) and GETBACKLIGHT
// (0x14), plus the display ACPI ids from _DOD. Without them NV0073_CTRL_CMD_SPECIFIC_GET_BACKLIGHT_BRIGHTNESS
// fails with NV_ERR_NOT_SUPPORTED and NVKMS never calls nvkms_register_backlight().
// Both are evaluated on the GPU's ACPI node, as kernel-open/nvidia/nv-acpi.c does. Only NBCI is answered:
// RM asks each _DSM GUID for its functions separately (_acpiDsmSupportedFuncCacheInit), so every other
// GUID (NVHG, MXM, NVOP, GPS, JT, NVPCF...) stays "not supported", as before.
extern "C" {
#include "nbci.h"
}
#define NV_XNU_MAX_ACPI_DSM_PARAM_SIZE 1024                                                  // NV_MAX_ACPI_DSM_PARAM_SIZE (nv-linux.h)
static const NvU8 nv_xnu_nbci_guid[16] = { 0x75, 0x0B, 0xA5, 0xD4, 0xC7, 0x65, 0xF7, 0x46,      // NBCI_DSM_GUID_STR
                                           0xBF, 0xB7, 0x41, 0x51, 0x4C, 0xEA, 0x02, 0x44 };    // (acpidsmguids.h)

// The GPU's ACPI node: AppleACPIPCI puts its IOACPIPlane path on the PCI device as "acpi-path".
static IOACPIPlatformDevice *nv_xnu_gpu_acpi(nv_state_t *nv)
{
    static IOACPIPlatformDevice *acpi;      // one GPU per driver instance; kept for the life of the kext
    if (acpi || !nv) return acpi;
    nv_xnu_pci_slot *s = (nv_xnu_pci_slot *)os_pci_init_handle(nv->pci_info.domain, nv->pci_info.bus,
                                                               nv->pci_info.slot, nv->pci_info.function, NULL, NULL);
    OSString *path = s ? OSDynamicCast(OSString, s->pci->getProperty("acpi-path")) : NULL;
    IORegistryEntry *e = path ? IORegistryEntry::fromPath(path->getCStringNoCopy()) : NULL;
    acpi = OSDynamicCast(IOACPIPlatformDevice, e);
    if (acpi) kprintf("NVRM-xnu: GPU ACPI node %s\n", path->getCStringNoCopy());
    else if (e) e->release();
    return acpi;
}

// nv_acpi_extract_object (nv-acpi.c): integers, buffers and packages of them, flattened into one buffer.
static NV_STATUS nv_xnu_acpi_extract(OSObject *o, NvU8 *buf, NvU32 cap, NvU32 *size)
{
    *size = 0;
    if (!o) return NV_OK;
    if (OSNumber *n = OSDynamicCast(OSNumber, o)) {
        NvU64 v = n->unsigned64BitValue(); NvU32 len = (v >> 32) ? 8 : 4;
        *size = len; if (cap < len) return NV_ERR_BUFFER_TOO_SMALL;
        memcpy(buf, &v, len); return NV_OK;
    }
    if (OSData *d = OSDynamicCast(OSData, o)) {
        *size = d->getLength(); if (cap < d->getLength()) return NV_ERR_BUFFER_TOO_SMALL;
        if (d->getLength()) memcpy(buf, d->getBytesNoCopy(), d->getLength());
        return NV_OK;
    }
    if (OSArray *a = OSDynamicCast(OSArray, o)) {
        NvU32 used = 0;
        for (unsigned i = 0; i < a->getCount(); i++) {
            NvU32 len = 0; NV_STATUS st = nv_xnu_acpi_extract(a->getObject(i), buf + used, cap - used, &len);
            if (st != NV_OK) { *size = used; return st; }
            used += len;
        }
        *size = used; return NV_OK;
    }
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS NV_API_CALL nv_acpi_dod_method(nv_state_t *nv, NvU32 *pOutData, NvU32 *pSize)
{
    TRC;
    IOACPIPlatformDevice *acpi = nv_xnu_gpu_acpi(nv);
    if (!acpi || !pOutData || !pSize) return NV_ERR_INVALID_ARGUMENT;
    NvU32 count = *pSize / sizeof(NvU32);
    OSObject *o = NULL;
    if (acpi->evaluateObject("_DOD", &o) != kIOReturnSuccess) { OSSafeReleaseNULL(o); return NV_ERR_GENERIC; }
    OSArray *dod = OSDynamicCast(OSArray, o);
    NV_STATUS st = NV_ERR_GENERIC;
    if (dod && dod->getCount() <= count) {
        st = NV_OK; *pSize = 0;
        for (unsigned i = 0; i < dod->getCount(); i++) {
            OSNumber *n = OSDynamicCast(OSNumber, dod->getObject(i));
            if (!n) { st = NV_ERR_GENERIC; break; }
            pOutData[i] = n->unsigned32BitValue(); *pSize += sizeof(NvU32);
        }
    }
    kprintf("NVRM-xnu: _DOD -> 0x%x, %u display id(s)\n", st, dod ? dod->getCount() : 0);
    OSSafeReleaseNULL(o);
    return st;
}

NV_STATUS NV_API_CALL nv_acpi_dsm_method(nv_state_t *nv, NvU8 *pAcpiDsmGuid, NvU32 acpiDsmRev, NvBool acpiNvpcfDsmFunction,
                                         NvU32 acpiDsmSubFunction, void *pInParams, NvU16 inParamSize, NvU32 *outStatus,
                                         void *pOutData, NvU16 *pSize)
{
    TRC;
    if (!pAcpiDsmGuid || !pInParams || inParamSize > NV_XNU_MAX_ACPI_DSM_PARAM_SIZE || !pOutData || !pSize)
        return NV_ERR_INVALID_ARGUMENT;
    if (acpiNvpcfDsmFunction || memcmp(pAcpiDsmGuid, nv_xnu_nbci_guid, sizeof nv_xnu_nbci_guid) != 0 ||
        (acpiDsmSubFunction != 0 /* NV_ACPI_ALL_FUNC_SUPPORT */ && acpiDsmSubFunction != NV_NBCI_FUNC_GETOBJBYTYPE &&
         acpiDsmSubFunction != NV_NBCI_FUNC_GETBACKLIGHT))
        return NV_ERR_NOT_SUPPORTED;
    IOACPIPlatformDevice *acpi = nv_xnu_gpu_acpi(nv);
    if (!acpi) return NV_ERR_NOT_SUPPORTED;

    OSObject *args[4] = { OSData::withBytes(pAcpiDsmGuid, 16), OSNumber::withNumber(acpiDsmRev, 32),
                          OSNumber::withNumber(acpiDsmSubFunction, 32),
                          inParamSize ? OSData::withBytes(pInParams, inParamSize) : OSData::withCapacity(1) };
    OSObject *o = NULL;
    IOReturn r = (args[0] && args[1] && args[2] && args[3]) ? acpi->evaluateObject("_DSM", &o, args, 4) : kIOReturnNoMemory;
    for (OSObject *a : args) OSSafeReleaseNULL(a);
    NV_STATUS st = NV_ERR_OPERATING_SYSTEM;
    if (r == kIOReturnSuccess) {
        NvU32 len = 0;
        st = nv_xnu_acpi_extract(o, (NvU8 *)pOutData, *pSize, &len);
        *pSize = (NvU16)len;
    }
    OSSafeReleaseNULL(o);
    static unsigned logged;
    if (logged < 8) { logged++; kprintf("NVRM-xnu: NBCI _DSM 0x%x -> 0x%x (%u B)\n", acpiDsmSubFunction, st, (unsigned)*pSize); }
    return st;
}
void  NV_API_CALL nv_get_screen_info(nv_state_t *nv, NvU64 *pa, NvU32 *w, NvU32 *h, NvU32 *depth, NvU32 *pitch, NvU64 *size)
{
    TRC;
    if (pa) *pa = 0; if (w) *w = 0; if (h) *h = 0;
    if (depth) *depth = 0; if (pitch) *pitch = 0; if (size) *size = 0;
    IOPlatformExpert *pe = IOService::getPlatform();
    if (!pe) return;
    PE_Video pv;
    memset(&pv, 0, sizeof pv);
    if (pe->getConsoleInfo(&pv) != kIOReturnSuccess) return;
    if (!pv.v_baseAddr || !pv.v_rowBytes || !pv.v_width || !pv.v_height) return;
    NvU64 base = (NvU64)pv.v_baseAddr & ~3ull;
    NvU64 sz   = (NvU64)pv.v_rowBytes * (NvU64)pv.v_height;
    if (pa)    *pa    = base;
    if (w)     *w     = (NvU32)pv.v_width;
    if (h)     *h     = (NvU32)pv.v_height;
    if (depth) *depth = (NvU32)pv.v_depth;
    if (pitch) *pitch = (NvU32)pv.v_rowBytes;
    if (size)  *size  = sz;
    static bool logged;
    if (!logged) {
        logged = true;
        kprintf("NVRM-xnu: nv_get_screen_info -> base 0x%llx %ux%u depth %u pitch %u size %llu\n",
                (unsigned long long)base, (unsigned)pv.v_width, (unsigned)pv.v_height,
                (unsigned)pv.v_depth, (unsigned)pv.v_rowBytes, (unsigned long long)sz);
    }
}
NvU32 NV_API_CALL nv_get_dev_minor(nv_state_t *nv) { TRC; return 0; }

struct nv_nano_timer { nv_state_t *nv; void *event; thread_call_t call; };
static void nv_xnu_nano_fire(thread_call_param_t p0, thread_call_param_t p1)
{
    nv_nano_timer_t *t = (nv_nano_timer_t *)p0;
    rm_run_nano_timer_callback(NULL, t->nv, t->event);
}
void NV_API_CALL nv_create_nano_timer(nv_state_t *nv, void *pTmrEvent, nv_nano_timer_t **pt)
{
    TRC; nv_nano_timer_t *t = (nv_nano_timer_t *)kern_os_malloc(sizeof *t);
    if (!t) { *pt = NULL; return; }
    t->nv = nv; t->event = pTmrEvent; t->call = thread_call_allocate(nv_xnu_nano_fire, t);
    *pt = t;
}
void NV_API_CALL nv_start_nano_timer(nv_state_t *nv, nv_nano_timer_t *t, NvU64 timens)
{
    if (!t || !t->call) return;
    uint64_t deadline; clock_interval_to_deadline((uint32_t)(timens / 1000ULL), kMicrosecondScale, &deadline);
    thread_call_enter_delayed(t->call, deadline);
}
void NV_API_CALL nv_cancel_nano_timer(nv_state_t *nv, nv_nano_timer_t *t) { if (t && t->call) thread_call_cancel(t->call); }
void NV_API_CALL nv_destroy_nano_timer(nv_state_t *nv, nv_nano_timer_t *t)
{
    TRC; if (!t) return; if (t->call) { thread_call_cancel(t->call); thread_call_free(t->call); } kern_os_free(t);
}

extern "C" nv_file_private_t *nv_xnu_lookup_fd(NvS32 fd, void **priv, NvBool *isCtl);
extern "C" nv_alloc_mapping_context_t *nv_xnu_mmap_context(void *priv);
nv_file_private_t *NV_API_CALL nv_get_file_private(NvS32 fd, NvBool ctl, void **os_private)
{
    TRC; NvBool isCtl = NV_FALSE; nv_file_private_t *fp = nv_xnu_lookup_fd(fd, os_private, &isCtl);
    if (!fp) { kprintf("NVRM-xnu: nv_get_file_private(fd %d) -> none\n", fd); return NULL; }
    if (!!isCtl != !!ctl) { kprintf("NVRM-xnu: nv_get_file_private(fd %d): wanted %s, is %s\n", fd, ctl ? "ctl" : "device", isCtl ? "ctl" : "device"); return NULL; }
    return fp;
}
extern "C" IOMemoryDescriptor *nv_xnu_alloc_md(void *priv, NvU64 *bytes, NvU32 *cache)
{
    nv_xnu_alloc *a = (nv_xnu_alloc *)priv; if (!a) return NULL;
    IOMemoryDescriptor *md = NULL;
    if (nv_pm_lock) {
        IOSimpleLockLock(nv_pm_lock);
        if (nv_pm_is_live(a)) { md = a->md; md->retain(); if (bytes) *bytes = a->bytes; if (cache) *cache = a->cache; }
        IOSimpleLockUnlock(nv_pm_lock);
    }
    if (!md) { OSIncrementAtomic64((volatile SInt64 *)&nv_pm_stale_n); nv_pm_note(7, 0, 0, 0); }
    return md;
}
NV_STATUS NV_API_CALL nv_alloc_user_mapping(nv_state_t *nv, void *pAllocPrivate, NvU64 pageIndex, NvU32 pageOffset, NvU64 size, NvU32 protect, NvU64 *pUserAddress, void **ppPrivate)
{
    TRC; nv_xnu_alloc *a = (nv_xnu_alloc *)pAllocPrivate; if (!a || pageIndex >= a->page_count) return NV_ERR_INVALID_ARGUMENT;
    IOByteCount len = 0;
    addr64_t phys = a->md->getPhysicalSegment((IOByteCount)(pageIndex * PAGE_SIZE), &len, kIOMemoryMapperNone);
    if (!phys) return NV_ERR_GENERIC;
    *pUserAddress = (NvU64)phys + pageOffset; if (ppPrivate) *ppPrivate = NULL;
    return NV_OK;
}
void NV_API_CALL nv_free_user_mapping(nv_state_t *nv, void *pAllocPrivate, NvU64 userAddress, void *pPrivate) { TRC; }
NV_STATUS NV_API_CALL os_match_mmap_offset(void *pAllocPrivate, NvU64 offset, NvU64 *pPageIndex)
{
    TRC; nv_xnu_alloc *a = (nv_xnu_alloc *)pAllocPrivate; if (!a) return NV_ERR_OBJECT_NOT_FOUND;
    for (NvU32 i = 0; i < a->page_count; i++) {
        IOByteCount len = 0;
        addr64_t phys = a->md->getPhysicalSegment((IOByteCount)((NvU64)i * PAGE_SIZE), &len, kIOMemoryMapperNone);
        if (phys == offset) { *pPageIndex = i; return NV_OK; }
    }
    kprintf("NVRM-xnu: os_match_mmap_offset: 0x%llx not in a %u-page alloc\n", offset, a->page_count);
    return NV_ERR_OBJECT_NOT_FOUND;
}
NV_STATUS NV_API_CALL nv_add_mapping_context_to_file(nv_state_t *nv, nv_usermap_access_params_t *nvuap, NvU32 prot, void *pAllocPriv, NvU64 pageIndex, NvU32 fd)
{
    TRC; void *priv = NULL;
    nv_file_private_t *nvfp = nv_get_file_private((NvS32)fd, NV_IS_CTL_DEVICE(nv), &priv);
    if (!nvfp) return NV_ERR_INVALID_ARGUMENT;
    nv_alloc_mapping_context_t *c = nv_xnu_mmap_context(priv);
    if (!c) return NV_ERR_INVALID_ARGUMENT;
    if (c->valid) return NV_ERR_STATE_IN_USE;
    nvx2_memset(c, 0, sizeof *c);
    if (NV_IS_CTL_DEVICE(nv)) { c->alloc = pAllocPriv; c->page_index = pageIndex; }
    else {
        NvU64 n = nvuap->memArea.numRanges;
        if (n == 0 || n > 4096) return NV_ERR_INVALID_ARGUMENT;
        c->memArea.pRanges = (MemoryRange *)kern_os_malloc((size_t)(sizeof(MemoryRange) * n));
        if (!c->memArea.pRanges) return NV_ERR_NO_MEMORY;
        nvx2_memcpy(c->memArea.pRanges, nvuap->memArea.pRanges, (size_t)(sizeof(MemoryRange) * n));
        c->memArea.numRanges = n;
    }
    c->access_start = nvuap->access_start; c->access_size = nvuap->access_size;
    c->prot = prot; c->caching = nvuap->caching; c->valid = NV_TRUE;
    if (nv_xnu_chat()) kprintf("NVRM-xnu: mapping context on fd %u: %s page_index=%llu ranges=%llu access=0x%llx+0x%llx caching=%u prot=0x%x\n",
            fd, NV_IS_CTL_DEVICE(nv) ? "sysmem" : "device", pageIndex, c->memArea.numRanges, c->access_start, c->access_size, c->caching, prot);
    return NV_OK;
}
void NV_API_CALL nv_put_file_private(void *os_private) { TRC; }
NvBool NV_API_CALL nv_is_gpu_accessible(nv_state_t *nv) { TRC; return NV_TRUE; }
static nv_state_t nv_xnu_ctl_state;
nv_state_t *NV_API_CALL nv_get_ctl_state(void)
{
    static bool init = false;
    if (!init) { nvx2_memset(&nv_xnu_ctl_state, 0, sizeof nv_xnu_ctl_state); nv_xnu_ctl_state.flags |= NV_FLAG_CONTROL; init = true; }
    return &nv_xnu_ctl_state;
}

struct nv_xnu_upin { NvU32 magic; IOMemoryDescriptor *md; NvU64 page_count; };
struct nv_xnu_ureg { nv_xnu_alloc a; nv_xnu_upin *pin; void *import_priv; };
static const NvU32 kNvUpinMagic = 0x55504e31u;
static volatile SInt64 nv_upin_live = 0, nv_upin_pages = 0, nv_upin_total = 0;
static const NvU64 kNvUpinMaxPages = 1ULL << 20;

NV_STATUS NV_API_CALL os_lock_user_pages(void *address, NvU64 page_count, void **page_array, NvU32 flags)
{
    if (!address || !page_count || !page_array || ((uintptr_t)address & PAGE_MASK)) return NV_ERR_INVALID_ARGUMENT;
    if (page_count > kNvUpinMaxPages) return NV_ERR_INVALID_LIMIT;
    const bool write = (flags & 1u) != 0;
    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddressRange((mach_vm_address_t)(uintptr_t)address,
                                                                  (mach_vm_size_t)(page_count * PAGE_SIZE),
                                                                  write ? kIODirectionInOut : kIODirectionOut, current_task());
    if (!md) return NV_ERR_NO_MEMORY;
    const IOReturn pr = md->prepare();
    if (pr != kIOReturnSuccess) {
        md->release();
        kprintf("NVRM-xnu: os_lock_user_pages(%p, %llu pages) prepare -> 0x%x\n", address, page_count, pr);
        return NV_ERR_INVALID_ADDRESS;
    }
    nv_xnu_upin *p = (nv_xnu_upin *)kern_os_malloc(sizeof *p);
    if (!p) { md->complete(); md->release(); return NV_ERR_NO_MEMORY; }
    p->magic = kNvUpinMagic; p->md = md; p->page_count = page_count;
    *page_array = p;
    OSIncrementAtomic64(&nv_upin_live); OSAddAtomic64((SInt64)page_count, &nv_upin_pages);
    const SInt64 k = OSIncrementAtomic64(&nv_upin_total);
    if (k < 4 || ((k + 1) & 0x3FF) == 0)
        kprintf("NVRM-xnu: user pages PINNED #%lld: %llu pages at %p (%s) | live pins %lld, %lld pages\n",
                (long long)k + 1, page_count, address, write ? "rw" : "ro", (long long)nv_upin_live, (long long)nv_upin_pages);
    return NV_OK;
}

NV_STATUS NV_API_CALL nv_register_user_pages(nv_state_t *nv, NvU64 page_count, NvU64 *phys_addr, void *import_priv,
                                             void **priv_data, NvBool unprotected)
{
    nv_xnu_upin *p = priv_data ? (nv_xnu_upin *)*priv_data : NULL;
    if (!p || p->magic != kNvUpinMagic || !phys_addr || page_count > p->page_count) {
        kprintf("NVRM-xnu: nv_register_user_pages(%llu pages): *priv_data %p is not a pin (import_priv %p) — refused\n",
                page_count, (void *)p, import_priv);
        return NV_ERR_INVALID_ARGUMENT;
    }
    const uint64_t lim = (nv_xnu_dma_bits >= 64) ? ~0ULL : ((1ULL << nv_xnu_dma_bits) - 1);
    for (NvU64 i = 0; i < page_count; i++) {
        IOByteCount len = 0;
        const addr64_t ph = p->md->getPhysicalSegment((IOByteCount)(i * PAGE_SIZE), &len, kIOMemoryMapperNone);
        if (!ph || (ph & PAGE_MASK) || ph + PAGE_SIZE - 1 > lim) {
            kprintf("NVRM-xnu: nv_register_user_pages page %llu/%llu phys 0x%llx refused (dma bits %u)\n", i, page_count,
                    (unsigned long long)ph, nv_xnu_dma_bits);
            return NV_ERR_INVALID_ADDRESS;
        }
        phys_addr[i] = ph;
    }
    nv_xnu_ureg *r = (nv_xnu_ureg *)kern_os_malloc(sizeof *r);
    if (!r) return NV_ERR_NO_MEMORY;
    nvx2_memset(r, 0, sizeof *r);
    r->a.bytes = page_count * PAGE_SIZE; r->a.page_count = (NvU32)page_count; r->a.page_size = PAGE_SIZE; r->a.cache = NV_MEMORY_CACHED;
    r->pin = p; r->import_priv = import_priv;
    *priv_data = r;
    return NV_OK;
}

void NV_API_CALL nv_unregister_user_pages(nv_state_t *nv, NvU64 page_count, void **import_priv, void **priv_data)
{
    if (!priv_data || !*priv_data) return;
    nv_xnu_ureg *r = (nv_xnu_ureg *)*priv_data;
    nv_xnu_upin *p = r->pin;
    if (import_priv) *import_priv = r->import_priv;
    *priv_data = p;
    kern_os_free(r);
}

NV_STATUS NV_API_CALL os_unlock_user_pages(NvU64 page_count, void *page_array, NvU32 flags)
{
    nv_xnu_upin *p = (nv_xnu_upin *)page_array;
    if (!p || p->magic != kNvUpinMagic) {
        kprintf("NVRM-xnu: os_unlock_user_pages(%llu pages) got %p which is not a pin — NOT unpinned\n", page_count, page_array);
        return NV_ERR_INVALID_ARGUMENT;
    }
    p->magic = 0;
    p->md->complete(); p->md->release();
    OSDecrementAtomic64(&nv_upin_live); OSAddAtomic64(-(SInt64)p->page_count, &nv_upin_pages);
    kern_os_free(p);
    return NV_OK;
}

}
