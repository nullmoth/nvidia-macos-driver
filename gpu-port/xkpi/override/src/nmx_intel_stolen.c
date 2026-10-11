/*
 * Intel graphics stolen memory for i915 (the RAM the firmware reserves for the integrated GPU). FreeBSD finds it in
 * its early PCI quirks (sys/x86/pci/pci_early_quirks.c, BSD-2-Clause, Copyright (c) 2018 Johannes Lundberg); this is
 * the Gen9-and-later part of that logic, read from the IGD at 00:02.0 before i915 initialises.
 *   base: Gen11+ keeps a 64-bit BSM at 0xC0/0xC4; earlier parts a 32-bit BSM at 0x5C (1 MiB aligned).
 *   size: GGC (0x50) GMS field: 0x00-0xEF in 32 MiB steps, 0xF0-0xFE in 4 MiB steps from 4 MiB.
 */
#include <linux/kernel.h>
#include <linux/ioport.h>
#include <sys/kernel.h>
#include "nmx_xnu.h"

#define MiB(v) ((uint64_t)(v) << 20)
#define INTEL_BSM 0x5c
#define INTEL_GEN11_BSM_DW0 0xc0
#define INTEL_GEN11_BSM_DW1 0xc4
#define INTEL_BSM_MASK 0xfff00000u
#define SNB_GMCH_CTRL 0x50
#define BDW_GMCH_GMS_SHIFT 8
#define BDW_GMCH_GMS_MASK 0xff

vm_paddr_t intel_graphics_stolen_base;
vm_paddr_t intel_graphics_stolen_size;
/* intel_graphics_stolen_res itself is i915's (i915_module.c), filled from these two at i915_init */

static void nmx_intel_stolen_init(void *arg)
{
    void *igd = nmx_xnu_pci_find(0, 0, 2, 0);
    if (igd == NULL || nmx_xnu_pci_read(igd, 0x00, 2) != 0x8086)
        return;
    uint64_t base = nmx_xnu_pci_read(igd, INTEL_GEN11_BSM_DW0, 4) & INTEL_BSM_MASK;
    if (base != 0)
        base |= (uint64_t)nmx_xnu_pci_read(igd, INTEL_GEN11_BSM_DW1, 4) << 32;
    else
        base = nmx_xnu_pci_read(igd, INTEL_BSM, 4) & INTEL_BSM_MASK;
    uint32_t gms = (nmx_xnu_pci_read(igd, SNB_GMCH_CTRL, 2) >> BDW_GMCH_GMS_SHIFT) & BDW_GMCH_GMS_MASK;
    uint64_t size = gms < 0xF0 ? gms * MiB(32) : (gms - 0xF0) * MiB(4) + MiB(4);
    intel_graphics_stolen_base = base;
    intel_graphics_stolen_size = size;
    printf("NullMoth Intel: stolen memory 0x%llx, %llu MiB\n", (unsigned long long)base,
           (unsigned long long)(size >> 20));
}
/* before every driver's module init (SI_SUB_DRIVERS), as FreeBSD's early quirks run before i915 loads */
SYSINIT(nmx_intel_stolen, SI_SUB_KLD, SI_ORDER_ANY, nmx_intel_stolen_init, NULL);
