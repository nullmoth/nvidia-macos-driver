#include "NMKbdBacklight.hpp"
#include <Headers/kern_version.hpp>

OSDefineMetaClassAndStructors(NMKbdBacklight, IOService)

bool ADDPR(debugEnabled) = false;
uint32_t ADDPR(debugPrintDelay) = 0;

/* LKSB and LKSS are 2-byte values. A write from CoreBrightness arrives in writeAccess() with the new bytes in `data`; a
 * read gets the level the firmware reports. */
class KbdBacklightValue : public VirtualSMCValue {
    NMKbdBacklight *owner;
public:
    explicit KbdBacklightValue(NMKbdBacklight *o) : owner(o) {}
protected:
    SMC_RESULT readAccess() override {
        int level = 0;
        if (owner->getLevel(&level)) kbl_smc_encode(kbl_level_to_smc(level, owner->maxLevel()), &data[0], &data[1]);
        return SmcSuccess;
    }
    SMC_RESULT writeAccess() override {
        owner->setLevel(kbl_smc_to_level(kbl_smc_decode(data[0], data[1]), owner->maxLevel()));
        return SmcSuccess;
    }
};

bool NMKbdBacklight::callAsus(uint32_t method, uint32_t argument, uint32_t *result) {
    uint32_t buf[2] = { KBL_ASUS_DEVID, argument };
    OSObject *params[3] = { OSNumber::withNumber(0ULL, 32), OSNumber::withNumber(method, 32), OSData::withBytes(buf, sizeof buf) };
    OSObject *out = nullptr;
    IOReturn ret = (params[0] && params[1] && params[2]) ? wmi->evaluateObject("WMNB", &out, params, 3) : kIOReturnNoMemory;
    for (auto p : params) if (p) p->release();
    if (ret != kIOReturnSuccess) { DBGLOG("nmkbl", "WMNB failed %x", ret); return false; }
    auto n = OSDynamicCast(OSNumber, out);
    if (result) *result = n ? (uint32_t)n->unsigned32BitValue() : 0;
    bool ok = n != nullptr;
    if (out) out->release();
    return ok;
}

bool NMKbdBacklight::setLevel(int level) {
    return wmi && callAsus(KBL_ASUS_METHOD_DEVS, kbl_asus_set_argument(level), nullptr);
}

bool NMKbdBacklight::getLevel(int *level) {
    uint32_t r = 0;
    if (!wmi || !callAsus(KBL_ASUS_METHOD_DSTS, 0, &r) || !kbl_asus_present(r)) return false;
    *level = kbl_asus_level(r);
    return true;
}

IOService *NMKbdBacklight::probe(IOService *provider, SInt32 *score) {
    if (!IOService::probe(provider, score)) return nullptr;
    uint32_t on = 0;
    if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return nullptr;   // off unless asked for

    auto dict = IOService::nameMatching("PNP0C14");
    if (!dict) return nullptr;
    auto it = IOService::getMatchingServices(dict);
    dict->release();
    if (!it) return nullptr;
    while (auto obj = it->getNextObject()) {
        auto dev = OSDynamicCast(IOACPIPlatformDevice, obj);
        if (dev && dev->validateObject("WMNB") == kIOReturnSuccess) { wmi = dev; break; }
    }
    it->release();
    if (!wmi) { SYSLOG("nmkbl", "no ASUS WMI device (WMNB) on this laptop"); return nullptr; }

    int level = 0;
    if (!getLevel(&level)) { SYSLOG("nmkbl", "the firmware reports no keyboard backlight"); wmi = nullptr; return nullptr; }

    setProperty("VersionInfo", kextVersion);
    vsmcNotifier = VirtualSMCAPI::registerHandler(vsmcNotificationHandler, this);
    return vsmcNotifier ? this : nullptr;
}

bool NMKbdBacklight::start(IOService *provider) { return IOService::start(provider); }

bool NMKbdBacklight::vsmcNotificationHandler(void *sensors, void *refCon, IOService *vsmc, IONotifier *notifier) {
    if (!sensors || !vsmc) return false;
    auto self = static_cast<NMKbdBacklight *>(sensors);
    uint8_t zero[2] = {0, 0};
    VirtualSMCAPI::addKey(SMC_MAKE_IDENTIFIER('L','K','S','B'), self->vsmcPlugin.data,
        VirtualSMCAPI::valueWithData(reinterpret_cast<const SMC_DATA *>(zero), sizeof zero, SmcKeyTypeLkb,
                                     new KbdBacklightValue(self), SMC_KEY_ATTRIBUTE_READ | SMC_KEY_ATTRIBUTE_WRITE));
    VirtualSMCAPI::addKey(SMC_MAKE_IDENTIFIER('L','K','S','S'), self->vsmcPlugin.data,
        VirtualSMCAPI::valueWithData(reinterpret_cast<const SMC_DATA *>(zero), sizeof zero, SmcKeyTypeLks,
                                     new KbdBacklightValue(self), SMC_KEY_ATTRIBUTE_READ | SMC_KEY_ATTRIBUTE_WRITE));
    auto ret = vsmc->callPlatformFunction(VirtualSMCAPI::SubmitPlugin, true, sensors, &self->vsmcPlugin, nullptr, nullptr);
    if (ret == kIOReturnSuccess) { DBGLOG("nmkbl", "submitted plugin"); return true; }
    if (ret != kIOReturnUnsupported) SYSLOG("nmkbl", "plugin submission failure %X", ret);
    return false;
}

void NMKbdBacklight::stop(IOService *) { PANIC("nmkbl", "a VirtualSMC plugin must not be stopped"); }

EXPORT extern "C" kern_return_t ADDPR(kern_start)(kmod_info_t *, void *) {
    lilu_get_boot_args("liludelay", &ADDPR(debugPrintDelay), sizeof(ADDPR(debugPrintDelay)));
    ADDPR(debugEnabled) = checkKernelArgument("-vsmcdbg") || checkKernelArgument("-nmkbldbg") || checkKernelArgument("-liludbgall");
    return KERN_SUCCESS;
}

EXPORT extern "C" kern_return_t ADDPR(kern_stop)(kmod_info_t *, void *) { return KERN_FAILURE; }
