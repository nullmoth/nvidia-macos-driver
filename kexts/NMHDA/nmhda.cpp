// NMHDA: HDMI and DisplayPort audio from NVIDIA cards.
//
// NMHDA adds NVIDIA HD Audio device IDs to AppleHDAController's table. When
// AppleALC applies its shared Intel device-ID spoof, AppleHDA's native
// HwFactoryPrefixTranslation restores the NVIDIA ID before codec probing.

#include <Headers/plugin_start.hpp>
#include <Headers/kern_api.hpp>
#include <Headers/kern_util.hpp>
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSArray.h>
#include <libkern/c++/OSNumber.h>
#include <mach/kmod.h>
#include <stdarg.h>
#include <sys/sysctl.h>

static const char *kHdaPath[] {
    "/System/Library/Extensions/AppleHDA.kext/Contents/PlugIns/AppleHDAController.kext/Contents/MacOS/AppleHDAController"
};
static KernelPatcher::KextInfo kHda { "com.apple.driver.AppleHDAController", kHdaPath, arrsize(kHdaPath), {true}, {},
                                       KernelPatcher::KextInfo::Unloaded };

// Kepler entries replaced in this order, one per NVIDIA audio function found.
static const uint16_t kKepler[] { 0x0E0A, 0x0E0B, 0x0E1B, 0x0E1A, 0x0E08 };

static uint32_t readU32(IORegistryEntry *e, const char *key) {
    auto d = OSDynamicCast(OSData, e->getProperty(key));
    if (!d || d->getLength() < 2) return 0xFFFFFFFF;
    uint32_t v = 0;
    memcpy(&v, d->getBytesNoCopy(), d->getLength() < 4 ? d->getLength() : 4);
    return v;
}

// NVIDIA audio functions: vendor 0x10DE, class code 0x0403xx (HD Audio). At most arrsize(kKepler) are returned.
static size_t nvidiaAudioIds(uint16_t *ids, size_t max) {
    size_t n = 0;
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (!match) return 0;
    OSIterator *it = IOService::getMatchingServices(match);
    match->release();
    if (!it) return 0;
    while (auto o = it->getNextObject()) {
        auto pci = OSDynamicCast(IOPCIDevice, o);
        if (!pci) continue;
        if ((readU32(pci, "vendor-id") & 0xFFFF) != 0x10DE) continue;
        if ((readU32(pci, "class-code") & 0xFFFF00) != 0x040300) continue;
        uint16_t dev = (uint16_t)(readU32(pci, "device-id") & 0xFFFF);
        bool seen = false;
        for (size_t i = 0; i < n; i++) seen |= ids[i] == dev;
        if (!seen && n < max) ids[n++] = dev;
    }
    it->release();
    return n;
}

// Lilu keeps its log in its own buffer, so the outcome is also kept in a read-only sysctl, debug.nmhda (the newest
// lines, oldest first), which Send logs and a user can read at any time after boot.
static char gReport[512];
SYSCTL_STRING(_debug, OID_AUTO, nmhda, CTLFLAG_RD | CTLFLAG_KERN, gReport, sizeof(gReport), "NVIDIA HD Audio table patch");
static void report(const char *, const char *fmt, ...) {
    char line[160]; va_list ap; va_start(ap, fmt); vsnprintf(line, sizeof line, fmt, ap); va_end(ap);
    SYSLOG("nmhda", "%s", line);
    size_t used = strlen(gReport), need = strlen(line) + 2;
    if (used + need > sizeof(gReport)) { gReport[0] = 0; used = 0; }   // keep the newest lines
    snprintf(gReport + used, sizeof(gReport) - used, "%s%s", used ? " | " : "", line);
}

