/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include <IOKit/IOService.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/IODeviceMemory.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSData.h>
#include <pexpert/pexpert.h>
#include "nvrm_gpu_select_policy.h"

namespace NvPrimaryGpu {
inline bool displayDevice(IOPCIDevice *pci)
{
    if (!pci || pci->configRead16(0) != 0x10de || pci->configRead8(0x0b) != 3) return false;
    unsigned sub = pci->configRead8(0x0a);
    return sub == 0 || sub == 2;
}

inline bool ownsConsole(IOPCIDevice *pci, uint64_t address, uint64_t bytes)
{
    // Read only: never size a BAR by writing all ones and never initialize RM.
    UInt32 low = pci->configRead32(0x14);
    if (low == 0xffffffffU || (low & 1)) return false;
    unsigned type = (low >> 1) & 3;
    if (type != 0 && type != 2) return false;
    uint64_t base = low & ~0xfULL;
    if (type == 2) base |= static_cast<uint64_t>(pci->configRead32(0x18)) << 32;
    if (!base) return false;
    IODeviceMemory *bar = pci->getDeviceMemoryWithRegister(0x14);
    if (bar && bar->getPhysicalAddress() == base && contains(base, bar->getLength(), address, bytes)) return true;
    // IOPCIFamily's aperture list can be stale. This project's PCI property
    // contains native UInt32 cells; require the BAR tag AND live base to agree.
    if (OSData *data = OSDynamicCast(OSData, pci->getProperty("assigned-addresses"))) {
        const UInt8 *cells = static_cast<const UInt8 *>(data->getBytesNoCopy());
        if (data->getLength() % 20) return false;
        for (unsigned off = 0; cells && off + 20 <= data->getLength(); off += 20) {
            UInt32 c[5]; memcpy(c, cells + off, sizeof c);
            unsigned space = (c[0] >> 24) & 3;
            uint64_t a = (static_cast<uint64_t>(c[1]) << 32) | c[2];
            uint64_t n = (static_cast<uint64_t>(c[3]) << 32) | c[4];
            if ((c[0] & 0xff) == 0x14 && (space == 2 || space == 3) && a == base &&
                contains(a, n, address, bytes)) return true;
        }
    }
    return false;
}


}
