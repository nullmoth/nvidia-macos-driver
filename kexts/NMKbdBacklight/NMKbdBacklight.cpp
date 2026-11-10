#include "NMKbdBacklight.hpp"
#include <Headers/kern_version.hpp>
#include "NMKbdBacklightHID.hpp"

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

/* the first ACPI device matching `nameMatch` that has `method` */
IOACPIPlatformDevice *NMKbdBacklight::find(const char *nameMatch, const char *method) {
    auto dict = IOService::nameMatching(nameMatch);
    if (!dict) return nullptr;
    auto it = IOService::getMatchingServices(dict);
    dict->release();
    if (!it) return nullptr;
    IOACPIPlatformDevice *found = nullptr;
    while (auto obj = it->getNextObject()) {
        auto d = OSDynamicCast(IOACPIPlatformDevice, obj);
        if (d && d->validateObject(method) == kIOReturnSuccess) { found = d; break; }
    }
    it->release();
    return found;
}

bool NMKbdBacklight::evalInt(const char *method, uint32_t arg, uint32_t *result, bool noArgument) {
    { uint32_t on = 0; if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return false; }   // firmware is called only with nmkbl=1
    OSObject *params[1] = { noArgument ? nullptr : OSNumber::withNumber(arg, 32) };
    OSObject *out = nullptr;
    IOReturn ret = noArgument ? dev->evaluateObject(method, &out, nullptr, 0)
                              : (params[0] ? dev->evaluateObject(method, &out, params, 1) : kIOReturnNoMemory);
    if (params[0]) params[0]->release();
    if (ret != kIOReturnSuccess) { DBGLOG("nmkbl", "%s failed %x", method, ret); return false; }
    auto n = OSDynamicCast(OSNumber, out);
    if (result) *result = n ? (uint32_t)n->unsigned32BitValue() : 0;
    bool ok = n != nullptr;
    if (out) out->release();
    return ok;
}

bool NMKbdBacklight::callAsus(uint32_t method, uint32_t argument, uint32_t *result) {
    { uint32_t on = 0; if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return false; }   // firmware is called only with nmkbl=1
    uint32_t buf[2] = { KBL_ASUS_DEVID, argument };
    OSObject *params[3] = { OSNumber::withNumber(0ULL, 32), OSNumber::withNumber(method, 32), OSData::withBytes(buf, sizeof buf) };
    OSObject *out = nullptr;
    IOReturn ret = (params[0] && params[1] && params[2]) ? dev->evaluateObject("WMNB", &out, params, 3) : kIOReturnNoMemory;
    for (auto p : params) if (p) p->release();
    if (ret != kIOReturnSuccess) { DBGLOG("nmkbl", "WMNB failed %x", ret); return false; }
    auto n = OSDynamicCast(OSNumber, out);
    if (result) *result = n ? (uint32_t)n->unsigned32BitValue() : 0;
    bool ok = n != nullptr;
    if (out) out->release();
    return ok;
}

// Dell: one SMBIOS call through the WMI method named from _WDG, with the buffer layout in kbl_core.h.
bool NMKbdBacklight::dellCall(uint32_t in0, uint32_t in1, uint32_t in2, uint32_t *out1, uint32_t *out2) {
    { uint32_t on = 0; if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return false; }   // firmware is called only with nmkbl=1
    uint8_t *raw = (uint8_t *)IOMalloc(dellBuf);
    if (!raw) return false;
    bzero(raw, dellBuf);
    kbl_dell_fill(raw, in0, in1, in2, 0);
    OSObject *params[3] = { OSNumber::withNumber(0ULL, 32), OSNumber::withNumber(1ULL, 32), OSData::withBytes(raw, dellBuf) };
    OSObject *out = nullptr;
    IOReturn ret = (params[0] && params[1] && params[2]) ? dev->evaluateObject(dellMethod, &out, params, 3) : kIOReturnNoMemory;
    for (auto p : params) if (p) p->release();
    IOFree(raw, dellBuf);
    if (ret != kIOReturnSuccess) { DBGLOG("nmkbl", "%s failed %x", dellMethod, ret); return false; }
    auto d = OSDynamicCast(OSData, out);
    bool ok = d && d->getLength() >= KBL_DELL_BUF;
    if (ok) { if (out1) *out1 = kbl_dell_output((const uint8_t *)d->getBytesNoCopy(), 1); if (out2) *out2 = kbl_dell_output((const uint8_t *)d->getBytesNoCopy(), 2); }
    if (out) out->release();
    return ok;
}

