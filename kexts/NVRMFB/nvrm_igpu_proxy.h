// nvrm_igpu_proxy.h: when does the NVIDIA framebuffer stand in for a panel that the integrated GPU lit?
//
// BIOS Hybrid laptops wire the internal panel to the integrated GPU. The NVIDIA GPU has no display connected, so
// NVKMS reports zero displays and the framebuffer would refuse to start, and then the accelerator never gets its
// device. With boot-arg nvfbigpu=1 the framebuffer instead starts as a proxy head: it describes the firmware console
// (the integrated GPU's stolen-memory framebuffer) as the only mode, publishes that memory as a CPU-only scanout and
// refuses NVKMS flips, so the accelerator renders on the NVIDIA GPU and copies each frame into the panel's memory.
//
// This file holds the decision only (pure C, no IOKit), so a host test can prove it. It is conservative on purpose:
// a console that sits inside one of the NVIDIA GPU's own BARs is that GPU's console and is not a proxy target, and a
// machine without an Intel display-class PCI device has no integrated GPU to stand in for.

#ifndef NVRM_IGPU_PROXY_H
#define NVRM_IGPU_PROXY_H

#include <stdint.h>

#define NVRM_PROXY_MAX_BARS 8

struct nvrm_proxy_console {
    uint64_t base;          // physical address of the firmware console
    uint32_t width, height; // visible size in pixels
    uint32_t pitch;         // bytes per row
    uint32_t depth;         // bits per pixel
};

struct nvrm_proxy_range { uint64_t base, size; };

enum nvrm_proxy_verdict {
    NVRM_PROXY_OK = 0,
    NVRM_PROXY_OFF,             // boot-arg not set
    NVRM_PROXY_NO_CONSOLE,      // no usable console description
    NVRM_PROXY_BAD_FORMAT,      // not 32 bit per pixel, or pitch smaller than a row
    NVRM_PROXY_TOO_LARGE,       // implausible size (corrupt console record)
    NVRM_PROXY_NO_IGPU,         // no Intel display-class device on the bus
    NVRM_PROXY_OWN_CONSOLE      // the console lives in the NVIDIA GPU's own BAR
};

static inline enum nvrm_proxy_verdict nvrm_proxy_decide(int bootArg, const struct nvrm_proxy_console *c,
                                                        int intelDisplayPresent,
                                                        const struct nvrm_proxy_range *nvBars, unsigned nBars)
{
    if (!bootArg) return NVRM_PROXY_OFF;
    if (!c || !c->base || !c->width || !c->height) return NVRM_PROXY_NO_CONSOLE;
    if (c->depth != 32 || c->pitch < c->width * 4u) return NVRM_PROXY_BAD_FORMAT;
    // 16384 x 8192 x 4 bytes is more than any panel firmware lights; anything beyond is a corrupt record
    if (c->width > 16384u || c->height > 8192u || (uint64_t)c->pitch * c->height > (512ull << 20)) return NVRM_PROXY_TOO_LARGE;
    if (!intelDisplayPresent) return NVRM_PROXY_NO_IGPU;
    const uint64_t end = c->base + (uint64_t)c->pitch * c->height;
    for (unsigned i = 0; i < nBars && i < NVRM_PROXY_MAX_BARS; i++) {
        const uint64_t b = nvBars[i].base, e = nvBars[i].base + nvBars[i].size;
        if (nvBars[i].size && c->base < e && end > b) return NVRM_PROXY_OWN_CONSOLE;
    }
    return NVRM_PROXY_OK;
}

#endif
