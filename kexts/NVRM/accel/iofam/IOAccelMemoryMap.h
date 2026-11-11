/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include "iofam_types.h"
#include <libkern/c++/OSObject.h>
#if defined(NM_ERA) && NM_ERA < 1100   // macOS 10.15 registers 264 bytes for this class (read from its metaclass)
enum { kIOAccelMemoryMapSize = 0x108 };
#else
enum { kIOAccelMemoryMapSize = 0x118 };
#endif
class IOAccelMemoryMap : public OSObject {
    OSDeclareDefaultStructors(IOAccelMemoryMap);
public:
    virtual bool init(IOGraphicsAccelerator2 *, IOAccelTask *, IOAccelMemory *, unsigned int);
    virtual bool matchOptionBits(unsigned int);
    virtual unsigned long long getGPUVirtualAddress();
    virtual unsigned long long getGPUVirtualAddressForUserProcess();
    virtual int prepare();
    virtual int complete();
    virtual bool compatibleWith(IOAccelMemoryMap *);
    virtual bool allocGPUVirtualAddress();
#if !defined(NM_ERA) || NM_ERA >= 1100   // macOS 10.15 has no such slot
    virtual bool reserveGPUVirtualAddress(unsigned long long, unsigned long long);
#endif
    virtual void freeGPUVirtualAddress();
    virtual unsigned long long getLength() const;
    virtual bool commitIntoGPUPageTable() = 0;
    virtual void releaseFromGPUPageTable() = 0;
    virtual bool updateGPUPageTable() = 0;
    virtual unsigned long long getGPUVirtualAddressLengthForUserProcess();
#if defined(NM_ERA) && NM_ERA < 1300   // macOS 11 and 12: this slot is still the family's reserved pad (read from their binaries)
    virtual void _RESERVEDIOAccelMemoryMap0();
#else
    virtual unsigned long long getAssignedGPUVirtualAddressLength() const;
#endif
    virtual void _RESERVEDIOAccelMemoryMap1();
    virtual void _RESERVEDIOAccelMemoryMap2();
    virtual void _RESERVEDIOAccelMemoryMap3();
    virtual void _RESERVEDIOAccelMemoryMap4();
    virtual void _RESERVEDIOAccelMemoryMap5();
    virtual void redirect(IOAccelMemory *);
private:
    uint8_t _opaque[kIOAccelMemoryMapSize - sizeof(OSObject)];
};
static_assert(sizeof(IOAccelMemoryMap) == kIOAccelMemoryMapSize, "IOAccelMemoryMap must be the size its release registers (280 bytes; 264 on 10.15)");
