/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include "IOAccelSubmitter2.h"
#include "iofam_types.h"
enum { kIOAccelContext2Size = 0x10a8 };
class IOAccelContext2 : public IOAccelSubmitter2 {
    OSDeclareDefaultStructors(IOAccelContext2);
public:
    virtual void * getOwningTask() const APPLE_KEXT_OVERRIDE;
    virtual void * getGPUTask() const APPLE_KEXT_OVERRIDE;
    virtual int getOwningTaskPid() const APPLE_KEXT_OVERRIDE;
    virtual void affectedByHardwareReset(bool, unsigned int) APPLE_KEXT_OVERRIDE;
    virtual bool shouldHardwareCommandNooped() APPLE_KEXT_OVERRIDE;
    virtual void contextStart();
    virtual void contextStop();
    virtual void getDataBufferLimits();
    virtual void context_finish();
    virtual bool initSidebandBufferHeader(IOAccelSidebandBufferHeader*, unsigned long long, unsigned long long);
    virtual void processSidebandBuffer(IOAccelCommandDescriptor*, bool);
    virtual void processSidebandToken(IOAccelCommandStreamInfo&);
    virtual void discardSidebandToken(IOAccelCommandStreamInfo&);
    virtual void postTokenSanityCheck(IOAccelCommandStreamInfo&);
    virtual void processDataBuffers(unsigned int);
    virtual void beginCommandStream(IOAccelCommandStreamInfo&);
    virtual void endCommandStream(IOAccelCommandStreamInfo&);
    virtual void bindResource(IOAccelCommandStreamInfo&, IOAccelResource2*, bool, IOAccelChannel2*, unsigned int, bool);
    virtual void unbindResource(IOAccelCommandStreamInfo&, IOAccelResource2*, IOAccelChannel2*);
    virtual void compactCurrentVidMemory();
    virtual bool prepareResources(IOAccelResource2**, int);
    virtual void compactCurrentMappings(IOAccelMemoryMap*);
    virtual void getDataBuffer(IOAccelContextGetDataBufferIn*, IOAccelContextGetDataBufferOut*, IOAccelResourcePrivate*, unsigned long long);
    virtual bool allocOneDataBuffer(bool, unsigned int);
    virtual void getDataBufferPrivate(IOAccelResource2*, IOAccelResourcePrivate*, unsigned long long);
    virtual void validateDataBuffer(unsigned long long, IOBufferMemoryDescriptor*, IOAccelResource2*);
    virtual void populateContextConfig(IOAccelContextConfig*);
    virtual void addDataBufferToChannel(IOAccelResource2*, unsigned int);
    virtual void removeDataBufferFromChannel(IOAccelResource2*, unsigned int);
    virtual void removeCurrentResourceFromChannel(IOAccelResource2*, unsigned int);
    virtual void invalidate();
    virtual void getFrameDelimiter();
    virtual bool canSubmitCommandBuffer();
    virtual void pauseSubmitCommandBuffer();
    virtual void getPriorityInfo();
    virtual void _RESERVEDIOAccelContext0();
    virtual void _RESERVEDIOAccelContext1();
    virtual void _RESERVEDIOAccelContext2();
    virtual void _RESERVEDIOAccelContext3();
    virtual void _RESERVEDIOAccelContext4();
    virtual void _RESERVEDIOAccelContext5();
    virtual void getSurfaceReqBits() const;
    virtual void requiresBackingStore() const;
    virtual void set_compatible_surface_mode(unsigned long long*, unsigned long long, unsigned int);
    virtual void removeSurface();
    virtual void allowsExclusiveMode() const;
    virtual bool isTripleBuffered() const;
    virtual void contextModeBits() const;
    virtual void describeDriverAllocations(IOAccelAllocationInfo*);
private:
    uint8_t _opaque[kIOAccelContext2Size - sizeof(IOAccelSubmitter2)];
};
static_assert(sizeof(IOAccelContext2) == kIOAccelContext2Size, "IOAccelContext2 must be 0x10a8 bytes");
