/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * May the integrated GPU's display engine read a frame straight out of the NVIDIA GPU's memory (zero copy)? The display engine would
 * be a bus master reading the NVIDIA GPU's BAR1 window across the host bridge, which only some chipsets route. Linux's peer-to-peer
 * rule (a path that crosses the host bridge is allowed only when the host bridge is on a known-good list, never by default) is the
 * model; the list here is our own and starts empty, because no client chipset has a measured result yet. A tester can force it.
 *
 * Boot-arg nmhybridp2p: absent or 0 off; 1 allow when the host bridge is on the list; 2 allow regardless of the list (testers).
 * Whatever the answer, the caller falls back to the copy paths whenever the verdict is not "allowed" or a mapping fails.
 * Plain C: tools/test_hybrid_p2p.c.
 */
#ifndef NM_HYBRID_P2P_H
#define NM_HYBRID_P2P_H

#include <stdint.h>

enum { NMP_OFF = 0, NMP_LISTED = 1, NMP_FORCE = 2 };
enum { NMP_OK_LISTED = 0, NMP_OK_FORCED, NMP_NO_SWITCH, NMP_NO_BAR, NMP_NO_IOMMU, NMP_NOT_LISTED, NMP_NO_PATH };

struct nmp_host { uint16_t vendor, device; };
/* Host bridges (PCI 0:0.0) with a measured good result. Empty until a tester reports one; an entry needs the vendor, the device id and the
 * date and machine it was measured on in the commit that adds it. */
static const struct nmp_host nmp_known_good[] = { { 0, 0 } };

struct nmp_topo {
    uint16_t host_vendor, host_device;   /* the host bridge the integrated GPU sits behind */
    uint8_t  dgpu_reachable;             /* the NVIDIA GPU is on the same root complex (always on a laptop) */
    uint8_t  bar1_64bit_prefetch;        /* BAR1 is a 64-bit prefetchable window */
    uint64_t bar1_bytes;                 /* its size */
    uint8_t  igpu_dma_translated;        /* VT-d (or an IOMMU) translates the integrated GPU's DMA: a BAR address would not be mapped */
};

static inline int nmp_parse(int present, long long v) { return !present ? NMP_OFF : (v == 1 ? NMP_LISTED : (v == 2 ? NMP_FORCE : NMP_OFF)); }
static inline int nmp_is_listed(uint16_t vendor, uint16_t device)
{
    unsigned i; for (i = 0; i < sizeof nmp_known_good / sizeof nmp_known_good[0]; i++) if (nmp_known_good[i].vendor && nmp_known_good[i].vendor == vendor && nmp_known_good[i].device == device) return 1;
    return 0;
}
/* The verdict for a frame of `frame_bytes`. */
static inline int nmp_decide(int mode, const struct nmp_topo *t, uint64_t frame_bytes)
{
    if (mode == NMP_OFF) return NMP_NO_SWITCH;
    if (!t->dgpu_reachable) return NMP_NO_PATH;
    if (!t->bar1_64bit_prefetch || t->bar1_bytes < frame_bytes) return NMP_NO_BAR;
    if (t->igpu_dma_translated) return NMP_NO_IOMMU;
    if (nmp_is_listed(t->host_vendor, t->host_device)) return NMP_OK_LISTED;
    return mode == NMP_FORCE ? NMP_OK_FORCED : NMP_NOT_LISTED;
}
/* The page list of a frame that stays in the NVIDIA GPU's BAR1 window: consecutive 4 KiB pages from the window address. 0 when the frame cannot be
 * scanned out that way: not page aligned, longer than the allocation, or a page beyond what a graphics-table entry can address (bits 45:12). */
static inline int nmp_pages(uint64_t phys, uint64_t alloc_bytes, uint64_t len, uint64_t *out, unsigned n)
{
    unsigned i;
    if (!phys || (phys & 4095ull) || len > alloc_bytes || n != (len + 4095ull) / 4096ull) return 0;
    for (i = 0; i < n; i++) { out[i] = phys + 4096ull * i; if (out[i] >> 46) return 0; }
    return 1;
}
static inline int nmp_allowed(int verdict) { return verdict == NMP_OK_LISTED || verdict == NMP_OK_FORCED; }
static inline const char *nmp_name(int v)
{
    switch (v) {
    case NMP_OK_LISTED: return "allowed: the host bridge is on the known-good list";
    case NMP_OK_FORCED: return "allowed: forced by nmhybridp2p=2";
    case NMP_NO_SWITCH: return "off (set nmhybridp2p=1)";
    case NMP_NO_BAR: return "the NVIDIA GPU's BAR1 is not a 64-bit prefetchable window big enough for a frame";
    case NMP_NO_IOMMU: return "the integrated GPU's DMA is translated, so a BAR address is not mapped";
    case NMP_NOT_LISTED: return "the host bridge is not on the known-good list (nmhybridp2p=2 forces it)";
    default: return "the NVIDIA GPU is not on the integrated GPU's root complex";
    }
}

#endif
