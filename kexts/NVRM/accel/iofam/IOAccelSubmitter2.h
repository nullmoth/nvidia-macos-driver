/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include <IOKit/IOUserClient.h>
#include "iofam_types.h"
enum { kIOAccelSubmitter2Size = 0x568 };
class IOAccelSubmitter2 : public IOUserClient {
    OSDeclareDefaultStructors(IOAccelSubmitter2);
public:
    virtual void * getOwningTask() const = 0;
    virtual void * getGPUTask() const = 0;
    virtual void getOwningTaskPid() const = 0;
    virtual void affectedByHardwareReset(bool, unsigned int) = 0;
    virtual void retireCommandBuffer(IOAccelEventFence*);
    virtual bool shouldHardwareCommandNooped() = 0;
    virtual void orphanClientMappings(OSSet*);
    virtual void setProtectionOptions(unsigned long long);
    virtual void setSubmissionError(unsigned int);
    virtual void _RESERVEDIOAccelSubmitter0();
    virtual void _RESERVEDIOAccelSubmitter1();
    virtual void _RESERVEDIOAccelSubmitter2();
    virtual void _RESERVEDIOAccelSubmitter3();
    virtual void _RESERVEDIOAccelSubmitter4();
    virtual void _RESERVEDIOAccelSubmitter5();
#if !defined(NM_ERA) || NM_ERA >= 1100   // macOS 10.15 has no such slot: its table ends one entry earlier
    virtual bool isOpportunisticWorkload() const;
#endif
private:
    uint8_t _opaque[kIOAccelSubmitter2Size - sizeof(IOUserClient)];
};
static_assert(sizeof(IOAccelSubmitter2) == kIOAccelSubmitter2Size, "IOAccelSubmitter2 must be 0x568 bytes");
