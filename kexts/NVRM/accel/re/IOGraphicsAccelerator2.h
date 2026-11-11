/*
 * IOGraphicsAccelerator2 — reconstructed from IOAcceleratorFamily2
 * (macOS Sequoia, x86_64). Apple ships no header.
 *
 * THIS IS THE CLASS OUR DRIVER MUST *BE*. Everything else in the stack hangs
 * off an instance of this: macOS finds a graphics accelerator by finding an
 * IOGraphicsAccelerator2 attached to a PCI GPU.
 *
 * IT IS AN ABSTRACT CLASS. 17 of its vtable slots are `___cxa_pure_virtual`,
 * and those 17 methods are the ENTIRE required vendor interface — see
 * re/DRIVER-CONTRACT.md. The other 67 new virtuals have working defaults we
 * inherit for free and override only where NVIDIA differs from Apple's model.
 *
 * WHERE EACH FACT CAME FROM
 *   size        3544 bytes — the KERNEL'S OWN OSMetaClass registry (read by the
 *               inspector in bench/AccelDevice), cross-checked against the
 *               `movl $0xdd8, %ecx` immediate in the metaclass constructor.
 *               Two independent sources agreeing.
 *   superclass  IOAccelerator (full chain: IOAccelerator -> IOService ->
 *               IORegistryEntry -> OSObject), from the same registry.
 *   vtable      __ZTV22IOGraphicsAccelerator2 — 84 new virtual slots, 268..351.
 *
 * THE 84 NEW VIRTUALS BELOW ARE MACHINE-GENERATED, IN VTABLE ORDER.
 * Regenerate, never hand-edit:
 *
 *   python3 re/gen_virtuals.py re/x86/IOAcceleratorFamily2 IOGraphicsAccelerator2 268 \
 *     --names re/x86/AMDRadeonX4000:AMDRadeonX4000_AMDGraphicsAccelerator \
 *     --names re/x86/AMDRadeonX4000:AMDRadeonX4000_AMDEllesmereGraphicsAccelerator
 *
 * RETURN TYPES ARE INFERRED, NOT RECOVERED. The Itanium ABI does not encode
 * return types for non-template functions, so they are guessed from naming
 * convention and marked. This is INERT for vtable layout, and on x86_64 every
 * method here returns a pointer or an integer — same ABI register. It would
 * only matter for a struct returned by value or a float return, neither of
 * which appears here. Confirm the return type of any method you IMPLEMENT; the
 * 67 we inherit are unaffected.
 *
 * THE INHERITED OVERRIDES ARE DELIBERATELY NOT DECLARED. IOGraphicsAccelerator2
 * overrides 14 IOService/OSObject methods (free, start, stop, terminate,
 * finalize, message, newUserClient, getWorkLoop, setProperties, requestProbe,
 * requestTerminate, didTerminate, configureReport, updateReport). Re-declaring
 * them here would be pure risk: they occupy slots that already exist, so
 * declaring them cannot help, while getting ONE signature subtly wrong would
 * create a NEW virtual slot and shift all 84 below it. A subclass can still
 * override any of them directly — they are inherited normally.
 */

#ifndef _IOGRAPHICSACCELERATOR2_H
#define _IOGRAPHICSACCELERATOR2_H

#include "IOAccelerator.h"

/* Referenced across the interface; layouts not needed to subclass. */
class IOAccelAllocationInfo;
class IOAccelClientSharedRO;
class IOAccelClientSharedRW;
class IOAccelCommandDescriptor;
class IOAccelConfig;
class IOAccelEvent;
class IOAccelMemory;
class IOAccelMemoryMap;
class IOAccelResource2;
class IOAccelShared2;
class IOAccelSurface;
class IOAccelSysMemory;
class IOAccelTask;
class IOAccelVidMemory;
class IOFramebuffer;
class IOMemoryDescriptor;
class IOPCIDevice;
class IOSurface;

/* From the kernel's registry, cross-checked against the metaclass constructor. */
#if defined(NM_ERA) && NM_ERA < 1100   /* macOS 10.15 registers 3552 bytes for this class */
enum { kIOGraphicsAccelerator2Size = 0xde0 };
#else
enum { kIOGraphicsAccelerator2Size = 0xdd8 };   /* 3544 */
#endif