// probe/start results for NVIDIA audio functions, so a refusal says which step refused
static mach_vm_address_t orgProbe, orgStart;
static bool isNvidiaAudio(IOService *p) {
    auto pci = OSDynamicCast(IOPCIDevice, p);
    return pci && (readU32(pci, "vendor-id") & 0xFFFF) == 0x10DE && (readU32(pci, "class-code") & 0xFFFF00) == 0x040300;
}
static bool setNumber(OSDictionary *dict, const char *key, uint32_t value) {
    auto number = OSNumber::withNumber(value, 32);
    if (!number) return false;
    bool ok = dict->setObject(key, number);
    number->release();
    return ok;
}
static uint32_t numberValue(OSDictionary *dict, const char *key) {
    auto number = OSDynamicCast(OSNumber, dict->getObject(key));
    return number ? number->unsigned32BitValue() : 0xFFFFFFFF;
}
static bool restoreNvidiaDeviceId(IOService *self, IOService *provider) {
    uint32_t layout = readU32(provider, "layout-id");
    uint32_t device = readU32(provider, "device-id");
    // Without a layout, the native translation key cannot be built; keep the normal probe path.
    if (layout == 0xFFFFFFFF) return true;
    if (device == 0xFFFFFFFF) return false;
    auto existing = OSDynamicCast(OSArray, self->getProperty("HwFactoryPrefixTranslation"));
    if (existing) {
        for (unsigned i = 0; i < existing->getCount(); i++) {
            auto entry = OSDynamicCast(OSDictionary, existing->getObject(i));
            if (entry && numberValue(entry, "LayoutID") == layout && numberValue(entry, "VID") == 0x10DE &&
                numberValue(entry, "SourceDID") == 0x9D70 && numberValue(entry, "StandInDID") == (device & 0xFFFF))
                return true;
        }
    }
    auto entries = existing ? OSArray::withArray(existing) : OSArray::withCapacity(1);
    auto entry = OSDictionary::withCapacity(4);
    bool ok = entries && entry && setNumber(entry, "LayoutID", layout) &&
        setNumber(entry, "VID", 0x10DE) && setNumber(entry, "SourceDID", 0x9D70) &&
        setNumber(entry, "StandInDID", device & 0xFFFF) && entries->setObject(entry) &&
        self->setProperty("HwFactoryPrefixTranslation", entries);
    if (entry) entry->release();
    if (entries) entries->release();
    return ok;
}
static IOService *wrapProbe(IOService *self, IOService *provider, SInt32 *score) {
    if (isNvidiaAudio(provider) && !restoreNvidiaDeviceId(self, provider)) {
        report("probe", "cannot prepare NVIDIA native device-id translation");
        return nullptr;
    }
    auto r = FunctionCast(wrapProbe, orgProbe)(self, provider, score);
    if (isNvidiaAudio(provider)) {
        report("probe", "probe %s -> %s (10DE:%04x)", provider->getName(), r ? "accepted" : "REFUSED",
               (unsigned)(readU32(provider, "device-id") & 0xFFFF));
    }
    return r;
}
static bool wrapStart(IOService *self, IOService *provider) {
    bool r = FunctionCast(wrapStart, orgStart)(self, provider);
    if (isNvidiaAudio(provider)) {
        report("start", "start %s -> %s", provider->getName(), r ? "ok" : "FAILED");
        // On a GPU audio link (hda-gfx) the controller keeps the codec link down until the GPU's audio driver announces
        // itself with iokit_vendor_specific_msg(0) (0xE3FF8000, sender = that driver); AppleGFXHDA does this for AMD and
        // Intel, and nothing does it for NVIDIA. The GPU function (function 0 of the same slot) stands in as the sender.
        if (r) {
            IOService *gpu = nullptr;
            if (auto parent = provider->getParentEntry(gIOServicePlane))
                if (auto it = parent->getChildIterator(gIOServicePlane)) {
                    while (auto o = it->getNextObject()) {
                        auto pci = OSDynamicCast(IOPCIDevice, o);
                        if (pci && (readU32(pci, "vendor-id") & 0xFFFF) == 0x10DE && (readU32(pci, "class-code") & 0xFF0000) == 0x030000) { gpu = pci; break; }
                    }
                    it->release();
                }
            IOReturn m = gpu ? self->message(0xE3FF8000, gpu, nullptr) : kIOReturnNotFound;
            report("link", "graphics link announce via %s -> 0x%x", gpu ? gpu->getName() : "(no GPU found)", m);
        }
    }
    return r;
}

static void rematchNvidiaAudio() {
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (!match) return;
    OSIterator *it = IOService::getMatchingServices(match);
    match->release();
    if (!it) return;
    size_t n = 0;
    while (auto o = it->getNextObject()) {
        auto pci = OSDynamicCast(IOPCIDevice, o);
        if (!pci || (readU32(pci, "vendor-id") & 0xFFFF) != 0x10DE || (readU32(pci, "class-code") & 0xFFFF00) != 0x040300) continue;
        pci->registerService(kIOServiceAsynchronous);
        n++;
    }
    it->release();
    report("nmhda-rematch", "matching restarted on %lu NVIDIA audio function(s)", (unsigned long)n);
}

