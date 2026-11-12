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
 * renderer that draws with the CPU. Two hooks reach the card, both through the framebuffer kext's named platform
 * functions (nvrm_vram_abi.h), so this kext links against nothing of the resource manager:
 *   - a surface's buffer can be video memory (property NMGPUSurfaces, off unless the personality sets it), with
 *     system memory as the fallback whenever the framebuffer refuses or is absent;
 *   - a flush from a privileged client (the window server) copies the surface into the scanout of the
 *     framebuffers named by the mask.
 * Both are written against the framebuffer kext's source and have been compiled and symbol-checked only; neither
 * has run on a card.
 * The personality in Info.plist names a provider class that does not exist, so the kext cannot attach to anything
 * until that line is changed for an approved hardware run.
 */
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/graphics/IOAccelerator.h>
#include <IOKit/graphics/IOAccelSurfaceConnect.h>
#include "nvrm_vram_abi.h"
#include "nm_present.h"

#include "nm_accel_contract.h"
#include "nm_surface_table.h"

extern "C" int proc_selfpid(void);

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
#define NM_RETIRED_SLOTS 64        /* chosen: reshapes in flight at once; when full, new surfaces use system memory */
#define NM_FB_CLASS "NVRMNVDAFramebuffer"

class NMLegacyAccelerator;
class NMLegacySurface : public OSObject
{
    OSDeclareDefaultStructors(NMLegacySurface)
public:
    uint32_t fID, fMode, fWidth, fHeight, fRowBytes, fGeneration, fFlushes, fPresented;
    int32_t fX, fY;                        /* where the shape puts the surface on its framebuffer */
    void *fOwner;                          /* the surface client that created it; only that client reshapes it */
    IOMemoryDescriptor *fBuffer;           /* 32-bit BGRA rows, top row first; NULL until the first shape */
    void *fKva;                            /* the same rows in the kernel's address space */
    NMLegacyAccelerator *fAccel;           /* not retained: every client stops before the accelerator does */
    struct NVRMVramRequest fVram;          /* handle != NULL: fBuffer is video memory the framebuffer kext allocated */
    static NMLegacySurface *withID(uint32_t id, uint32_t mode, void *owner, NMLegacyAccelerator *accel);
    virtual void free();
    /* (Re)allocates the buffer for a new size. The old pages stay alive for whoever still has them mapped. */
    IOReturn reshape(uint32_t width, uint32_t height);
    void dropBuffer();
};
OSDefineMetaClassAndStructors(NMLegacySurface, OSObject)

class NMLegacyAccelerator : public IOAccelerator
{
    OSDeclareDefaultStructors(NMLegacyAccelerator)
    IOLock *fLock;
    nm_surface_table *fTable;                    /* who owns which id, and who was granted what */
    NMLegacySurface *fSurf[NM_TABLE_SLOTS];      /* the surface in each table slot */
    /* Video memory given up by a surface while a client may still have it mapped. It goes back to the framebuffer
     * kext only when ours is the last reference to its descriptor, so no mapping ever outlives the memory. */
    struct Retired { IOMemoryDescriptor *desc; struct NVRMVramRequest req; IOService *fb; } fRetired[NM_RETIRED_SLOTS];
    const OSSymbol *fSymAlloc, *fSymFree, *fSymScanout;
    bool fGPUSurfaces;
    void reapRetired(bool final);                /* accelerator lock held */
public:
    /* The framebuffer service with this dependent index (retained), or NULL. Never waits. */
    IOService *copyFramebuffer(uint32_t index);
    /* Video memory for a surface, zeroed. False means "use system memory": property off, no framebuffer, refused,
     * or the retired list is full. Accelerator lock held. */
    bool allocVram(vm_size_t size, struct NVRMVramRequest *req, IOMemoryDescriptor **desc);
    void retireVram(IOMemoryDescriptor *desc, const struct NVRMVramRequest *req);   /* accelerator lock held */
    /* Copies a surface's rows into the scanout of each framebuffer in the mask. Returns how many it reached. */
    uint32_t present(NMLegacySurface *s, uint32_t mask);
    virtual bool start(IOService *provider);
    virtual void stop(IOService *provider);
    virtual IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler);
    /* The table of every process's surfaces, keyed by (owning process, id). */
    IOReturn addSurface(NMLegacySurface *s, int32_t ownerPid);
    void removeSurface(NMLegacySurface *s);
    /* The surface `callerPid` reaches by `id` under the rules of nm_surface_table.h: retained, with the access it
     * is allowed, or NULL with the reason in *why. */
    NMLegacySurface *findSurface(int32_t callerPid, bool privileged, uint32_t id, int *access, IOReturn *why);
    IOReturn grant(int32_t callerPid, bool privileged, const nm_gl_surface_grant *g);
    void lock() { IOLockLock(fLock); }
    void unlock() { IOLockUnlock(fLock); }
};

