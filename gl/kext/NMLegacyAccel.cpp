/*
 * NMLegacyAccel.cpp: stub of the accelerator for the releases that composite through OpenGL (10.6 to 10.12).
 *
 * Shape of the real thing, taken from Apple's own accelerators on 10.7 (which link the graphics family and no
 * accelerator family): one IOAccelerator subclass in match category IOAccelerator, a user client of type 0 that
 * implements the eighteen public surface methods, and private clients for the vendor's own GL code.
 *
 * This file implements both cross-process pieces of the contract with surfaces backed by ordinary kernel memory:
 * the surface table, shape and reshape, CPU locks that map the buffer into the caller, and the private client
 * through which the engine finds and maps the buffer behind any surface id. That is a complete accelerator for a
 * renderer that draws with the CPU. What it does not do is touch the GPU or a display: binding a surface's buffer
 * to GPU memory and showing a surface on the framebuffer are the two marked hooks (NEEDS HARDWARE), because they
 * go through the resource-manager and framebuffer kexts and cannot be exercised without the card.
 * The personality in Info.plist names a provider class that does not exist, so the kext cannot attach to anything
 * until that line is changed for an approved hardware run.
 */
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <libkern/c++/OSArray.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/graphics/IOAccelerator.h>
#include <IOKit/graphics/IOAccelSurfaceConnect.h>

#include "nm_accel_contract.h"

/* The table in the contract header was written from the user library's source; these tie it to the kernel's own
 * headers of the release being built, so a difference between the two stops the build. */
static_assert(kIOAccelSurfaceClientType == NM_SURFACE_CLIENT_TYPE, "public surface client type");
static_assert(kIOAccelNumClientTypes == NM_GL_CLIENT_TYPE, "our private client is the first type after the public ones");
static_assert(kIOAccelNumSurfaceMethods == NM_SURFACE_METHOD_COUNT, "eighteen public surface methods");
static_assert(kIOAccelSurfaceReadLockOptions == 0 && kIOAccelSurfaceSetIDMode == 7 && kIOAccelSurfaceSetShape == 9 &&
              kIOAccelSurfaceFlush == 10 && kIOAccelSurfaceControl == 16 && kIOAccelSurfaceSetShapeBackingAndLength == 17,
              "method numbers used by the window server");
static_assert(sizeof(IOAccelSurfaceInformation) == NM_SIZEOF_SURFACE_INFORMATION, "IOAccelSurfaceInformation");
static_assert(sizeof(IOAccelSurfaceReadData) == NM_SIZEOF_SURFACE_READ_DATA, "IOAccelSurfaceReadData");
static_assert(sizeof(IOAccelSurfaceScaling) == NM_SIZEOF_SURFACE_SCALING, "IOAccelSurfaceScaling");
static_assert(sizeof(IOAccelDeviceRegion) == NM_SIZEOF_DEVICE_REGION_HEAD, "IOAccelDeviceRegion header");

static const nm_surface_method_shape gSurfaceShape[NM_SURFACE_METHOD_COUNT] = { NM_SURFACE_METHOD_SHAPES };

/* True when the caller's argument counts and sizes are the ones the user library sends for this method. */
static bool nm_shape_ok(const nm_surface_method_shape &s, const IOExternalMethodArguments *a)
{
    if (a->scalarInputCount != s.scalar_in || a->scalarOutputCount < s.scalar_out) return false;
    if (s.struct_in == NM_VARIABLE) {
        /* a device region: the fixed header, then exactly num_rects rectangles */
        if (!a->structureInput || a->structureInputSize < NM_SIZEOF_DEVICE_REGION_HEAD) return false;
        const IOAccelDeviceRegion *r = (const IOAccelDeviceRegion *)a->structureInput;
        uint64_t need = NM_SIZEOF_DEVICE_REGION_HEAD + (uint64_t)r->num_rects * sizeof(IOAccelBounds);
        if (need != a->structureInputSize) return false;
    } else if (a->structureInputSize != s.struct_in) {
        return false;
    }
    /* output structures: the caller may pass a larger buffer, never a smaller one */
    if (s.struct_out && a->structureOutputSize < s.struct_out) return false;
    return true;
}

/* ---- surfaces --------------------------------------------------------------------------------------------- */

#define NM_MAX_SURFACE_DIM 16384   /* larger than any display or window these releases create */

