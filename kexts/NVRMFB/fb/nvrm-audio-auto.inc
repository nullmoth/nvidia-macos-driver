// Kernel extensions still use IOPCIDevice; PCIDriverKit is a DriverKit API.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/pci/IOPCIDevice.h>
static IOPCIDevice *audioGpu(IORegistryEntry *entry)
{
    for (; entry; entry = entry->getParentEntry(gIOServicePlane)) {
        auto pci = OSDynamicCast(IOPCIDevice, entry);
        if (pci && (pci->configRead32(kIOPCIConfigClassCode) >> 24) == 3) return pci;
    }
    return nullptr;
}

static IOService *audioTopologyService(IORegistryEntry *entry)
{
    auto gpu = audioGpu(entry);
    if (!gpu) return nullptr;
    OSDictionary *match = IOService::serviceMatching(NMHDA_TOPOLOGY_SERVICE);
    if (!match) return nullptr;
    OSIterator *it = IOService::getMatchingServices(match);
    match->release();
    if (!it) return nullptr;
    IOService *found = nullptr;
    while (auto object = it->getNextObject()) {
        auto service = OSDynamicCast(IOService, object);
        auto controller = service ? service->getProvider() : nullptr;
        auto hda = controller ? OSDynamicCast(IOPCIDevice, controller->getProvider()) : nullptr;
        if (hda && hda->getBusNumber() == gpu->getBusNumber() &&
            hda->getDeviceNumber() == gpu->getDeviceNumber() &&
            hda->getParentEntry(gIOServicePlane) == gpu->getParentEntry(gIOServicePlane)) {
            service->retain(); found = service; break;
        }
    }
    it->release();
    return found;
}

static bool audioCodecValid(OSData *data)
{
    if (!data || data->getLength() != 4) return false;
    auto bytes = static_cast<const uint8_t *>(data->getBytesNoCopy());
    return nvrm_audio_codec_valid(bytes, data->getLength());
}

void NVRMNVDAFramebuffer::publishDisplayAudioProperties()
{
    NVRMDisplayGate gate;
    clearDisplayAudioProperties();
    fAudioAttempt = 0;
    if (fInternal || fDpyLost || fEdidSize < 128 || OSAddAtomic(0, &fStopping)) return;
    if (!fAudioCall) fAudioCall = nvrmfbCall(audioFire, this);
    if (!fAudioReady) {
        OSDictionary *match = IOService::serviceMatching(NMHDA_TOPOLOGY_SERVICE);
        if (match) { fAudioReady = IOService::addMatchingNotification(gIOFirstMatchNotification,
            match, audioReady, this, nullptr); match->release(); }
    }
    if (!fCodecReady) {
        OSDictionary *match = IOService::serviceMatching("IOHDACodecDevice");
        if (match) { fCodecReady = IOService::addMatchingNotification(gIOFirstMatchNotification,
            match, audioReady, this, nullptr); match->release(); }
    }
    if (fAudioCall) scheduleCall(fAudioCall, 0);
}

void NVRMNVDAFramebuffer::refreshDisplayAudioProperties()
{
    NVRMDisplayGate gate;
    if (fInternal || fDpyLost || fEdidSize < 128 || OSAddAtomic(0, &fStopping)) return;
    if (!fAudioCall) fAudioCall = nvrmfbCall(audioFire, this);
    if (!fAudioCall) return;
    ++fAudioGeneration;
    fAudioAttempt = 0;
    scheduleCall(fAudioCall, 0);
}

bool NVRMNVDAFramebuffer::audioReady(void *target, void *, IOService *, IONotifier *)
{
    auto me = static_cast<NVRMNVDAFramebuffer *>(target);
    NVRMDisplayGate gate;
    if (me->fAudioCall && !OSAddAtomic(0, &me->fStopping)) {
        ++me->fAudioGeneration; me->fAudioAttempt = 0;
        me->scheduleCall(me->fAudioCall, 0);
    }
    return true;
}