class IOGraphicsAccelerator2 : public IOAccelerator
{
    OSDeclareDefaultStructors(IOGraphicsAccelerator2);

public:
    // NOT PART OF THE GENERATED LIST BELOW. newUserClient is an IOService virtual, so its
    //   vtable slot already exists: declaring it here adds no slot and moves nothing. It is here
    //   because the family DOES override it -- IOAcceleratorFamily2 exports
    //   IOGraphicsAccelerator2::newUserClient(task*, void*, unsigned int, IOUserClient**) at 0x3e34a
    //   -- and this header did not say so. Without it, a subclass override calling
    //   super::newUserClient would resolve all the way up to IOService's default and SILENTLY BYPASS
    //   the family's own user clients: every IOAccelerationUserClient, IOAccelSharedUserClient2 and
    //   IOAccelDisplayPipeUserClient2 the compositor opens to drive us at all.
    virtual IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type,
                                   IOUserClient **handler) APPLE_KEXT_OVERRIDE;

    /* ---- IOGraphicsAccelerator2's own virtuals, slots 268..351 (84 of them) ----
     * ORDER IS LOAD-BEARING AND MACHINE-GENERATED. Do not reorder, do not
     * insert, do not delete. Regenerate with re/gen_virtuals.py.
     * Return types are INFERRED — see the note in that script. */
    virtual void acceleratorDidLock(char const*, int);   /* slot 268 */
    virtual void acceleratorWillUnlock(char const*, int);   /* slot 269 */
    virtual uint64_t tmpTotalVRAM();   /* slot 270 */
    virtual uint64_t totalTextureMemory();   /* slot 271 */
    virtual bool isIdle();   /* slot 272 */
    virtual uint64_t getStampMemory(unsigned int*) = 0;   /* slot 273 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * createEventMachine();   /* slot 274 — factory — returns a pointer */
    virtual OSObject * newEventMachine() = 0;   /* slot 275 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newClientSharedRO();   /* slot 276 — factory — returns a pointer */
    virtual OSObject * newClientSharedRW();   /* slot 277 — factory — returns a pointer */
    virtual void deleteClientSharedRO(IOAccelClientSharedRO*);   /* slot 278 */
    virtual void deleteClientSharedRW(IOAccelClientSharedRW*);   /* slot 279 */
    virtual OSObject * createShared(task*);   /* slot 280 — factory — returns a pointer */
    virtual OSObject * createSysMemory();   /* slot 281 — factory — returns a pointer */
    virtual OSObject * createVidMemory(IOAccelShared2*, IOAccelResource2*, unsigned long long, void*);   /* slot 282 — factory — returns a pointer */
    virtual OSObject * createMemoryMap(IOAccelTask*, IOAccelMemory*, unsigned int);   /* slot 283 — factory — returns a pointer */
    virtual OSObject * createResource(IOAccelShared2*, unsigned int);   /* slot 284 — factory — returns a pointer */
    virtual OSObject * createCommandDescriptor();   /* slot 285 — factory — returns a pointer */
    virtual void deleteCommandDescriptor(IOAccelCommandDescriptor*);   /* slot 286 */
    virtual OSObject * newBlockFence();   /* slot 287 — factory — returns a pointer */
    virtual void scrubEvents();   /* slot 288 */
    virtual OSObject * createDrawable(IOAccelSurface*, IOAccelShared2*, bool);   /* slot 289 — factory — returns a pointer */
    virtual void resourceTypeForIOSurface(IOSurface*);   /* slot 290 */
    virtual OSObject * createDisplayPipe(IOFramebuffer*, unsigned int);   /* slot 291 — factory — returns a pointer */
    virtual OSObject * createMemoryDescriptorWithAddressRange(unsigned long long, unsigned long long, unsigned int, task*);   /* slot 292 — factory — returns a pointer */
    virtual OSObject * createMemoryDescriptorWithPersistentMemoryDescriptor(IOMemoryDescriptor*);   /* slot 293 — factory — returns a pointer */
    virtual OSObject * createBufferMemoryDescriptorWithOptions(unsigned int, unsigned long, unsigned long);   /* slot 294 — factory — returns a pointer */
    virtual OSObject * createBufferMemoryDescriptorInTaskWithOptions(task*, unsigned int, unsigned long, unsigned long);   /* slot 295 — factory — returns a pointer */
    virtual void freeAllGPUMappings();   /* slot 296 */
    virtual void freeWaitToPrepareVidMemory(IOAccelVidMemory*, bool, bool);   /* slot 297 */
    virtual void freeWaitToPrepareVidMap(IOAccelMemoryMap*, bool, bool);   /* slot 298 */
    virtual void unwireAllVidMemory();   /* slot 299 */
    virtual void freeAllVidMemoryMappings();   /* slot 300 */
    virtual void pageoffSurfaceInLinear();   /* slot 301 */
    virtual void freeWaitToPrepareSysMemory(IOAccelSysMemory*, bool);   /* slot 302 */
    virtual void freeWaitToPrepareSysMap(IOAccelMemoryMap*, bool);   /* slot 303 */
    virtual void collectGartWirings();   /* slot 304 */
    virtual void unwireAllSysMemory();   /* slot 305 */
    virtual void freeAllSysMemoryMappings();   /* slot 306 */
    virtual uint64_t getMaxSingleSysMemorySize();   /* slot 307 */
    virtual uint64_t getMaxWiredSysMemorySize();   /* slot 308 */
    virtual OSObject * createUserGPUTask() = 0;   /* slot 309 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual void acceleratorFinalize();   /* slot 310 */
    virtual void submitSwapCopy(IOAccelEvent*, IOAccelResource2*, IOAccelResource2*);   /* slot 311 */
    /* NON-VIRTUAL (a direct callq in the family, so it takes NO vtable slot and adding it here shifts
       nothing). Measured 2026-09-15: `IOAccelSharedUserClient2::new_resource` calls
       `IOGraphicsAccelerator2::acceleratorWaitEnabled()`, which SLEEPS until bit 0x2 of the flags dword at
       ivar 0xc78 is set — and the only thing that sets it is `enableAccelerator()`, which the family calls
       from `IOAccel[Legacy]DisplayMachine::display_mode_did_change(uint32_t)`. Its whole body is: bail if
       ivar 0xc92 & 8, start the event machine's hardware-progress timer, OR in 0x2. Every allocation on our
       accelerator hung here, in the kernel, for as long as this bit stayed clear. */
    void enableAccelerator();
    void disableAccelerator();
    virtual void populateAccelConfig(IOAccelConfig*) = 0;   /* slot 312 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual uint64_t calcMaxGPUPhysicalMemoryBytes(unsigned long long);   /* slot 313 */
    virtual bool configureDevice(IOPCIDevice*) = 0;   /* slot 314 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual void teardownDevice(IOPCIDevice*) = 0;   /* slot 315 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * createKernelGPUTask() = 0;   /* slot 316 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual void systemWillSleep();   /* slot 317 */
    virtual void systemDidWake();   /* slot 318 */
    virtual void systemWillChangeSpeed();   /* slot 319 */
    virtual void systemDidChangeSpeed();   /* slot 320 */
    virtual bool configIsHeadless();   /* slot 321 */
    virtual OSObject * newDisplayMachine() = 0;   /* slot 322 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newShared();   /* slot 323 — factory — returns a pointer */
    virtual OSObject * newSharedUserClient();   /* slot 324 — factory — returns a pointer */
    virtual OSObject * newDevice();   /* slot 325 — factory — returns a pointer */
    virtual OSObject * newGLContext() = 0;   /* slot 326 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newCLContext() = 0;   /* slot 327 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newSurface() = 0;   /* slot 328 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * new2DContext() = 0;   /* slot 329 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newVideoContext() = 0;   /* slot 330 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newDisplayPipe();   /* slot 331 — factory — returns a pointer */
    virtual OSObject * newDrawable();   /* slot 332 — factory — returns a pointer */
    virtual OSObject * newContext(unsigned int);   /* slot 333 — factory — returns a pointer */
    virtual OSObject * newSysMemory() = 0;   /* slot 334 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newVidMemory() = 0;   /* slot 335 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newResource() = 0;   /* slot 336 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual OSObject * newCommandDescriptor();   /* slot 337 — factory — returns a pointer */
    virtual OSObject * newStatistics();   /* slot 338 — factory — returns a pointer */
    virtual OSObject * newMemoryMap() = 0;   /* slot 339 — PURE VIRTUAL, WE MUST IMPLEMENT */
    virtual uint64_t getMaxResourceSize();   /* slot 340 */
    virtual OSObject * newRemoteMemory();   /* slot 341 — factory — returns a pointer */
    virtual OSObject * createRemoteMemory(IOAccelShared2*, unsigned long long, unsigned int, unsigned int);   /* slot 342 — factory — returns a pointer */
    virtual OSObject * newContextEventFence();   /* slot 343 — factory — returns a pointer */
