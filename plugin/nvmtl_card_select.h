/* nvmtl_card_select.h - which NVIDIA card a process's Metal device is.
   The plugin runs one Vulkan device per process, so a process drives one card. With one card, or without nvmultigpu=1, nothing
   here applies and the first accelerator is the device, exactly as before. With nvmultigpu=1 the kernel marks one accelerator
   primary (NVAccelPrimary) and every NVIDIA accelerator is still a Metal device candidate; the process takes the one this
   rule accepts and refuses the others, so MTLCreateSystemDefaultDevice stays the primary card and an application the user
   assigned to another card gets that card.

   The assignment is NVMTL_CARD in the user's block of nvrm610.conf: one number for every application ("1"), or a list
   ("Game=1,*=0"). 0 is the primary card, 1 the second accelerator in registry order, and so on, up to 7.
   Pure over its inputs, so tools/test_card_select.c proves the rules without a GPU. */
#ifndef NVMTL_CARD_SELECT_H
#define NVMTL_CARD_SELECT_H
#include <stdio.h>
#include <string.h>

/* The card number for one executable from a setting string, or -1 for "no assignment" (use the primary). A malformed entry is
   skipped, so a typo costs that entry only. */
static inline int nvmtl_card_for(const char *spec, const char *exe)
{
    if (!spec || !*spec || !exe) return -1;
    if (!strchr(spec, '=')) return (spec[0] >= '0' && spec[0] <= '7' && !spec[1]) ? spec[0] - '0' : -1;
    int dflt = -1; size_t elen = strlen(exe); const char *p = spec;
    while (*p) {
        const char *end = strchr(p, ','); size_t len = end ? (size_t)(end - p) : strlen(p);
        const char *eq = memchr(p, '=', len);
        if (eq) {
            size_t klen = (size_t)(eq - p); const char *v = eq + 1; size_t vlen = len - klen - 1;
            int n = (vlen == 1 && v[0] >= '0' && v[0] <= '7') ? v[0] - '0' : -1;
            if (n >= 0) {
                if (klen == elen && !memcmp(p, exe, elen)) return n;
                if (klen == 1 && p[0] == '*') dflt = n;
            }
        }
        if (!end) break;
        p = end + 1;
    }
    return dflt;
}

/* May this accelerator be the process's device?
     multi          nvmultigpu=1 is on (the plugin reads it from the boot line or the conf)
     wanted         the card number from nvmtl_card_for, or -1
     primary_prop   NVAccelPrimary on this accelerator: 1 yes, 0 no, -1 not published
     ordinal        this accelerator's position among the NVAccel services (0 first) */
static inline int nvmtl_card_accept(int multi, int wanted, int primary_prop, int ordinal)
{
    if (!multi) return 1;                                  /* today's behaviour: any NVIDIA accelerator */
    if (wanted >= 0) return ordinal == wanted;
    if (primary_prop >= 0) return primary_prop == 1;
    return ordinal == 0;                                   /* kernel did not say: the first one, as before */
}

/* Several vendors (an NVIDIA card with an AMD or Intel GPU that NMXGPU drives). The ordinal counts every Metal accelerator of
   either class, in registry-id order, so NVMTL_CARD indexes every card in the machine in one order.
   Position of `mine` among ids[0..n) when sorted ascending; 0 if it is not in the list (as the NVIDIA-only code did). */
static inline int nvmtl_card_ordinal(const unsigned long long *ids, unsigned n, unsigned long long mine)
{
    int below = 0, found = 0;
    for (unsigned i = 0; i < n; i++) { if (ids[i] < mine) below++; else if (ids[i] == mine) found = 1; }
    return found ? below : 0;
}

/* NVAccelPrimary is an NVIDIA property. An NMXAccel publishes none, so with no assignment it is the process's device only when
   the machine has no NVIDIA accelerator: an AMD or Intel GPU never takes the primary away from an NVIDIA card. nvidia_count is
   the number of NVAccel services; the result feeds nvmtl_card_accept as primary_prop. */
static inline int nvmtl_card_nmx_primary(int nvidia_count)
{
    return nvidia_count > 0 ? 0 : -1;
}

/* Naming one of several GPUs of the same model. Mesa's device-select layer takes "vendor:device" (the last matching GPU wins,
   so two identical cards cannot be told apart) or, in DRI_PRIME, a PCI location tag "pci-DDDD_BB_SS_F". The accelerator's PCI
   parent publishes its location as "pcidebug" ("bus:device:function", decimal). */
static inline int nvmtl_pci_loc_parse(const char *s, unsigned *bus, unsigned *dev, unsigned *fn)
{
    unsigned v[3] = {0, 0, 0}; int k = 0;
    if (!s || !*s) return 0;
    for (const char *p = s; ; p++) {
        if (*p >= '0' && *p <= '9') { v[k] = v[k] * 10 + (unsigned)(*p - '0'); if (v[k] > 255) return 0; }
        else if (*p == ':' && k < 2 && p != s && p[-1] != ':') k++;
        else if (*p == 0 && k == 2 && p[-1] != ':') break;
        else return 0;
    }
    if (v[1] > 31 || v[2] > 7) return 0;
    *bus = v[0]; *dev = v[1]; *fn = v[2];
    return 1;
}

static inline void nvmtl_pci_tag(char *out, size_t n, unsigned bus, unsigned dev, unsigned fn)
{
    snprintf(out, n, "pci-0000_%02x_%02x_%1x", bus, dev, fn);
}

/* What names this GPU to RADV/ANV. same_model is how many AMD/Intel GPUs in the machine share this one's vendor and device id.
   Returns 1 when the PCI tag must be used (DRI_PRIME), 0 when "vendor:device" is enough and keeps working as before. */
static inline int nvmtl_gpu_use_pci_tag(int same_model, int have_loc)
{
    return same_model > 1 && have_loc;
}
#endif