// MSI: the embedded controller through ACPI's embedded-controller address space.
bool NMKbdBacklight::ecRead(uint8_t addr, uint8_t *v) {
    UInt64 x = 0;
    IOACPIAddress at; at.addr64 = addr;
    if (dev->readAddressSpace(&x, kIOACPIAddressSpaceIDEmbeddedController, at, 8) != kIOReturnSuccess) return false;
    *v = (uint8_t)x;
    return true;
}

bool NMKbdBacklight::ecWrite(uint8_t addr, uint8_t v) {
    IOACPIAddress at; at.addr64 = addr;
    return dev->writeAddressSpace(v, kIOACPIAddressSpaceIDEmbeddedController, at, 8) == kIOReturnSuccess;
}

bool NMKbdBacklight::probeMsi() {
    dev = find("PNP0C09", "_STA");   // the embedded controller device
    if (!dev) return false;
    char fw[KBL_MSI_FW_LENGTH + 1];
    for (int i = 0; i < KBL_MSI_FW_LENGTH; i++) {
        uint8_t b = 0;
        if (!ecRead((uint8_t)(KBL_MSI_FW_ADDRESS + i), &b)) return false;
        fw[i] = (char)b;
    }
    fw[KBL_MSI_FW_LENGTH] = 0;
    msiRow = kbl_msi_find(fw);
    if (!msiRow) return false;      // a firmware the table does not name: the controller is not touched again
    backend = Msi; maxLevel_ = msiRow->max_state;
    int l; return getLevel(&l);
}

bool NMKbdBacklight::setLevel(int level) {
    if (!dev) return false;
    switch (backend) {
    case Asus: return callAsus(KBL_ASUS_METHOD_DEVS, kbl_asus_set_argument(level), nullptr);
    case IdeaPad: return evalInt("KBLC", kbl_lenovo_set_command(lenovoType, level), nullptr);
    case IdeaPadPlain: return evalInt("SALS", kbl_lenovo_plain_set_argument(level), nullptr);
    case ThinkPad: return evalInt("MLCS", (uint32_t)level, nullptr);
    case Msi: return ecWrite(msiRow->addr, kbl_msi_value(msiRow, level));
    case Dell: {
        uint32_t o1 = 0, o2 = 0;
        if (!dellCall(1, 0, 0, &o1, &o2)) return false;
        struct kbl_dell_state st; kbl_dell_parse_state(o1, o2, &st);
        st.level = level;
        uint32_t i1, i2; kbl_dell_set_inputs(&st, &i1, &i2);
        return dellCall(2, i1, i2, nullptr, nullptr);
    }
    default: return false;
    }
}

bool NMKbdBacklight::getLevel(int *level) {
    uint32_t r = 0;
    if (!dev) return false;
    switch (backend) {
    case Asus:
        if (!callAsus(KBL_ASUS_METHOD_DSTS, 0, &r) || !kbl_asus_present(r)) return false;
        *level = kbl_asus_level(r); return true;
    case IdeaPad: {
        if (!evalInt("KBLC", kbl_lenovo_get_command(lenovoType), &r)) return false;
        int l = kbl_lenovo_level_from_answer(r);
        if (l < 0) return false;
        *level = l; return true;
    }
    case IdeaPadPlain:
        if (!evalInt("HALS", 0, &r, true) || !kbl_lenovo_plain_supported(r)) return false;
        *level = kbl_lenovo_plain_level(r); return true;
    case ThinkPad:
        if (!evalInt("MLCG", 0, &r) || !kbl_thinkpad_present(r)) return false;
        *level = kbl_thinkpad_level(r); return true;
    case Msi: {
        uint8_t v = 0;
        if (!ecRead(msiRow->addr, &v)) return false;
        int l = kbl_msi_level(msiRow, v);
        if (l < 0) return false;
        *level = l; return true;
    }
    case Dell: {
        uint32_t o1 = 0, o2 = 0;
        if (!dellCall(1, 0, 0, &o1, &o2)) return false;
        struct kbl_dell_state st; kbl_dell_parse_state(o1, o2, &st);
        *level = st.level > maxLevel_ ? maxLevel_ : st.level; return true;
    }
    default: return false;
    }
}

