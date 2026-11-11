/*
 * NMLegacyAccel.cpp: stub of the accelerator for the releases that composite through OpenGL (10.6 to 10.12).
 *
 * Shape of the real thing, taken from Apple's own accelerators on 10.7 (which link the graphics family and no
 * accelerator family): one IOAccelerator subclass in match category IOAccelerator, a user client of type 0 that
 * implements the eighteen public surface methods, and private clients for the vendor's own GL code.
 *
 * This file is the contract in compilable form. It checks every call's argument shape against the table in
 * nm_accel_contract.h and then answers kIOReturnUnsupported: no surface is created, nothing touches the GPU, and
 * the personality in Info.plist cannot match, so the kext is inert even if it were loaded. The bodies come next.
 */
#include <IOKit/IOLib.h>
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

/* ---- the public surface client --------------------------------------------------------------------------- */

class NMLegacySurfaceClient : public IOUserClient
{
    OSDeclareDefaultStructors(NMLegacySurfaceClient)
public:
    virtual bool initWithTask(task_t owningTask, void *securityID, UInt32 type);
    virtual IOReturn clientClose(void);
    virtual IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *arguments,
                                    IOExternalMethodDispatch *dispatch, OSObject *target, void *reference);
};
OSDefineMetaClassAndStructors(NMLegacySurfaceClient, IOUserClient)

bool NMLegacySurfaceClient::initWithTask(task_t owningTask, void *securityID, UInt32 type)
{
    return type == NM_SURFACE_CLIENT_TYPE && IOUserClient::initWithTask(owningTask, securityID, type);
}

IOReturn NMLegacySurfaceClient::clientClose(void)
{
    terminate();
    return kIOReturnSuccess;
}

IOReturn NMLegacySurfaceClient::externalMethod(uint32_t selector, IOExternalMethodArguments *arguments,
                                               IOExternalMethodDispatch *, OSObject *, void *)
{
    if (selector >= NM_SURFACE_METHOD_COUNT) return kIOReturnBadArgument;
    if (!nm_shape_ok(gSurfaceShape[selector], arguments)) return kIOReturnBadArgument;
    return kIOReturnUnsupported;   /* contract only: no method has a body yet */
}

/* ---- our private client ---------------------------------------------------------------------------------- */

class NMLegacyGLClient : public IOUserClient
{
    OSDeclareDefaultStructors(NMLegacyGLClient)
public:
    virtual bool initWithTask(task_t owningTask, void *securityID, UInt32 type);
    virtual IOReturn clientClose(void);
    virtual IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *arguments,
                                    IOExternalMethodDispatch *dispatch, OSObject *target, void *reference);
};
OSDefineMetaClassAndStructors(NMLegacyGLClient, IOUserClient)

bool NMLegacyGLClient::initWithTask(task_t owningTask, void *securityID, UInt32 type)
{
    return type == NM_GL_CLIENT_TYPE && IOUserClient::initWithTask(owningTask, securityID, type);
}

IOReturn NMLegacyGLClient::clientClose(void)
{
    terminate();
    return kIOReturnSuccess;
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
    case NM_GL_METHOD_SURFACE_BUFFER:
        if (a->structureInputSize != sizeof(nm_gl_surface_query) || a->structureOutputSize < sizeof(nm_gl_surface_buffer))
            return kIOReturnBadArgument;
        return kIOReturnUnsupported;
    case NM_GL_METHOD_SURFACE_PRESENTED:
        if (a->structureInputSize != sizeof(nm_gl_surface_query)) return kIOReturnBadArgument;
        return kIOReturnUnsupported;
    default:
        return kIOReturnBadArgument;
    }
}

/* ---- the accelerator ------------------------------------------------------------------------------------- */

class NMLegacyAccelerator : public IOAccelerator
{
    OSDeclareDefaultStructors(NMLegacyAccelerator)
public:
    virtual bool start(IOService *provider);
    virtual IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler);
};
OSDefineMetaClassAndStructors(NMLegacyAccelerator, IOAccelerator)

bool NMLegacyAccelerator::start(IOService *provider)
{
    if (!IOAccelerator::start(provider)) return false;
    /* No IOGLBundleName: Apple's engine must not look for a renderer bundle for this accelerator. The engine at
     * the framework's engine path recognises the accelerator by class instead. */
    registerService();
    return true;
}

IOReturn NMLegacyAccelerator::newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler)
{
    IOUserClient *c = NULL;
    if (type == NM_SURFACE_CLIENT_TYPE) c = new NMLegacySurfaceClient;
    else if (type == NM_GL_CLIENT_TYPE) c = new NMLegacyGLClient;
    else return kIOReturnBadArgument;
    if (!c) return kIOReturnNoMemory;
    if (!c->initWithTask(owningTask, securityID, type) || !c->attach(this) || !c->start(this)) {
        c->detach(this);
        c->release();
        return kIOReturnError;
    }
    *handler = c;
    return kIOReturnSuccess;
}
