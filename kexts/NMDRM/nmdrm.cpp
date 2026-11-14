// NMDRM: DRM device nodes for macOS, so Mesa's Vulkan drivers (RADV for AMD, ANV for Intel) reach a GPU kernel driver
// through the same interface they use on Linux and the BSDs.
//
// Each display-class PCI device gets /dev/dri/cardN and /dev/dri/renderD(128+N). This stage implements the DRM core
// ioctls libdrm needs to enumerate a device - VERSION, GET_UNIQUE, GET_CAP, GET_PCIINFO - and touches no GPU register:
// identity comes from the PCI properties the platform expert already published. The driver-specific ioctls (amdgpu,
// i915/xe) are added behind this interface stage by stage; until a family's ioctls exist, VERSION reports "nmdrm", so
// RADV and ANV skip the device instead of failing half way through initialisation.
//
// Matching uses its own IOMatchCategory, so NMDRM sits beside whatever driver owns the device (NVRM, Apple's AMD
// drivers, IONDRVFramebuffer) without competing for it. Boot argument -nmdrmoff disables it.
//
// ioctl numbers and structure layouts: libdrm include/drm/drm.h (MIT); GET_PCIINFO as libdrm defines it for OpenBSD
// and Darwin.
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/OSAtomic.h>
#include <sys/conf.h>
#include <sys/errno.h>
#include <sys/fcntl.h>
#include <sys/ioccom.h>
#include <sys/systm.h>
#include <miscfs/devfs/devfs.h>
#include <pexpert/pexpert.h>
// kmutil places a kext in a kernel collection only with a _kmod_info symbol (Xcode generates it; this build does not)
#include <mach/kmod.h>

#define NMDRM_LOG(fmt, ...) IOLog("NMDRM: " fmt "\n", ##__VA_ARGS__)

// --- DRM uapi (libdrm include/drm/drm.h). Pointer and size fields are 64-bit: macOS 15 runs 64-bit processes only. ---
struct nm_drm_version {
    int32_t version_major, version_minor, version_patchlevel;
    uint32_t pad0;
    uint64_t name_len; uint64_t name;
    uint64_t date_len; uint64_t date;
    uint64_t desc_len; uint64_t desc;
};
struct nm_drm_unique { uint64_t unique_len; uint64_t unique; };
struct nm_drm_get_cap { uint64_t capability; uint64_t value; };
struct nm_drm_pciinfo {
    uint16_t domain; uint8_t bus; uint8_t dev; uint8_t func;
    uint16_t vendor_id; uint16_t device_id; uint16_t subvendor_id; uint16_t subdevice_id; uint8_t revision_id;
};
#define NM_DRM_IOCTL_BASE 'd'
#define NM_DRM_IOCTL_VERSION     _IOWR(NM_DRM_IOCTL_BASE, 0x00, struct nm_drm_version)
#define NM_DRM_IOCTL_GET_UNIQUE  _IOWR(NM_DRM_IOCTL_BASE, 0x01, struct nm_drm_unique)
#define NM_DRM_IOCTL_GET_CAP     _IOWR(NM_DRM_IOCTL_BASE, 0x0c, struct nm_drm_get_cap)
#define NM_DRM_IOCTL_GET_PCIINFO _IOR(NM_DRM_IOCTL_BASE, 0x15, struct nm_drm_pciinfo)
#define NM_DRM_CAP_PRIME 0x5

static_assert(sizeof(nm_drm_version) == 64, "drm_version layout");
// natural alignment, as libdrm declares it: the ioctl number encodes this size, so both sides must agree
static_assert(sizeof(nm_drm_pciinfo) == 16, "drm_pciinfo layout (OpenBSD/libdrm)");

// Linux numbers primary nodes from 0 and render nodes from 128; libdrm derives one from the other.
static constexpr int kMaxDevices = 16;
static constexpr int kRenderMinorBase = 128;
static const char kDriverName[] = "nmdrm";
static const char kDriverDate[] = "20261011";
static const char kDriverDesc[] = "NullMoth DRM device nodes";

struct NMDRMSlot {
    bool used;
    bool gone;            // provider terminated; open descriptors now get ENXIO
    nm_drm_pciinfo pci;
    char unique[32];      // "pci:DDDD:BB:DD.F", what Linux returns for GET_UNIQUE
    void *cardNode, *renderNode;
};

static NMDRMSlot gSlots[kMaxDevices];
static IOLock *gLock;
static int gMajor = -1;
static SInt32 gOpenCount;