class NMLegacySurface : public OSObject
{
    OSDeclareDefaultStructors(NMLegacySurface)
public:
    uint32_t fID, fMode, fWidth, fHeight, fRowBytes, fGeneration, fFlushes;
    void *fOwner;                          /* the surface client that created it; only that client reshapes it */
    IOBufferMemoryDescriptor *fBuffer;     /* 32-bit BGRA rows, top row first; NULL until the first shape */
    static NMLegacySurface *withID(uint32_t id, uint32_t mode, void *owner);
    virtual void free();
    /* (Re)allocates the buffer for a new size. The old pages stay alive for whoever still has them mapped. */
    IOReturn reshape(uint32_t width, uint32_t height);
};
OSDefineMetaClassAndStructors(NMLegacySurface, OSObject)

NMLegacySurface *NMLegacySurface::withID(uint32_t id, uint32_t mode, void *owner)
{
    NMLegacySurface *s = new NMLegacySurface;
    if (!s || !s->init()) { if (s) s->release(); return NULL; }
    s->fID = id; s->fMode = mode; s->fOwner = owner;
    s->fWidth = s->fHeight = s->fRowBytes = s->fGeneration = s->fFlushes = 0; s->fBuffer = NULL;
    return s;
}

void NMLegacySurface::free()
{
    if (fBuffer) { fBuffer->release(); fBuffer = NULL; }
    OSObject::free();
}

IOReturn NMLegacySurface::reshape(uint32_t width, uint32_t height)
{
    if (width == 0 || height == 0 || width > NM_MAX_SURFACE_DIM || height > NM_MAX_SURFACE_DIM) return kIOReturnBadArgument;
    if (fBuffer && width == fWidth && height == fHeight) return kIOReturnSuccess;
    vm_size_t size = round_page((vm_size_t)width * 4u * (vm_size_t)height);
    /* NEEDS HARDWARE: with the card present this is where the buffer becomes GPU memory allocated through the
     * resource manager, so the Vulkan driver can render into it without a copy. */
    IOBufferMemoryDescriptor *b = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task,
        kIODirectionInOut | kIOMemoryKernelUserShared, size, page_size);
    if (!b) return kIOReturnNoMemory;
    bzero(b->getBytesNoCopy(), size);
    if (fBuffer) fBuffer->release();
    fBuffer = b; fWidth = width; fHeight = height; fRowBytes = width * 4u;
    fGeneration++;
    return kIOReturnSuccess;
}

class NMLegacyAccelerator : public IOAccelerator
{
    OSDeclareDefaultStructors(NMLegacyAccelerator)
    IOLock *fLock;
    OSArray *fSurfaces;
public:
    virtual bool start(IOService *provider);
    virtual void stop(IOService *provider);
    virtual IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler);
    /* The table of every process's surfaces. find returns a retained surface or NULL. */
    IOReturn addSurface(NMLegacySurface *s);
    void removeSurface(NMLegacySurface *s);
    NMLegacySurface *findSurface(uint32_t id);
    void lock() { IOLockLock(fLock); }
    void unlock() { IOLockUnlock(fLock); }
};

/* ---- the public surface client --------------------------------------------------------------------------- */

class NMLegacySurfaceClient : public IOUserClient
{
    OSDeclareDefaultStructors(NMLegacySurfaceClient)
    NMLegacyAccelerator *fAccel;
    NMLegacySurface *fSurface;
    IOMemoryMap *fMap;      /* the buffer mapped into the client while it holds a CPU lock */
    task_t fTask;
    IOReturn setIDMode(uint32_t id, uint32_t mode);
    IOReturn setShape(const IOAccelDeviceRegion *region);
    IOReturn lockSurface(IOExternalMethodArguments *a);
    void unlockSurface();
public:
    virtual bool initWithTask(task_t owningTask, void *securityID, UInt32 type);
    virtual bool start(IOService *provider);
    virtual void stop(IOService *provider);
    virtual IOReturn clientClose(void);
    virtual IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *arguments,
                                    IOExternalMethodDispatch *dispatch, OSObject *target, void *reference);
};
OSDefineMetaClassAndStructors(NMLegacySurfaceClient, IOUserClient)

