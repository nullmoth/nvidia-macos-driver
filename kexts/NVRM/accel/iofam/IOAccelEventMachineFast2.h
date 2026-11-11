/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include "IOAccelEventMachine2.h"
enum { kIOAccelEventMachineFast2Size = 0xd30 };
class IOAccelEventMachineFast2 : public IOAccelEventMachine2 {
    OSDeclareDefaultStructors(IOAccelEventMachineFast2);
public:
    virtual bool init(IOGraphicsAccelerator2 *, unsigned int, int) APPLE_KEXT_OVERRIDE;
    virtual void *getClientTestEventFunc() APPLE_KEXT_OVERRIDE;
    virtual void initEvent(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void cleanEvent(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void finishStamp(int) APPLE_KEXT_OVERRIDE;
    virtual void finishAllStamps() APPLE_KEXT_OVERRIDE;
    virtual bool testStamp(int) APPLE_KEXT_OVERRIDE;
    virtual bool testAllStamps() APPLE_KEXT_OVERRIDE;
    virtual bool testAllStampsUnlocked() APPLE_KEXT_OVERRIDE;
    virtual void finishEventUnlocked(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual bool testEventUnlocked(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void finishEvent(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual bool testEvent(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void finishEventExcluding(IOAccelEvent *, int) APPLE_KEXT_OVERRIDE;
    virtual bool testEventExcluding(IOAccelEvent *, int) APPLE_KEXT_OVERRIDE;
    virtual void moveEvent(IOAccelEvent *, IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void copyEvent(IOAccelEvent *, IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void mergeEvent(IOAccelEvent *, IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual void copyEventExcluding(IOAccelEvent *, IOAccelEvent *, int) APPLE_KEXT_OVERRIDE;
    virtual void mergeEventExcluding(IOAccelEvent *, IOAccelEvent *, int) APPLE_KEXT_OVERRIDE;
    virtual void setEventStamp(int, IOAccelEvent *) APPLE_KEXT_OVERRIDE;
    virtual unsigned int incrementStamp(int) APPLE_KEXT_OVERRIDE;
    virtual void writeStampCommand(int, IOAccelEventQueue *, vendevtCommandRec *) APPLE_KEXT_OVERRIDE;
    virtual void writeEventStampCommand(int, IOAccelEventQueue *, IOAccelEvent *, vendevtCommandRec *) APPLE_KEXT_OVERRIDE;
    virtual void writeEventBarrierCommand(IOAccelEventQueue *, IOAccelEvent *, vendevtBarrierRec *, int) APPLE_KEXT_OVERRIDE;
    virtual void logCommandSubmission(int) APPLE_KEXT_OVERRIDE;
    virtual bool stampIsIncremented() APPLE_KEXT_OVERRIDE;
    virtual unsigned int getStamp(int) APPLE_KEXT_OVERRIDE;
    virtual bool checkGPUProgress() APPLE_KEXT_OVERRIDE;
    virtual bool checkChannelProgress(int) APPLE_KEXT_OVERRIDE;
    virtual void *eventTimeout(int) APPLE_KEXT_OVERRIDE;
    virtual void deviceTerminatedUnlocked() APPLE_KEXT_OVERRIDE;
    virtual void enableEventStampInterrupts(IOAccelEvent const *) APPLE_KEXT_OVERRIDE;
    virtual void disableEventStampInterrupts(IOAccelEvent const *) APPLE_KEXT_OVERRIDE;
#if !defined(NM_ERA) || NM_ERA >= 1100
    virtual void scrubEvent(IOAccelEvent *) APPLE_KEXT_OVERRIDE;
#endif
    virtual void triage(char **, unsigned long long *) APPLE_KEXT_OVERRIDE;
    virtual void writeStamp(int, vendevtCommandRec *, unsigned int) = 0;
    virtual void prepareBarrier(vendevtBarrierRec *) = 0;
    virtual void completeBarrier(vendevtBarrierRec *) = 0;
    virtual void writeBarrierElement(vendevtBarrierRec *, int, unsigned int) = 0;
    virtual int waitForAnyStamp(IOAccelEvent *, int *);
    virtual void _RESERVEDIOAccelEventMachineFast0();
    virtual void _RESERVEDIOAccelEventMachineFast1();
    virtual void _RESERVEDIOAccelEventMachineFast2();
    virtual void _RESERVEDIOAccelEventMachineFast3();
    virtual void _RESERVEDIOAccelEventMachineFast4();
    virtual void _RESERVEDIOAccelEventMachineFast5();
private:
    uint8_t _opaque2[kIOAccelEventMachineFast2Size - kIOAccelEventMachine2Size];
};
static_assert(sizeof(IOAccelEventMachineFast2) == kIOAccelEventMachineFast2Size, "IOAccelEventMachineFast2 must be 3376 bytes");