static int slotForMinor(int minor)
{
    if (minor >= kRenderMinorBase) minor -= kRenderMinorBase;
    return (minor >= 0 && minor < kMaxDevices) ? minor : -1;
}

static int nmdrm_open(dev_t dev, int flags, int devtype, struct proc *p)
{
    int s = slotForMinor(minor(dev));
    if (s < 0) return ENXIO;
    IOLockLock(gLock);
    int err = (gSlots[s].used && !gSlots[s].gone) ? 0 : ENXIO;
    if (!err) OSIncrementAtomic(&gOpenCount);
    IOLockUnlock(gLock);
    return err;
}

static int nmdrm_close(dev_t dev, int flags, int devtype, struct proc *p)
{
    OSDecrementAtomic(&gOpenCount);
    return 0;
}

// Linux semantics: lengths come back as the full string length; at most the caller's length is copied.
static int copyString(const char *s, uint64_t *len, uint64_t uaddr)
{
    size_t n = strlen(s);
    int err = 0;
    if (uaddr && *len) err = copyout(s, (user_addr_t)uaddr, (size_t)(*len < n ? *len : n));
    *len = n;
    return err;
}

static int nmdrm_ioctl(dev_t dev, u_long cmd, caddr_t data, int fflag, struct proc *p)
{
    int s = slotForMinor(minor(dev));
    if (s < 0) return ENXIO;

    IOLockLock(gLock);
    bool live = gSlots[s].used && !gSlots[s].gone;
    NMDRMSlot slot = gSlots[s];
    IOLockUnlock(gLock);
    if (!live) return ENXIO;

    switch (cmd) {
    case NM_DRM_IOCTL_VERSION: {
        auto *v = (nm_drm_version *)data;
        v->version_major = 0; v->version_minor = 1; v->version_patchlevel = 0;
        int err = copyString(kDriverName, &v->name_len, v->name);
        if (!err) err = copyString(kDriverDate, &v->date_len, v->date);
        if (!err) err = copyString(kDriverDesc, &v->desc_len, v->desc);
        return err;
    }
    case NM_DRM_IOCTL_GET_UNIQUE: {
        auto *u = (nm_drm_unique *)data;
        return copyString(slot.unique, &u->unique_len, u->unique);
    }
    case NM_DRM_IOCTL_GET_CAP: {
        auto *c = (nm_drm_get_cap *)data;
        // No buffer sharing exists yet; every other capability is unknown to this stage.
        if (c->capability == NM_DRM_CAP_PRIME) { c->value = 0; return 0; }
        return EINVAL;
    }
    case NM_DRM_IOCTL_GET_PCIINFO:
        *(nm_drm_pciinfo *)data = slot.pci;
        return 0;
    default:
        return ENOTTY;
    }
}

static struct cdevsw gCdevsw = {
    .d_open = nmdrm_open,
    .d_close = nmdrm_close,
    .d_read = eno_rdwrt,
    .d_write = eno_rdwrt,
    .d_ioctl = nmdrm_ioctl,
    .d_stop = eno_stop,
    .d_reset = eno_reset,
    .d_ttys = nullptr,
    .d_select = eno_select,
    .d_mmap = eno_mmap,
    .d_strategy = eno_strat,
    .d_reserved_1 = eno_getc,
    .d_reserved_2 = eno_putc,
    .d_type = 0,
};

static uint16_t pciProp16(IOPCIDevice *d, const char *key)
{
    auto *data = OSDynamicCast(OSData, d->getProperty(key));
    if (!data || data->getLength() < 2) return 0;
    const uint8_t *b = (const uint8_t *)data->getBytesNoCopy();
    return (uint16_t)(b[0] | (b[1] << 8));
}

class NMDRMDevice : public IOService {
    OSDeclareDefaultStructors(NMDRMDevice)
    int fSlot = -1;
public:
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
};

OSDefineMetaClassAndStructors(NMDRMDevice, IOService)