NMLegacySurface *NMLegacySurface::withID(uint32_t id, uint32_t mode, void *owner, NMLegacyAccelerator *accel)
{
    NMLegacySurface *s = new NMLegacySurface;
    if (!s || !s->init()) { if (s) s->release(); return NULL; }
    s->fID = id; s->fMode = mode; s->fOwner = owner; s->fAccel = accel;
    s->fWidth = s->fHeight = s->fRowBytes = s->fGeneration = s->fFlushes = s->fPresented = 0;
    s->fX = s->fY = 0; s->fBuffer = NULL; s->fKva = NULL;
    bzero(&s->fVram, sizeof s->fVram);
    return s;
}

/* Accelerator lock held, or the last reference going away. */
void NMLegacySurface::dropBuffer()
{
    if (!fBuffer) return;
    if (fVram.handle && fAccel) fAccel->retireVram(fBuffer, &fVram);   /* takes over our reference */
    else fBuffer->release();
    fBuffer = NULL; fKva = NULL; bzero(&fVram, sizeof fVram);
}

void NMLegacySurface::free()
{
    if (fBuffer && fVram.handle && fAccel) { fAccel->lock(); dropBuffer(); fAccel->unlock(); }
    else dropBuffer();
    OSObject::free();
}

IOReturn NMLegacySurface::reshape(uint32_t width, uint32_t height)
{
    if (width == 0 || height == 0 || width > NM_MAX_SURFACE_DIM || height > NM_MAX_SURFACE_DIM) return kIOReturnBadArgument;
    if (fBuffer && width == fWidth && height == fHeight) return kIOReturnSuccess;
    vm_size_t size = round_page((vm_size_t)width * 4u * (vm_size_t)height);
    /* Video memory first when the accelerator allows it, so the buffer the engine renders for is already on the
     * card; system memory otherwise. Either way the pages are zeroed before anyone can map them. */
    struct NVRMVramRequest req; IOMemoryDescriptor *d = NULL; void *kva = NULL;
    bzero(&req, sizeof req);
    if (fAccel && fAccel->allocVram(size, &req, &d)) kva = req.kva;
    else {
        bzero(&req, sizeof req);
        IOBufferMemoryDescriptor *b = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task,
            kIODirectionInOut | kIOMemoryKernelUserShared, size, page_size);
        if (!b) return kIOReturnNoMemory;
        kva = b->getBytesNoCopy();
        bzero(kva, size);
        d = b;
    }
    dropBuffer();
    fBuffer = d; fKva = kva; fVram = req; fWidth = width; fHeight = height; fRowBytes = width * 4u;
    fGeneration++;
    return kIOReturnSuccess;
}

/* ---- the public surface client --------------------------------------------------------------------------- */

class NMLegacySurfaceClient : public IOUserClient
{
    OSDeclareDefaultStructors(NMLegacySurfaceClient)
    NMLegacyAccelerator *fAccel;
    NMLegacySurface *fSurface;
    IOMemoryMap *fMap;      /* the buffer mapped into the client while it holds a CPU lock */
    task_t fTask;
    int32_t fPid;           /* the opening process: the owner of the surface this connection creates */
    IOLock *fCallLock;      /* one external method at a time per connection; stop() takes it to wait out a call in flight */
    bool fStopped;
    bool fPrivileged;       /* administrator privilege at open: only such a client's flush reaches a display */
    IOReturn dispatch(uint32_t selector, IOExternalMethodArguments *a);
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
    fTask = owningTask; fSurface = NULL; fMap = NULL; fAccel = NULL; fStopped = false;
    fPid = proc_selfpid();               /* initWithTask runs in the opening process's context */
    fPrivileged = IOUserClient::clientHasPrivilege(securityID, kIOClientPrivilegeAdministrator) == kIOReturnSuccess;
    fCallLock = IOLockAlloc();
    return fCallLock != NULL;
}