bool NMLegacySurfaceClient::initWithTask(task_t owningTask, void *securityID, UInt32 type)
{
    if (type != NM_SURFACE_CLIENT_TYPE || !IOUserClient::initWithTask(owningTask, securityID, type)) return false;
    fTask = owningTask; fSurface = NULL; fMap = NULL; fAccel = NULL;
    return true;
}

bool NMLegacySurfaceClient::start(IOService *provider)
{
    fAccel = OSDynamicCast(NMLegacyAccelerator, provider);
    return fAccel && IOUserClient::start(provider);
}

void NMLegacySurfaceClient::stop(IOService *provider)
{
    unlockSurface();
    if (fSurface) { if (fAccel) fAccel->removeSurface(fSurface); fSurface->release(); fSurface = NULL; }
    IOUserClient::stop(provider);
}

IOReturn NMLegacySurfaceClient::clientClose(void)
{
    terminate();
    return kIOReturnSuccess;
}

IOReturn NMLegacySurfaceClient::setIDMode(uint32_t id, uint32_t mode)
{
    if (fSurface) {                      /* the same connection may only change its own surface's mode */
        if (fSurface->fID != id) return kIOReturnExclusiveAccess;
        fSurface->fMode = mode;
        return kIOReturnSuccess;
    }
    NMLegacySurface *s = NMLegacySurface::withID(id, mode, this);
    if (!s) return kIOReturnNoMemory;
    IOReturn r = fAccel->addSurface(s);
    if (r != kIOReturnSuccess) { s->release(); return r; }
    fSurface = s;
    return kIOReturnSuccess;
}

IOReturn NMLegacySurfaceClient::setShape(const IOAccelDeviceRegion *region)
{
    if (!fSurface) return kIOReturnNotReady;
    if (region->bounds.w <= 0 || region->bounds.h <= 0) return kIOReturnBadArgument;
    unlockSurface();                     /* a mapping of the old buffer must not outlive the reshape */
    fAccel->lock();
    IOReturn r = fSurface->reshape((uint32_t)region->bounds.w, (uint32_t)region->bounds.h);
    fAccel->unlock();
    return r;
}

IOReturn NMLegacySurfaceClient::lockSurface(IOExternalMethodArguments *a)
{
    if (!fSurface || !fSurface->fBuffer) return kIOReturnNotReady;
    if (!a->structureOutput || a->structureOutputSize < sizeof(IOAccelSurfaceInformation)) return kIOReturnBadArgument;
    if (!fMap) {
        fMap = fSurface->fBuffer->createMappingInTask(fTask, 0, kIOMapAnywhere);
        if (!fMap) return kIOReturnVMError;
    }
    IOAccelSurfaceInformation info;
    bzero(&info, sizeof info);
    info.address[0] = fMap->getAddress();
    info.rowBytes = fSurface->fRowBytes;
    info.width = fSurface->fWidth;
    info.height = fSurface->fHeight;
    info.pixelFormat = 0x42475241;       /* 'BGRA' */
    bcopy(&info, a->structureOutput, sizeof info);
    a->structureOutputSize = sizeof info;
    return kIOReturnSuccess;
}

void NMLegacySurfaceClient::unlockSurface()
{
    if (fMap) { fMap->release(); fMap = NULL; }
}