bool NMDRMDevice::start(IOService *provider)
{
    if (!IOService::start(provider)) return false;
    auto *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci || gMajor < 0) return false;

    char arg[8];
    if (PE_parse_boot_argn("-nmdrmoff", arg, sizeof(arg))) { NMDRM_LOG("disabled by -nmdrmoff"); return false; }

    nm_drm_pciinfo info = {};
    info.domain = 0;
    info.bus = pci->getBusNumber();
    info.dev = pci->getDeviceNumber();
    info.func = pci->getFunctionNumber();
    info.vendor_id = pciProp16(pci, "vendor-id");
    info.device_id = pciProp16(pci, "device-id");
    info.subvendor_id = pciProp16(pci, "subsystem-vendor-id");
    info.subdevice_id = pciProp16(pci, "subsystem-id");
    info.revision_id = (uint8_t)pciProp16(pci, "revision-id");

    IOLockLock(gLock);
    int s = -1;
    for (int i = 0; i < kMaxDevices; i++) if (!gSlots[i].used) { s = i; break; }
    if (s >= 0) {
        NMDRMSlot &sl = gSlots[s];
        bzero(&sl, sizeof(sl));
        sl.used = true;
        sl.pci = info;
        snprintf(sl.unique, sizeof(sl.unique), "pci:%04x:%02x:%02x.%u", info.domain, info.bus, info.dev, info.func);
    }
    IOLockUnlock(gLock);
    if (s < 0) { NMDRM_LOG("more than %d display devices; %04x:%04x not published", kMaxDevices, info.vendor_id, info.device_id); return false; }

    // devfs creates the dri/ directory with the first node. Render nodes are world-usable as on Linux; primary nodes are
    // for the owner (root) and the video group, since modesetting will live there.
    void *card = devfs_make_node(makedev(gMajor, s), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0660, "dri/card%d", s);
    void *render = devfs_make_node(makedev(gMajor, kRenderMinorBase + s), DEVFS_CHAR, UID_ROOT, GID_WHEEL, 0666,
                                   "dri/renderD%d", kRenderMinorBase + s);
    if (!card || !render) {
        if (card) devfs_remove(card);
        if (render) devfs_remove(render);
        IOLockLock(gLock); gSlots[s].used = false; IOLockUnlock(gLock);
        NMDRM_LOG("devfs_make_node failed for slot %d", s);
        return false;
    }
    IOLockLock(gLock);
    gSlots[s].cardNode = card;
    gSlots[s].renderNode = render;
    IOLockUnlock(gLock);
    fSlot = s;

    NMDRM_LOG("%04x:%04x (subsystem %04x:%04x rev %02x) at %s -> /dev/dri/card%d, /dev/dri/renderD%d",
              info.vendor_id, info.device_id, info.subvendor_id, info.subdevice_id, info.revision_id,
              gSlots[s].unique, s, kRenderMinorBase + s);
    registerService();
    return true;
}

void NMDRMDevice::stop(IOService *provider)
{
    if (fSlot >= 0) {
        IOLockLock(gLock);
        NMDRMSlot &sl = gSlots[fSlot];
        void *card = sl.cardNode, *render = sl.renderNode;
        sl.cardNode = sl.renderNode = nullptr;
        // The slot stays reserved while descriptors may still be open, so a new device never inherits them.
        sl.gone = true;
        IOLockUnlock(gLock);
        if (card) devfs_remove(card);
        if (render) devfs_remove(render);
        NMDRM_LOG("slot %d removed", fSlot);
        fSlot = -1;
    }
    IOService::stop(provider);
}

extern "C" kern_return_t nmdrm_kmod_start(kmod_info_t *, void *)
{
    gLock = IOLockAlloc();
    if (!gLock) return KERN_FAILURE;
    gMajor = cdevsw_add(-1, &gCdevsw);
    if (gMajor < 0) { IOLockFree(gLock); gLock = nullptr; NMDRM_LOG("cdevsw_add failed"); return KERN_FAILURE; }
    return KERN_SUCCESS;
}

extern "C" kern_return_t nmdrm_kmod_stop(kmod_info_t *, void *)
{
    // A descriptor still open would call into unloaded code.
    if (gOpenCount > 0) { NMDRM_LOG("unload refused: %d open descriptor(s)", (int)gOpenCount); return KERN_FAILURE; }
    for (int i = 0; i < kMaxDevices; i++)
        if (gSlots[i].used && !gSlots[i].gone) { NMDRM_LOG("unload refused: device %d still attached", i); return KERN_FAILURE; }
    if (gMajor >= 0) cdevsw_remove(gMajor, &gCdevsw);
    gMajor = -1;
    if (gLock) IOLockFree(gLock);
    gLock = nullptr;
    return KERN_SUCCESS;
}

// The kernel runs this kext's C++ static initialisers (the OSMetaClass for NMDRMDevice) before calling start.
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMDRM, "1.0.0", nmdrm_kmod_start, nmdrm_kmod_stop) }