bool NMLegacySurfaceClient::start(IOService *provider)
{
    fAccel = OSDynamicCast(NMLegacyAccelerator, provider);
    return fAccel && IOUserClient::start(provider);
}

void NMLegacySurfaceClient::stop(IOService *provider)
{
    /* Taking the call lock waits for an external method that is still running on another thread; after the flag
     * is set no later call touches the surface or the mapping. */
    IOLockLock(fCallLock);
    fStopped = true;
    unlockSurface();
    if (fSurface) { if (fAccel) fAccel->removeSurface(fSurface); fSurface->release(); fSurface = NULL; }
    IOLockUnlock(fCallLock);
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
    NMLegacySurface *s = NMLegacySurface::withID(id, mode, this, fAccel);
    if (!s) return kIOReturnNoMemory;
    IOReturn r = fAccel->addSurface(s, fPid);
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
    if (r == kIOReturnSuccess) { fSurface->fX = region->bounds.x; fSurface->fY = region->bounds.y; }
    fAccel->unlock();
    return r;
}

IOReturn NMLegacySurfaceClient::lockSurface(IOExternalMethodArguments *a)
{
    if (!fSurface || !fSurface->fBuffer) return kIOReturnNotReady;
    if (!a->structureOutput || a->structureOutputSize < sizeof(IOAccelSurfaceInformation)) return kIOReturnBadArgument;
    /* The buffer pointer and the geometry are read under the accelerator's lock, with the buffer retained, so a
     * reshape from this or any other path cannot free it between the read and the mapping. */
    fAccel->lock();
    IOMemoryDescriptor *buf = fSurface->fBuffer;
    uint32_t rowBytes = fSurface->fRowBytes, width = fSurface->fWidth, height = fSurface->fHeight;
    if (buf) buf->retain();
    fAccel->unlock();
    if (!buf) return kIOReturnNotReady;
    if (!fMap) fMap = buf->createMappingInTask(fTask, 0, kIOMapAnywhere);
    buf->release();                      /* the mapping keeps the pages alive from here */
    if (!fMap) return kIOReturnVMError;
    IOAccelSurfaceInformation info;
    bzero(&info, sizeof info);
    info.address[0] = fMap->getAddress();
    info.rowBytes = rowBytes;
    info.width = width;
    info.height = height;
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
    IOLockLock(fCallLock);
    IOReturn r = fStopped ? kIOReturnOffline : dispatch(selector, a);
    IOLockUnlock(fCallLock);
    return r;
}

IOReturn NMLegacySurfaceClient::dispatch(uint32_t selector, IOExternalMethodArguments *a)
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
    case kIOAccelSurfaceQueryLock: {
        if (!fSurface) return kIOReturnNotReady;
        fAccel->lock();
        bool ready = fSurface->fBuffer != NULL;
        fAccel->unlock();
        return ready ? kIOReturnSuccess : kIOReturnNotReady;
    }
    case kIOAccelSurfaceSetScale:
        return fSurface ? kIOReturnSuccess : kIOReturnNotReady;   /* accepted; scaling is applied at presentation */
    case kIOAccelSurfaceControl:
        a->scalarOutput[0] = 0;
        a->scalarOutputCount = 1;
        return kIOReturnSuccess;
    case kIOAccelSurfaceFlush:
        if (!fSurface) return kIOReturnNotReady;
        /* Only the window server's surfaces go to a display: it composites everyone else's, and an ordinary
         * process must not be able to paint the screen by naming a position. Every flush is counted. */
        fSurface->fFlushes++;
        if (fPrivileged) fSurface->fPresented += fAccel->present(fSurface, (uint32_t)a->scalarInput[0]);
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
    int32_t fPid;           /* the opening process: what every lookup is judged against */
    bool fPrivileged;       /* administrator privilege at open, as the kernel's user-client check reports it */
    IOLock *fCallLock;
    bool fStopped;
    IOReturn dispatch(uint32_t selector, IOExternalMethodArguments *a);
public:
    virtual void stop(IOService *provider);
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
    fAccel = NULL; fStopped = false;
    if (type != NM_GL_CLIENT_TYPE || !IOUserClient::initWithTask(owningTask, securityID, type)) return false;
    /* Opening is not restricted (Apple's accelerators of these releases do not restrict theirs either: every
     * application's GL needs one). What the connection may reach is: see nm_surface_table.h. */
    fPid = proc_selfpid();
    fPrivileged = IOUserClient::clientHasPrivilege(securityID, kIOClientPrivilegeAdministrator) == kIOReturnSuccess;
    fCallLock = IOLockAlloc();
    return fCallLock != NULL;
}

