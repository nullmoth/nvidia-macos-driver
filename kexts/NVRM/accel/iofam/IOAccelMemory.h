/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include <libkern/c++/OSObject.h>
#include "iofam_types.h"
enum { kIOAccelMemorySize = 0xb0 };
class IOAccelMemory : public OSObject {
    OSDeclareDefaultStructors(IOAccelMemory);
public:
    virtual bool init(IOGraphicsAccelerator2*);
    virtual uint64_t getLength() const;
    virtual uint64_t getDirtySize() const = 0;
    virtual uint64_t getResidentSize() const = 0;
    virtual OSObject * createMappingInTask(IOAccelTask*, unsigned int);
#if !defined(NM_ERA) || NM_ERA >= 1100   // macOS 10.15 has no such slot
    virtual OSObject * createMappingInTaskAtAddressLength(IOAccelTask*, unsigned int, unsigned long long, unsigned long long);
#endif
    virtual bool prepare();
    virtual bool complete();
    virtual uint64_t getPhysicalSegment(unsigned long long, unsigned long long*) = 0;
    virtual void setPurgeable(unsigned int, unsigned int*) = 0;
    virtual uint64_t setTag(unsigned int) = 0;
    virtual uint64_t getTag() = 0;
    virtual bool matchForReuse(void*, unsigned long long) = 0;
    virtual void _RESERVEDIOAccelMemory0();
    virtual void _RESERVEDIOAccelMemory1();
    virtual void _RESERVEDIOAccelMemory2();
    virtual void _RESERVEDIOAccelMemory3();
    virtual void _RESERVEDIOAccelMemory4();
    virtual void _RESERVEDIOAccelMemory5();
    virtual void wire() = 0;
    virtual void unwire() = 0;
    virtual void orphanIt();
    virtual void unorphanIt(IOAccelShared2*);
    virtual void increment_wire_count() = 0;
    virtual void decrement_wire_count() = 0;
private:
    uint8_t _opaque[kIOAccelMemorySize - sizeof(OSObject)];
};
static_assert(sizeof(IOAccelMemory) == kIOAccelMemorySize, "IOAccelMemory must be 0xb0 bytes");