void NVRMNVDAFramebuffer::audioFire(thread_call_param_t p0, thread_call_param_t)
{
    auto me = static_cast<NVRMNVDAFramebuffer *>(p0);
    NVRMCallReference reference(me);
    uint64_t generation;
    {
        NVRMDisplayGate gate;
        if (OSAddAtomic(0, &me->fStopping) || me->fDpyLost) return;
        generation = me->fAudioGeneration;
    }
    // HDA may wait for its work loop. Never hold the display gate across this call.
    OSData *data = nullptr;
    auto service = audioTopologyService(me->getProvider());
    if (service) {
        const OSSymbol *name = OSSymbol::withCString(NMHDA_TOPOLOGY_READ);
        if (name) { service->callPlatformFunction(name, false, &generation, &data, nullptr, nullptr); name->release(); }
        service->release();
    }
    bool notify = false;
    {
        NVRMDisplayGate gate;
        if (OSAddAtomic(0, &me->fStopping) || me->fDpyLost || generation != me->fAudioGeneration ||
            me->fIndex >= (unsigned)gNDpy) { OSSafeReleaseNULL(data); return; }
        auto &display = gDpy[me->fIndex];
        NvKmsKapiConnectorInfo connector = {};
        bool haveConnector = me->fKms->getConnectorInfo(me->fDev, me->fConnector, &connector);
        unsigned external = 0;
        for (int i = 0; i < gNDpy; ++i)
            if (gDpy[i].connected && !gDpy[i].internal) ++external;
        OSData *explicitCodec = nullptr;
        bool error = false;
        OSObject *overrideObject = me->copyProperty("NVRMFBDisplayAudioOverrides");
        if (overrideObject) {
            auto entries = OSDynamicCast(OSArray, overrideObject);
            if (!entries || !haveConnector) error = true;
            for (unsigned i = 0; entries && i < entries->getCount(); ++i) {
                auto item = OSDynamicCast(OSDictionary, entries->getObject(i));
                auto index = item ? OSDynamicCast(OSNumber, item->getObject("PhysicalIndex")) : nullptr;
                auto codec = item ? OSDynamicCast(OSData, item->getObject("CodecInfo")) : nullptr;
                if (!index || index->unsigned64BitValue() > 0xffffffffu || !audioCodecValid(codec)) { error = true; break; }
                for (unsigned j = 0; j < i; ++j) {
                    auto old = OSDynamicCast(OSDictionary, entries->getObject(j));
                    auto oldIndex = old ? OSDynamicCast(OSNumber, old->getObject("PhysicalIndex")) : nullptr;
                    auto oldCodec = old ? OSDynamicCast(OSData, old->getObject("CodecInfo")) : nullptr;
                    if (oldIndex && (oldIndex->unsigned64BitValue() == index->unsigned64BitValue() ||
                        (oldCodec && oldCodec->isEqualTo(codec)))) error = true;
                }
                if (index->unsigned64BitValue() == connector.physicalIndex) explicitCodec = codec;
            }
        }
        OSObject *legacy = external == 1 ? me->copyProperty("NVRMFBDisplayAudioCodecInfo") : nullptr;
        if (!explicitCodec && legacy) {
            explicitCodec = OSDynamicCast(OSData, legacy);
            if (!audioCodecValid(explicitCodec)) error = true;
        }
        const char *source = error ? "invalid-configuration" : "unmatched";
        uint8_t chosen[4] = {};
        bool valid = false;
        if (!error && explicitCodec) {
            memcpy(chosen, explicitCodec->getBytesNoCopy(), 4);
            valid = true; source = legacy == explicitCodec ? "legacy-override" : "connector-override";
        }
        const NMHDATopology *topology = data && data->getLength() == sizeof(NMHDATopology) ?
            static_cast<const NMHDATopology *>(data->getBytesNoCopy()) : nullptr;
        if (topology && !nvrm_audio_snapshot_current(topology->version, topology->generation, generation,
            topology->unavailable, topology->truncated, topology->count)) topology = nullptr;
        if (!valid && !error && !display.mst && topology) {
            struct Identities {
                nvrm_audio_identity displays[NVKMS_KAPI_MAX_CONNECTORS * 2];
                nvrm_audio_identity pins[NMHDA_MAX_PINS];
            };
            auto identities = static_cast<Identities *>(IOMalloc(sizeof(Identities)));
            if (identities) {
            bzero(identities, sizeof *identities);
            auto displays = identities->displays;
            auto pins = identities->pins;
            for (int i = 0; i < gNDpy; ++i) {
                auto &d = gDpy[i];
                uint8_t eld[NVRM_ELD_MAX];
                if (!d.connected || d.internal || d.mst) continue;
                size_t size = nvrm_build_eld(d.edid, d.edidSize,
                    d.hdaConnectorType == 0x800 ? NVRM_ELD_CONN_HDMI : NVRM_ELD_CONN_DP, eld);
                nvrm_audio_identity_parse(eld, size, displays + i);
            }
            for (unsigned i = 0; i < topology->count; ++i) {
                auto &pin = topology->pins[i];
                bool reserved = false;
                auto entries = OSDynamicCast(OSArray, overrideObject);
                for (unsigned j = 0; entries && j < entries->getCount(); ++j) {
                    auto item = OSDynamicCast(OSDictionary, entries->getObject(j));
                    auto value = item ? OSDynamicCast(OSData, item->getObject("CodecInfo")) : nullptr;
                    if (!audioCodecValid(value)) continue;
                    auto bytes = static_cast<const uint8_t *>(value->getBytesNoCopy());
                    if (bytes[0] == pin.codec && bytes[1] == pin.function && bytes[2] == pin.pin) reserved = true;
                }
                if (reserved) continue;
                if ((pin.sense & 0xc0000000u) == 0xc0000000u && pin.eldSize <= NMHDA_ELD_BYTES)
                    nvrm_audio_identity_parse(pin.eld, pin.eldSize, pins + i);
            }
            int pin = nvrm_audio_unique_match(displays, (size_t)gNDpy, me->fIndex, pins, topology->count);
            if (pin >= 0) {
                chosen[0] = topology->pins[pin].codec; chosen[1] = topology->pins[pin].function;
                chosen[2] = topology->pins[pin].pin; valid = true; source = "automatic";
            }
            IOFree(identities, sizeof *identities);
            }
        }
        if (!valid && nvrm_audio_fallback_allowed(external, display.mst, error)) {
            chosen[1] = 1; chosen[2] = 4; valid = true; source = "fallback-04";
        }
        if (!valid && !error && display.mst) source = "mst-unsupported";
        bool changed = me->fAudioValid != valid || memcmp(me->fAudioCodec, chosen, 4);
        me->fAudioValid = valid; memcpy(me->fAudioCodec, chosen, 4);
        me->setProperty("NVRMFBDisplayAudioSource", source);
        me->setProperty("NVRMFBDisplayAudioProbe", topology ? "available" : "unavailable");
        me->setProperty("NVRMFBDisplayAudioGeneration", generation, 64);
        if (haveConnector) me->setProperty("NVRMFBDisplayAudioPhysicalIndex", connector.physicalIndex, 32);
        me->publishDisplayAudioPropertiesNow();
        if (changed) FBLOG("fb%u dpy 0x%x connector 0x%x audio %s codec %u fg %u pin 0x%x",
            me->fIndex, me->fDisplay, me->fConnector, source, chosen[0], chosen[1], chosen[2]);
        notify = changed;
        if (!error && !explicitCodec && !display.mst && strcmp(source, "automatic") != 0 &&
            me->fAudioAttempt++ < 6) {
            uint64_t deadline; clock_interval_to_deadline(300, kMillisecondScale, &deadline);
            me->scheduleCall(me->fAudioCall, deadline);
        }
        OSSafeReleaseNULL(legacy); OSSafeReleaseNULL(overrideObject);
    }
    OSSafeReleaseNULL(data);
    if (notify) me->notifyConnectionChanged(0);
}
#pragma clang diagnostic pop