void NMLegacyGLClient::stop(IOService *provider)
{
    IOLockLock(fCallLock);               /* waits for a call in flight */
    fStopped = true;
    IOLockUnlock(fCallLock);
    IOUserClient::stop(provider);
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
    IOLockLock(fCallLock);
    IOReturn r = kIOReturnOffline;
    int access = NM_ACCESS_NONE;
    NMLegacySurface *s = fStopped ? NULL : fAccel->findSurface(fPid, fPrivileged, type, &access, &r);
    if (s) {
        r = kIOReturnNotReady;
        fAccel->lock();
        if (s->fBuffer) {
            s->fBuffer->retain(); *memory = s->fBuffer;
            /* only the owner draws into its surface; everyone else who may reach it reads */
            *options = (access == NM_ACCESS_WRITE) ? 0 : kIOMapReadOnly;
            r = kIOReturnSuccess;
        }
        fAccel->unlock();
        s->release();
    }
    IOLockUnlock(fCallLock);
    return r;
}

IOReturn NMLegacyGLClient::externalMethod(uint32_t selector, IOExternalMethodArguments *a,
                                          IOExternalMethodDispatch *, OSObject *, void *)
{
    IOLockLock(fCallLock);
    IOReturn r = fStopped ? kIOReturnOffline : dispatch(selector, a);
    IOLockUnlock(fCallLock);
    return r;
}

IOReturn NMLegacyGLClient::dispatch(uint32_t selector, IOExternalMethodArguments *a)
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
        int access = NM_ACCESS_NONE; IOReturn why = kIOReturnNotFound;
        NMLegacySurface *s = fAccel->findSurface(fPid, fPrivileged, q->surface_id, &access, &why);
        if (!s) return why;
        nm_gl_surface_buffer b;
        bzero(&b, sizeof b);
        fAccel->lock();
        IOReturn r = s->fBuffer ? kIOReturnSuccess : kIOReturnNotReady;
        /* Always 0: the pages are reached by mapping type = id, also when they are video memory. Naming the
         * buffer to the Vulkan driver needs a resource-manager handle duplicated into that driver's client, which
         * the framebuffer kext's platform functions do not offer. */
        b.memory_handle = 0;
        b.size = (uint64_t)s->fRowBytes * s->fHeight;
        b.width = s->fWidth; b.height = s->fHeight; b.row_bytes = s->fRowBytes;
        b.format = s->fMode & kIOAccelSurfaceModeColorDepthBits;
        b.generation = s->fGeneration;
        b.access = (uint32_t)access;
        fAccel->unlock();
        s->release();
        if (r != kIOReturnSuccess) return r;
        bcopy(&b, a->structureOutput, sizeof b);
        a->structureOutputSize = sizeof b;
        return kIOReturnSuccess;
    }
    case NM_GL_METHOD_SURFACE_PRESENTED: {
        if (a->structureInputSize != sizeof(nm_gl_surface_query) || !a->structureInput) return kIOReturnBadArgument;
        int access = NM_ACCESS_NONE; IOReturn why = kIOReturnNotFound;
        NMLegacySurface *s = fAccel->findSurface(fPid, fPrivileged, ((const nm_gl_surface_query *)a->structureInput)->surface_id, &access, &why);
        if (!s) return why;
        s->release();                    /* CPU drawing is complete when the call arrives; nothing to wait for */
        return access == NM_ACCESS_WRITE ? kIOReturnSuccess : kIOReturnNotPermitted;   /* only the owner presents */
    }
    case NM_GL_METHOD_SURFACE_GRANT:
        if (a->structureInputSize != sizeof(nm_gl_surface_grant) || !a->structureInput) return kIOReturnBadArgument;
        return fAccel->grant(fPid, fPrivileged, (const nm_gl_surface_grant *)a->structureInput);
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
    fTable = (nm_surface_table *)IOMalloc(sizeof(nm_surface_table));
    if (!fLock || !fTable) return false;
    bzero(fTable, sizeof(nm_surface_table));
    bzero(fSurf, sizeof fSurf);
    bzero(fRetired, sizeof fRetired);
    fSymAlloc = OSSymbol::withCString(NVRM_VRAM_FN_ALLOC);
    fSymFree = OSSymbol::withCString(NVRM_VRAM_FN_FREE);
    fSymScanout = OSSymbol::withCString(NVRM_VRAM_FN_SCANOUT);
    if (!fSymAlloc || !fSymFree || !fSymScanout) return false;
    /* Off unless the personality says otherwise: video-memory surfaces have never run on a card. */
    fGPUSurfaces = getProperty("NMGPUSurfaces") == kOSBooleanTrue;
    /* No IOGLBundleName: Apple's engine must not look for a renderer bundle for this accelerator. The engine at
     * the framework's engine path recognises the accelerator by class instead. */
    registerService();
    return true;
}