IOReturn NMLegacySurfaceClient::externalMethod(uint32_t selector, IOExternalMethodArguments *a,
                                               IOExternalMethodDispatch *, OSObject *, void *)
{
    if (selector >= NM_SURFACE_METHOD_COUNT) return kIOReturnBadArgument;
    if (!nm_shape_ok(gSurfaceShape[selector], a)) return kIOReturnBadArgument;
    switch (selector) {
    case kIOAccelSurfaceSetIDMode:
        return setIDMode((uint32_t)a->scalarInput[0], (uint32_t)a->scalarInput[1]);
    case kIOAccelSurfaceSetShape:
    case kIOAccelSurfaceSetShapeBacking:
    case kIOAccelSurfaceSetShapeBackingAndLength:
        /* A client backing store passed with the shape is not adopted: the surface keeps its own buffer, which
         * is what both the CPU locks and the engine see. */
        return setShape((const IOAccelDeviceRegion *)a->structureInput);
    case kIOAccelSurfaceReadLock: case kIOAccelSurfaceReadLockOptions:
    case kIOAccelSurfaceWriteLock: case kIOAccelSurfaceWriteLockOptions:
        return lockSurface(a);
    case kIOAccelSurfaceReadUnlock: case kIOAccelSurfaceReadUnlockOptions:
    case kIOAccelSurfaceWriteUnlock: case kIOAccelSurfaceWriteUnlockOptions:
        unlockSurface();
        return kIOReturnSuccess;
    case kIOAccelSurfaceGetState:
        a->scalarOutput[0] = kIOAccelSurfaceStateIdleBit;   /* CPU-drawn surfaces never have GPU work pending */
        a->scalarOutputCount = 1;
        return kIOReturnSuccess;
    case kIOAccelSurfaceQueryLock:
        return fSurface && fSurface->fBuffer ? kIOReturnSuccess : kIOReturnNotReady;
    case kIOAccelSurfaceSetScale:
        return fSurface ? kIOReturnSuccess : kIOReturnNotReady;   /* accepted; scaling is applied at presentation */
    case kIOAccelSurfaceControl:
        a->scalarOutput[0] = 0;
        a->scalarOutputCount = 1;
        return kIOReturnSuccess;
    case kIOAccelSurfaceFlush:
        if (!fSurface) return kIOReturnNotReady;
        /* NEEDS HARDWARE: showing the surface on the framebuffers named by the mask (flip or copy into the
         * scanout the framebuffer kext drives). Counted so a caller's flushes can be seen in the registry. */
        fSurface->fFlushes++;
        return kIOReturnSuccess;
    case kIOAccelSurfaceRead:
    default:
        return kIOReturnUnsupported;
    }
}

/* ---- our private client: the link between a context and a surface ------------------------------------------ */

class NMLegacyGLClient : public IOUserClient
{
    OSDeclareDefaultStructors(NMLegacyGLClient)
    NMLegacyAccelerator *fAccel;
public:
    virtual bool initWithTask(task_t owningTask, void *securityID, UInt32 type);
    virtual bool start(IOService *provider);
    virtual IOReturn clientClose(void);
    virtual IOReturn clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory);
    virtual IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *arguments,
                                    IOExternalMethodDispatch *dispatch, OSObject *target, void *reference);
};
OSDefineMetaClassAndStructors(NMLegacyGLClient, IOUserClient)

bool NMLegacyGLClient::initWithTask(task_t owningTask, void *securityID, UInt32 type)
{
    fAccel = NULL;
    return type == NM_GL_CLIENT_TYPE && IOUserClient::initWithTask(owningTask, securityID, type);
}

bool NMLegacyGLClient::start(IOService *provider)
{
    fAccel = OSDynamicCast(NMLegacyAccelerator, provider);
    return fAccel && IOUserClient::start(provider);
}

IOReturn NMLegacyGLClient::clientClose(void)
{
    terminate();
    return kIOReturnSuccess;
}

/* Mapping type = surface id: the engine maps the pages behind a surface, whichever process created it. This is
 * the CPU form of "the buffer behind a surface id"; with GPU-backed surfaces the handle in the reply replaces it. */
IOReturn NMLegacyGLClient::clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory)
{
    NMLegacySurface *s = fAccel->findSurface(type);
    if (!s) return kIOReturnNotFound;
    IOReturn r = kIOReturnNotReady;
    fAccel->lock();
    if (s->fBuffer) { s->fBuffer->retain(); *memory = s->fBuffer; *options = 0; r = kIOReturnSuccess; }
    fAccel->unlock();
    s->release();
    return r;
}