bool NMKbdBacklight::probeAsus() {
    dev = find("PNP0C14", "WMNB");
    if (!dev) return false;
    backend = Asus; maxLevel_ = KBL_ASUS_MAX_LEVEL;
    int l; return getLevel(&l);
}

bool NMKbdBacklight::probeIdeaPad() {
    dev = find("VPC2004", "KBLC");
    uint32_t q = 0;
    if (dev && evalInt("KBLC", KBL_LENOVO_QUERY_TYPE, &q) && kbl_lenovo_type(q)) {
        lenovoType = kbl_lenovo_type(q); backend = IdeaPad; maxLevel_ = kbl_lenovo_max_level(lenovoType);
        int l; return getLevel(&l);
    }
    dev = find("VPC2004", "HALS");   // a plain on/off light is read with HALS and set with SALS
    if (dev && dev->validateObject("SALS") == kIOReturnSuccess) {
        backend = IdeaPadPlain; maxLevel_ = kbl_lenovo_max_level(0);
        int l; return getLevel(&l);
    }
    return false;
}

bool NMKbdBacklight::probeThinkPad() {
    dev = find("LEN0068", "MLCG");
    if (!dev) dev = find("IBM0068", "MLCG");
    if (!dev) dev = find("LEN0268", "MLCG");
    if (!dev || dev->validateObject("MLCS") != kIOReturnSuccess) return false;
    backend = ThinkPad; maxLevel_ = KBL_THINKPAD_MAX_LEVEL;
    int l; return getLevel(&l);
}

bool NMKbdBacklight::probeDell() {
    { uint32_t on = 0; if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return false; }   // firmware is called only with nmkbl=1
    auto dict = IOService::nameMatching("PNP0C14");
    if (!dict) return false;
    auto it = IOService::getMatchingServices(dict);
    dict->release();
    if (!it) return false;
    // The SMBIOS method and the descriptor block are listed in the _WDG of the Dell WMI device(s); find the method, then ask
    // the descriptor how big a buffer the BIOS expects.
    bool found = false;
    char descObj[2] = {0, 0};
    bool haveDesc = false;
    IOACPIPlatformDevice *descDev = nullptr;
    while (auto obj = it->getNextObject()) {
        auto d = OSDynamicCast(IOACPIPlatformDevice, obj);
        OSObject *wdg = nullptr;
        if (!d || d->evaluateObject("_WDG", &wdg) != kIOReturnSuccess) continue;
        auto data = OSDynamicCast(OSData, wdg);
        if (data) {
            const uint8_t *bytes = (const uint8_t *)data->getBytesNoCopy();
            char name[2]; int flags = 0;
            if (!found && kbl_wdg_find(bytes, data->getLength(), kKblDellGuid, name, &flags) && (flags & 0x02)) {
                dev = d; found = true;
                snprintf(dellMethod, sizeof dellMethod, "WM%c%c", name[0], name[1]);
            }
            if (!haveDesc && kbl_wdg_find(bytes, data->getLength(), kKblDellDescGuid, descObj, &flags)) { haveDesc = true; descDev = d; }
        }
        if (wdg) wdg->release();
    }
    it->release();
    if (!found || !haveDesc) return false;      // without the descriptor the buffer size is unknown: refuse
    char q[8];
    snprintf(q, sizeof q, "WQ%c%c", descObj[0], descObj[1]);
    OSObject *args[1] = { OSNumber::withNumber(0ULL, 32) };
    OSObject *res = nullptr;
    IOReturn rc = args[0] ? descDev->evaluateObject(q, &res, args, 1) : kIOReturnNoMemory;
    if (args[0]) args[0]->release();
    auto block = OSDynamicCast(OSData, res);
    uint32_t size = 0, hotfix = 0;
    bool ok = rc == kIOReturnSuccess && block && kbl_dell_parse_descriptor((const uint8_t *)block->getBytesNoCopy(), block->getLength(), &size, &hotfix);
    if (res) res->release();
    if (!ok) return false;
    dellBuf = size;
    backend = Dell;
    uint32_t o1 = 0, o2 = 0;
    if (!dellCall(0, 0, 0, &o1, &o2)) return false;
    maxLevel_ = kbl_dell_info_levels(o2);
    return maxLevel_ > 0;
}