void NMLegacyAccelerator::stop(IOService *provider)
{
    if (fLock) { lock(); reapRetired(true); unlock(); }
    if (fSymAlloc) { fSymAlloc->release(); fSymAlloc = NULL; }
    if (fSymFree) { fSymFree->release(); fSymFree = NULL; }
    if (fSymScanout) { fSymScanout->release(); fSymScanout = NULL; }
    if (fTable) { IOFree(fTable, sizeof(nm_surface_table)); fTable = NULL; }
    if (fLock) { IOLockFree(fLock); fLock = NULL; }
    IOAccelerator::stop(provider);
}

/* ---- the two hooks that reach the card ------------------------------------------------------------------- */

IOService *NMLegacyAccelerator::copyFramebuffer(uint32_t index)
{
    OSDictionary *match = IOService::serviceMatching(NM_FB_CLASS);
    if (!match) return NULL;
    OSIterator *it = IOService::getMatchingServices(match);
    match->release();
    if (!it) return NULL;
    IOService *found = NULL, *first = NULL;
    while (OSObject *o = it->getNextObject()) {
        IOService *fb = OSDynamicCast(IOService, o);
        if (!fb) continue;
        OSNumber *n = OSDynamicCast(OSNumber, fb->getProperty("IOFBDependentIndex"));
        if (n && n->unsigned32BitValue() == index) { found = fb; break; }
        if (!first && !n) first = fb;
    }
    /* one head with no index published is head 0 */
    if (!found && index == 0) found = first;
    if (found) found->retain();
    it->release();
    return found;
}

void NMLegacyAccelerator::reapRetired(bool final)
{
    for (int i = 0; i < NM_RETIRED_SLOTS; i++) {
        Retired *r = &fRetired[i];
        if (!r->desc) continue;
        if (r->desc->getRetainCount() > 1) {
            /* still mapped somewhere: the memory is never handed back under a live mapping, even at stop */
            if (final) IOLog("NMLegacyAccel: %llu bytes of video memory left allocated, still mapped at stop\n", r->req.actualSize);
            continue;
        }
        r->desc->release();
        r->fb->callPlatformFunction(fSymFree, false, &r->req, NULL, NULL, NULL);
        r->fb->release();
        bzero(r, sizeof *r);
    }
}

bool NMLegacyAccelerator::allocVram(vm_size_t size, struct NVRMVramRequest *req, IOMemoryDescriptor **desc)
{
    if (!fGPUSurfaces) return false;
    reapRetired(false);
    int freeSlots = 0;
    for (int i = 0; i < NM_RETIRED_SLOTS; i++) if (!fRetired[i].desc) freeSlots++;
    if (freeSlots == 0) return false;   /* nowhere to park this one later: do not start */
    IOService *fb = copyFramebuffer(0);
    if (!fb) return false;
    bzero(req, sizeof *req);
    req->version = NVRM_VRAM_ABI_VERSION;
    req->size = size;
    IOReturn r = fb->callPlatformFunction(fSymAlloc, false, req, NULL, NULL, NULL);
    bool ok = r == kIOReturnSuccess && req->handle && req->phys && req->kva && req->actualSize >= size;
    IOMemoryDescriptor *d = NULL;
    if (ok) {
        d = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)req->phys, (IOByteCount)size, kIODirectionInOut);
        if (d) bzero(req->kva, size);
        else ok = false;
    }
    if (!ok && r == kIOReturnSuccess && req->handle) fb->callPlatformFunction(fSymFree, false, req, NULL, NULL, NULL);
    fb->release();
    if (!ok) return false;
    *desc = d;
    return true;
}