#if defined(NM_ERA) && NM_ERA < 1100   /* macOS 10.15: still the family's reserved pad */
    virtual void _RESERVEDIOGraphicsAccelerator1();
#else
    virtual OSObject * createIODMACommand();   /* slot 344 — factory — returns a pointer */
#endif
    virtual void triage(char**, unsigned long long*);   /* slot 345 */
    virtual void _RESERVEDIOGraphicsAccelerator3();   /* slot 346 — reserved padding */
    virtual void _RESERVEDIOGraphicsAccelerator4();   /* slot 347 — reserved padding */
    virtual void _RESERVEDIOGraphicsAccelerator5();   /* slot 348 — reserved padding */
    virtual void describeDriverAllocations(IOAccelAllocationInfo*);   /* slot 349 */
    virtual OSObject * newCommandQueue();   /* slot 350 — factory — returns a pointer */
    virtual void ktraceStateChanged();   /* slot 351 */

private:
    /* Layout not recovered; extent fixed by the registry size. */
    uint8_t _opaque[kIOGraphicsAccelerator2Size - sizeof(IOAccelerator)];
};

static_assert(sizeof(IOGraphicsAccelerator2) == kIOGraphicsAccelerator2Size,
              "IOGraphicsAccelerator2 must be exactly 0xdd8 (3544) bytes — our "
              "driver subclasses it and its members start at that offset");

#endif /* _IOGRAPHICSACCELERATOR2_H */