IOService *NMKbdBacklight::probe(IOService *provider, SInt32 *score) {
    if (!IOService::probe(provider, score)) return nullptr;
    uint32_t on = 0;
    if (!PE_parse_boot_argn("nmkbl", &on, sizeof on) || on != 1) return nullptr;   // off unless asked for

    // Two plugins must not own LKSB: stand aside when SMCLightSensor has registered it (unless forced).
    uint32_t force = 0;
    PE_parse_boot_argn("nmkblforce", &force, sizeof force);
    int sensor = 0, ali = 0;
    if (auto sd = IOService::nameMatching("SMCLightSensor")) { auto it = IOService::getMatchingServices(sd); sd->release(); if (it) { sensor = it->getNextObject() != nullptr; it->release(); } }
    ali = find("ACPI0008", "_ALI") != nullptr;
    forced_ = force == 1;
    skipSmc_ = kbl_should_decline(sensor, ali, forced_);   // only the SMC keys are given up; the HID route does not collide
    if (skipSmc_) SYSLOG("nmkbl", "SMCLightSensor owns LKSB/LKSS on this machine; not registering the SMC keys (nmkblforce=1 registers anyway)");

    if (!probeAsus() && !probeIdeaPad() && !probeThinkPad() && !probeDell() && !probeMsi()) { SYSLOG("nmkbl", "no supported keyboard backlight interface found"); dev = nullptr; return nullptr; }
    DBGLOG("nmkbl", "backend %d, max level %d", (int)backend, maxLevel_);

    setProperty("VersionInfo", kextVersion);
    if (!skipSmc_) vsmcNotifier = VirtualSMCAPI::registerHandler(vsmcNotificationHandler, this);
    return this;
}

bool NMKbdBacklight::start(IOService *provider) {
    if (!IOService::start(provider)) return false;
    uint32_t hid = 1;
    PE_parse_boot_argn("nmkblhid", &hid, sizeof hid);   // the HID route is on with nmkbl=1; nmkblhid=0 turns it off
    if (hid && backend != None) {
        auto d = OSTypeAlloc(NMKbdBacklightHID);
        if (d && d->init(this) && d->attach(this) && d->start(this)) { d->registerService(); hidDevice = d; DBGLOG("nmkbl", "virtual HID keyboard backlight published"); }
        else { SYSLOG("nmkbl", "the virtual HID keyboard could not be published"); if (d) d->release(); }
    }
    return true;
}

bool NMKbdBacklight::vsmcNotificationHandler(void *sensors, void *refCon, IOService *vsmc, IONotifier *notifier) {
    if (!sensors || !vsmc) return false;
    auto self = static_cast<NMKbdBacklight *>(sensors);
    // SMCLightSensor may have loaded since probe: check again so the answer does not depend on which plugin got here first
    if (!self->forced_) {
        int sensor = 0;
        if (auto sd = IOService::nameMatching("SMCLightSensor")) { auto it = IOService::getMatchingServices(sd); sd->release(); if (it) { sensor = it->getNextObject() != nullptr; it->release(); } }
        if (sensor && self->find("ACPI0008", "_ALI")) { SYSLOG("nmkbl", "SMCLightSensor loaded first; not registering the SMC keys"); return false; }
    }
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
