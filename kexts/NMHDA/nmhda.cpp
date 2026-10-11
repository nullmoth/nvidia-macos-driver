// NMHDA: HDMI and DisplayPort audio from NVIDIA cards.
//
// macOS 15's AppleHDAController attaches only to HD Audio controllers listed in its own table of 48-byte entries
// (vendor, device, flags, ...). The NVIDIA entries are Fermi and Kepler parts (10DE:0BE3-0BEE, 0E08, 0E0A, 0E0B, 0E1A,
// 0E1B), so the audio function of any newer card (an RTX 5060 is 10DE:22EB) gets no driver and the card has no sound
// output. OpenCore cannot patch this kext: it lives in the system kernel collection, and OpenCore's kernel patcher
// reports "Not Found" for it.
//
// When AppleHDAController loads, this plugin reads the device id of each NVIDIA audio function (PCI class 0x0403,
// vendor 0x10DE) from the I/O Registry and rewrites one Kepler entry per card to that id, keeping the Kepler entry's
// flags. Nothing else in the controller changes.

#include <Headers/plugin_start.hpp>
#include <Headers/kern_api.hpp>
#include <Headers/kern_util.hpp>
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSData.h>
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
static IOService *wrapProbe(IOService *self, IOService *provider, SInt32 *score) {
    auto r = FunctionCast(wrapProbe, orgProbe)(self, provider, score);
    if (isNvidiaAudio(provider)) {
        // AppleHDAController fields probe fills (macOS 15.8 layout): vendor/device read from config space, the matched
        // table entry's capability word, and the HDA-GFX flags
        auto *c = reinterpret_cast<const uint8_t *>(self);
        report("probe", "probe %s -> %s (ven %04x dev %04x cap %04x eb4 %u gfx %u)", provider->getName(), r ? "accepted" : "REFUSED",
               *(const uint32_t *)(c + 0xef0) & 0xFFFF, *(const uint32_t *)(c + 0xef4) & 0xFFFF, *(const uint16_t *)(c + 0x2d4),
               c[0xeb4], c[0x1e60]);
    }
    return r;
}
static bool wrapStart(IOService *self, IOService *provider) {
    bool r = FunctionCast(wrapStart, orgStart)(self, provider);
    if (isNvidiaAudio(provider)) report("start", "start %s -> %s", provider->getName(), r ? "ok" : "FAILED");
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

static void onHdaLoaded(void *, KernelPatcher &patcher, size_t index, mach_vm_address_t, size_t) {
    report("nmhda-controller", "AppleHDAController loaded (index %lu, ours %lu)", (unsigned long)index, (unsigned long)kHda.loadIndex);
    if (index != kHda.loadIndex) return;
    KernelPatcher::RouteRequest routes[] {
        { "__ZN18AppleHDAController5probeEP9IOServicePi", wrapProbe, orgProbe },
        { "__ZN18AppleHDAController5startEP9IOService", wrapStart, orgStart },
    };
    if (!patcher.routeMultiple(index, routes, arrsize(routes)))
        report("route", "could not wrap probe/start (patcher error %d)", patcher.getError());
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
    // With AppleALC loaded, AppleHDAController's probe reads the NVIDIA function as 10DE:9D70 (an Intel controller id), so
    // the card's own id never matches; the next free Kepler entry takes that id too (no NVIDIA device uses it). Measured
    // with this plugin's probe report on an RTX 5060: "ven 10de dev 9d70".
    if (patched && n < arrsize(kKepler)) {
        const uint16_t k = kKepler[n];
        const uint8_t find[8] { 0xDE, 0x10, (uint8_t)(k & 0xFF), (uint8_t)(k >> 8), 0x01, 0x00, 0x00, 0x00 };
        const uint8_t repl[8] { 0xDE, 0x10, 0x70, 0x9D, 0x01, 0x00, 0x00, 0x00 };
        KernelPatcher::LookupPatch p { &kHda, find, repl, sizeof(find), 1 };
        patcher.clearError();
        patcher.applyLookupPatch(&p);
        report("nmhda-result", "HD Audio table: 10DE:9D70 %s (in place of Kepler 10DE:%04X)",
               patcher.getError() == KernelPatcher::Error::NoError ? "accepted" : "NOT added", k);
        patcher.clearError();
    }
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
    KernelVersion::Sequoia, KernelVersion::Sequoia,
    pluginStart
};

// kmutil places a kext in a kernel collection only when it has a _kmod_info symbol
extern "C" kern_return_t NMHDA_kern_start(kmod_info_t *, void *);
extern "C" kern_return_t NMHDA_kern_stop(kmod_info_t *, void *);
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMHDA, "1.0.0", NMHDA_kern_start, NMHDA_kern_stop) }