void NMLegacyAccelerator::retireVram(IOMemoryDescriptor *desc, const struct NVRMVramRequest *req)
{
    IOService *fb = copyFramebuffer(0);
    for (int i = 0; fb && i < NM_RETIRED_SLOTS; i++) {
        if (fRetired[i].desc) continue;
        fRetired[i].desc = desc; fRetired[i].req = *req; fRetired[i].fb = fb;
        reapRetired(false);
        return;
    }
    /* No framebuffer to return it to, or no slot (allocVram keeps one free per live buffer only in the common
     * case): the memory stays allocated rather than being freed under a possible mapping. */
    if (fb) fb->release();
    IOLog("NMLegacyAccel: %llu bytes of video memory not returned\n", req->actualSize);
    desc->release();
}

uint32_t NMLegacyAccelerator::present(NMLegacySurface *s, uint32_t mask)
{
    /* Geometry and the buffer are taken under the lock with the descriptor retained; the copy runs outside it.
     * A reshape meanwhile retires the old buffer, which waits for this reference before the memory goes back. */
    lock();
    IOMemoryDescriptor *buf = s->fBuffer;
    const uint8_t *src = (const uint8_t *)s->fKva;
    uint32_t sw = s->fWidth, sh = s->fHeight, srow = s->fRowBytes;
    int32_t sx = s->fX, sy = s->fY;
    if (buf) buf->retain();
    unlock();
    if (!buf || !src) { if (buf) buf->release(); return 0; }
    if (mask == 0) mask = 1;
    uint32_t reached = 0;
    for (uint32_t head = 0; head < 8; head++) {
        if (!(mask & (1u << head))) continue;
        IOService *fb = copyFramebuffer(head);
        if (!fb) continue;
        struct NVRMVramRequest q;
        bzero(&q, sizeof q);
        q.version = NVRM_VRAM_ABI_VERSION;
        IOReturn r = fb->callPlatformFunction(fSymScanout, false, &q, NULL, NULL, NULL);
        fb->release();
        if (r != kIOReturnSuccess || !q.kva || !q.pitch || q.width == 0 || q.height == 0) continue;
        /* every bound is checked inside (nm_present.h, tested on the host) */
        if (nm_present_copy(src, sw, sh, srow, sx, sy, (uint8_t *)q.kva, q.width, q.height, q.pitch, q.actualSize,
                            (q.flags & NVRM_VRAM_SCANOUT_30BIT) != 0) == 0) continue;
        reached++;
    }
    buf->release();
    return reached;
}

static IOReturn nm_table_error(int e)
{
    switch (e) {
    case NM_T_FULL: return kIOReturnNoResources;
    case NM_T_EXISTS: return kIOReturnExclusiveAccess;
    case NM_T_DENIED: return kIOReturnNotPermitted;
    case NM_T_AMBIGUOUS: return kIOReturnNotPermitted;   /* an id two other processes use is never guessed */
    default: return kIOReturnNotFound;
    }
}

IOReturn NMLegacyAccelerator::addSurface(NMLegacySurface *s, int32_t ownerPid)
{
    lock();
    int slot = nm_table_add(fTable, ownerPid, s->fID);
    if (slot >= 0) { s->retain(); fSurf[slot] = s; }
    unlock();
    return slot >= 0 ? kIOReturnSuccess : nm_table_error(slot);
}

void NMLegacyAccelerator::removeSurface(NMLegacySurface *s)
{
    lock();
    for (int i = 0; i < NM_TABLE_SLOTS; i++)
        if (fSurf[i] == s) { nm_table_remove(fTable, i); fSurf[i] = NULL; s->release(); break; }
    unlock();
}

NMLegacySurface *NMLegacyAccelerator::findSurface(int32_t callerPid, bool privileged, uint32_t id, int *access, IOReturn *why)
{
    NMLegacySurface *found = NULL;
    lock();
    int slot = nm_table_lookup(fTable, callerPid, privileged ? 1 : 0, id, access);
    if (slot >= 0 && fSurf[slot]) { found = fSurf[slot]; found->retain(); }
    else if (why) *why = nm_table_error(slot);
    unlock();
    return found;
}

IOReturn NMLegacyAccelerator::grant(int32_t callerPid, bool privileged, const nm_gl_surface_grant *g)
{
    lock();
    int e = nm_table_grant(fTable, callerPid, privileged ? 1 : 0, g->owner_pid ? g->owner_pid : callerPid, g->surface_id, g->grantee_pid, g->revoke != 0);
    unlock();
    return e == NM_T_OK ? kIOReturnSuccess : nm_table_error(e);
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