IOReturn NMLegacyGLClient::externalMethod(uint32_t selector, IOExternalMethodArguments *a,
                                          IOExternalMethodDispatch *, OSObject *, void *)
{
    switch (selector) {
    case NM_GL_METHOD_GET_VERSION: {
        if (a->structureOutputSize < sizeof(nm_gl_version) || !a->structureOutput) return kIOReturnBadArgument;
        nm_gl_version v = { NM_GL_CONTRACT_VERSION, 0 };
        bcopy(&v, a->structureOutput, sizeof v);
        a->structureOutputSize = sizeof v;
        return kIOReturnSuccess;
    }
    case NM_GL_METHOD_SURFACE_BUFFER: {
        if (a->structureInputSize != sizeof(nm_gl_surface_query) || !a->structureInput ||
            a->structureOutputSize < sizeof(nm_gl_surface_buffer) || !a->structureOutput) return kIOReturnBadArgument;
        const nm_gl_surface_query *q = (const nm_gl_surface_query *)a->structureInput;
        NMLegacySurface *s = fAccel->findSurface(q->surface_id);
        if (!s) return kIOReturnNotFound;
        nm_gl_surface_buffer b;
        bzero(&b, sizeof b);
        fAccel->lock();
        IOReturn r = s->fBuffer ? kIOReturnSuccess : kIOReturnNotReady;
        b.memory_handle = 0;             /* no GPU memory object: the pages are reached by mapping type = id */
        b.size = (uint64_t)s->fRowBytes * s->fHeight;
        b.width = s->fWidth; b.height = s->fHeight; b.row_bytes = s->fRowBytes;
        b.format = s->fMode & kIOAccelSurfaceModeColorDepthBits;
        b.generation = s->fGeneration;
        fAccel->unlock();
        s->release();
        if (r != kIOReturnSuccess) return r;
        bcopy(&b, a->structureOutput, sizeof b);
        a->structureOutputSize = sizeof b;
        return kIOReturnSuccess;
    }
    case NM_GL_METHOD_SURFACE_PRESENTED: {
        if (a->structureInputSize != sizeof(nm_gl_surface_query) || !a->structureInput) return kIOReturnBadArgument;
        NMLegacySurface *s = fAccel->findSurface(((const nm_gl_surface_query *)a->structureInput)->surface_id);
        if (!s) return kIOReturnNotFound;
        s->release();                    /* CPU drawing is complete when the call arrives; nothing to wait for */
        return kIOReturnSuccess;
    }
    default:
        return kIOReturnBadArgument;
    }
}

/* ---- the accelerator ------------------------------------------------------------------------------------- */

OSDefineMetaClassAndStructors(NMLegacyAccelerator, IOAccelerator)

bool NMLegacyAccelerator::start(IOService *provider)
{
    if (!IOAccelerator::start(provider)) return false;
    fLock = IOLockAlloc();
    fSurfaces = OSArray::withCapacity(16);
    if (!fLock || !fSurfaces) return false;
    /* No IOGLBundleName: Apple's engine must not look for a renderer bundle for this accelerator. The engine at
     * the framework's engine path recognises the accelerator by class instead. */
    registerService();
    return true;
}

void NMLegacyAccelerator::stop(IOService *provider)
{
    if (fSurfaces) { fSurfaces->release(); fSurfaces = NULL; }
    if (fLock) { IOLockFree(fLock); fLock = NULL; }
    IOAccelerator::stop(provider);
}

IOReturn NMLegacyAccelerator::addSurface(NMLegacySurface *s)
{
    IOReturn r = kIOReturnSuccess;
    lock();
    for (unsigned i = 0; i < fSurfaces->getCount(); i++)
        if (((NMLegacySurface *)fSurfaces->getObject(i))->fID == s->fID) { r = kIOReturnExclusiveAccess; break; }
    if (r == kIOReturnSuccess && !fSurfaces->setObject(s)) r = kIOReturnNoMemory;
    unlock();
    return r;
}

void NMLegacyAccelerator::removeSurface(NMLegacySurface *s)
{
    lock();
    for (unsigned i = 0; i < fSurfaces->getCount(); i++)
        if (fSurfaces->getObject(i) == s) { fSurfaces->removeObject(i); break; }
    unlock();
}

NMLegacySurface *NMLegacyAccelerator::findSurface(uint32_t id)
{
    NMLegacySurface *found = NULL;
    lock();
    for (unsigned i = 0; i < fSurfaces->getCount() && !found; i++) {
        NMLegacySurface *s = (NMLegacySurface *)fSurfaces->getObject(i);
        if (s->fID == id) { s->retain(); found = s; }
    }
    unlock();
    return found;
}

IOReturn NMLegacyAccelerator::newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler)
{
    IOUserClient *c = NULL;
    if (type == NM_SURFACE_CLIENT_TYPE) c = new NMLegacySurfaceClient;
    else if (type == NM_GL_CLIENT_TYPE) c = new NMLegacyGLClient;
    else return kIOReturnBadArgument;
    if (!c) return kIOReturnNoMemory;
    if (!c->initWithTask(owningTask, securityID, type) || !c->attach(this)) { c->release(); return kIOReturnError; }
    if (!c->start(this)) { c->detach(this); c->release(); return kIOReturnError; }
    *handler = c;
    return kIOReturnSuccess;
}