static void onHdaLoaded(void *, KernelPatcher &patcher, size_t index, mach_vm_address_t address, size_t size) {
    if (orgProbe) return;
    auto probe = patcher.solveSymbol(index, "__ZN18AppleHDAController5probeEP9IOServicePi", address, size);
    patcher.clearError();
    if (!probe) {
        report("nmhda-controller", "could not find AppleHDAController probe in runtime image %lu", (unsigned long)index);
        return;
    }
    report("nmhda-controller", "AppleHDAController runtime image %lu (registered %lu)",
           (unsigned long)index, (unsigned long)kHda.loadIndex);
    kHda.loadIndex = index;
    KernelPatcher::RouteRequest routes[] {
        { "__ZN18AppleHDAController5probeEP9IOServicePi", wrapProbe, orgProbe },
        { "__ZN18AppleHDAController5startEP9IOService", wrapStart, orgStart },
    };
    if (!patcher.routeMultiple(index, routes, arrsize(routes))) {
        report("route", "could not wrap probe/start (patcher error %d)", patcher.getError());
        patcher.clearError();
        return;
    }
    patcher.clearError();
    uint16_t ids[arrsize(kKepler)] {};
    size_t n = nvidiaAudioIds(ids, arrsize(ids));
    if (n == 0) { report("nmhda-result", "no NVIDIA HD Audio function found - nothing to do"); return; }
    bool patched = false;
    for (size_t i = 0; i < n; i++) {
        bool kepler = false;
        for (auto k : kKepler) kepler |= ids[i] == k;
        if (kepler) { report("nmhda-result", "10DE:%04X is already in the controller's table", ids[i]); continue; }
        // entry prefix: vendor (LE), device (LE), flags 0x00000001 as the Kepler entries carry
        const uint8_t find[8] { 0xDE, 0x10, (uint8_t)(kKepler[i] & 0xFF), (uint8_t)(kKepler[i] >> 8), 0x01, 0x00, 0x00, 0x00 };
        const uint8_t repl[8] { 0xDE, 0x10, (uint8_t)(ids[i] & 0xFF), (uint8_t)(ids[i] >> 8), 0x01, 0x00, 0x00, 0x00 };
        KernelPatcher::LookupPatch p { &kHda, find, repl, sizeof(find), 1 };
        patcher.clearError();
        patcher.applyLookupPatch(&p);
        if (patcher.getError() == KernelPatcher::Error::NoError) {
            report("nmhda-result", "HD Audio table: 10DE:%04X now accepted (in place of Kepler 10DE:%04X)", ids[i], kKepler[i]);
            patched = true;
        } else
            report("nmhda-result", "HD Audio table: could not add 10DE:%04X (patcher error %d)", ids[i], patcher.getError());
    }
    // AppleHDAController is usually loaded (for the board's own audio) before Lilu reaches it, so the NVIDIA function was
    // already probed against the unpatched table and refused, and I/O Kit does not probe a device twice by itself. Matching
    // runs again for each NVIDIA audio function now that the table accepts it.
    if (patched) rematchNvidiaAudio();
}

static void pluginStart() {
    sysctl_register_oid(&sysctl__debug_nmhda);
    report("nmhda-started", "waiting for AppleHDAController");
    lilu.onKextLoadForce(&kHda, 1, onHdaLoaded);
}

static const char *bootargOff[] { "-nmhdaoff" };
static const char *bootargDebug[] { "-nmhdadbg" };
static const char *bootargBeta[] { "-nmhdabeta" };

PluginConfiguration ADDPR(config) {
    xStringify(PRODUCT_NAME),
    parseModuleVersion(xStringify(MODULE_VERSION)),
    LiluAPI::AllowNormal | LiluAPI::AllowInstallerRecovery | LiluAPI::AllowSafeMode,
    bootargOff, arrsize(bootargOff),
    bootargDebug, arrsize(bootargDebug),
    bootargBeta, arrsize(bootargBeta),
    KernelVersion::Sequoia, KernelVersion::Tahoe,
    pluginStart
};

// kmutil places a kext in a kernel collection only when it has a _kmod_info symbol
extern "C" kern_return_t NMHDA_kern_start(kmod_info_t *, void *);
extern "C" kern_return_t NMHDA_kern_stop(kmod_info_t *, void *);
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMHDA, "1.0.0", NMHDA_kern_start, NMHDA_kern_stop) }
