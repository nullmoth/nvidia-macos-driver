/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include "iofam_types.h"
#include <libkern/c++/OSObject.h>
enum { kIOAccelEventMachine2Size = 0xf0 };
class IOAccelEventMachine2 : public OSObject {
    OSDeclareDefaultStructors(IOAccelEventMachine2);
public:
    virtual bool init(IOGraphicsAccelerator2 *, unsigned int, int);
    virtual void *getClientTestEventFunc() = 0;
    virtual int getNumStamps();
    virtual void setStampBaseAddress(unsigned int volatile *);
    virtual unsigned int getStampOffset(int);
    virtual void initEvent(IOAccelEvent *) = 0;
    virtual void cleanEvent(IOAccelEvent *) = 0;
    virtual void finishStamp(int) = 0;
    virtual void finishAllStamps() = 0;
    virtual bool testStamp(int) = 0;
    virtual bool testAllStamps() = 0;
    virtual bool testAllStampsUnlocked() = 0;
    virtual void finishEventUnlocked(IOAccelEvent *) = 0;
    virtual bool testEventUnlocked(IOAccelEvent *) = 0;
    virtual void finishEvent(IOAccelEvent *) = 0;
    virtual bool testEvent(IOAccelEvent *) = 0;
    virtual void finishEventExcluding(IOAccelEvent *, int) = 0;
    virtual bool testEventExcluding(IOAccelEvent *, int) = 0;
    virtual void moveEvent(IOAccelEvent *, IOAccelEvent *) = 0;
    virtual void copyEvent(IOAccelEvent *, IOAccelEvent *) = 0;
    virtual void mergeEvent(IOAccelEvent *, IOAccelEvent *) = 0;
    virtual void copyEventExcluding(IOAccelEvent *, IOAccelEvent *, int) = 0;
    virtual void mergeEventExcluding(IOAccelEvent *, IOAccelEvent *, int) = 0;
    virtual void setEventStamp(int, IOAccelEvent *) = 0;
    virtual unsigned int incrementStamp(int) = 0;
    virtual void writeStampCommand(int, IOAccelEventQueue *, vendevtCommandRec *) = 0;
    virtual void writeEventStampCommand(int, IOAccelEventQueue *, IOAccelEvent *, vendevtCommandRec *) = 0;
    virtual void writeEventBarrierCommand(IOAccelEventQueue *, IOAccelEvent *, vendevtBarrierRec *, int) = 0;
    virtual void logCommandSubmission(int) = 0;
    virtual bool stampIsIncremented() = 0;
    virtual unsigned int getStamp(int) = 0;
    virtual bool checkGPUProgress() = 0;
    virtual bool checkChannelProgress(int) = 0;
    virtual void *eventTimeout(int) = 0;
    virtual void signalStamp(int, unsigned int);
    virtual void collectAllLiveEventIfNeeded();
    virtual int waitForStamp(int, unsigned int, unsigned int *);
    virtual void enableStampInterrupt(int);
    virtual void disableStampInterrupt(int);
    virtual void deviceTerminatedUnlocked();
    virtual void enableEventStampInterrupts(IOAccelEvent const *) = 0;
    virtual void disableEventStampInterrupts(IOAccelEvent const *) = 0;
    virtual void stop();
#if defined(NM_ERA) && NM_ERA < 1100   // macOS 10.15: still the family's reserved pad
    virtual void _RESERVEDIOAccelEventMachine0();
#else
    virtual void scrubEvent(IOAccelEvent *) = 0;
#endif
    virtual void triage(char **, unsigned long long *) = 0;
    virtual void _RESERVEDIOAccelEventMachine2();
    virtual void _RESERVEDIOAccelEventMachine3();
    virtual void _RESERVEDIOAccelEventMachine4();
    virtual void _RESERVEDIOAccelEventMachine5();
private:
    uint8_t _opaque[kIOAccelEventMachine2Size - sizeof(OSObject)];
};
static_assert(sizeof(IOAccelEventMachine2) == kIOAccelEventMachine2Size, "IOAccelEventMachine2 must be 240 bytes");
