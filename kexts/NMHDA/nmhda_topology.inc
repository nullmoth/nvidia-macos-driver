#include "nmhda_topology.h"

typedef IOReturn (*NMHDAExecuteVerb)(void *, uint16_t, uint16_t, uint16_t, unsigned *, bool);
static NMHDAExecuteVerb gTopologyVerb;
static bool topologyGet(IOService *codec, unsigned nid, unsigned verb, unsigned param, unsigned &value)
{
    value = 0xffffffff;
    return gTopologyVerb && gTopologyVerb(codec, (uint16_t)nid, (uint16_t)verb, (uint16_t)param,
        &value, true) == kIOReturnSuccess &&
        value != 0xffffffff;
}

class NMHDATopologyService : public IOService {
    OSDeclareDefaultStructors(NMHDATopologyService)
public:
    IOReturn callPlatformFunction(const OSSymbol *name, bool wait, void *p1, void *p2,
                                 void *p3, void *p4) override;
};
OSDefineMetaClassAndStructors(NMHDATopologyService, IOService)

IOReturn NMHDATopologyService::callPlatformFunction(const OSSymbol *name, bool wait,
                                                   void *p1, void *p2, void *p3, void *p4)
{
    if (!name->isEqualTo(NMHDA_TOPOLOGY_READ))
        return IOService::callPlatformFunction(name, wait, p1, p2, p3, p4);
    if (!p1 || !p2) return kIOReturnBadArgument;
    auto result = static_cast<OSData **>(p2);
    *result = nullptr;
    auto snapshot = static_cast<NMHDATopology *>(IOMalloc(sizeof(NMHDATopology)));
    if (!snapshot) return kIOReturnNoMemory;
    bzero(snapshot, sizeof *snapshot);
    snapshot->version = NMHDA_TOPOLOGY_VERSION;
    snapshot->generation = *static_cast<uint64_t *>(p1);
    snapshot->unavailable = !gTopologyVerb;
    auto controller = getProvider();
    auto it = controller ? controller->getChildIterator(gIOServicePlane) : nullptr;
    if (!it) snapshot->unavailable = true;
    while (it) {
        auto object = it->getNextObject();
        if (!object) break;
        auto codec = OSDynamicCast(IOService, object);
        if (!codec || !codec->metaCast("IOHDACodecDevice")) continue;
        unsigned vendor, groups;
        if (!topologyGet(codec, 0, 0xf00, 0, vendor) || (vendor >> 16) != 0x10de ||
            !topologyGet(codec, 0, 0xf00, 4, groups)) continue;
        auto address = OSDynamicCast(OSNumber, codec->getProperty("IOHDACodecAddress"));
        if (!address || address->unsigned32BitValue() > 15) { snapshot->unavailable = true; continue; }
        unsigned first = (groups >> 16) & 255, end = first + (groups & 255);
        if (end > 256) { snapshot->unavailable = true; continue; }
        for (unsigned fg = first; fg < end; ++fg) {
            unsigned type, nodes;
            if (!topologyGet(codec, fg, 0xf00, 5, type) || (type & 255) != 1 ||
                !topologyGet(codec, fg, 0xf00, 4, nodes)) continue;
            unsigned start = (nodes >> 16) & 255, stop = start + (nodes & 255);
            if (stop > 256) { snapshot->unavailable = true; continue; }
            for (unsigned nid = start; nid < stop; ++nid) {
                unsigned caps, pinCaps, sense, select;
                if (!topologyGet(codec, nid, 0xf00, 9, caps) || ((caps >> 20) & 15) != 4 ||
                    !(caps & 512) || !topologyGet(codec, nid, 0xf00, 12, pinCaps) ||
                    !(pinCaps & ((1u << 7) | (1u << 24)))) continue;
                if (snapshot->count == NMHDA_MAX_PINS) { snapshot->truncated = true; continue; }
                NMHDAPin &pin = snapshot->pins[snapshot->count++];
                pin.codec = (uint8_t)address->unsigned32BitValue();
                pin.function = (uint8_t)fg; pin.pin = (uint8_t)nid;
                if (!topologyGet(codec, nid, 0xf09, 0, sense)) continue;
                pin.sense = sense;
                // Do not change MST device selection; only device zero can describe an SST sink.
                if (!topologyGet(codec, nid, 0xf35, 0, select) || select != 0 ||
                    (sense & 0xc0000000u) != 0xc0000000u) continue;
                unsigned size = 4;
                for (unsigned byte = 0; byte < size; ++byte) {
                    unsigned value;
                    if (!topologyGet(codec, nid, 0xf2f, byte, value) || !(value & 0x80000000u)) break;
                    pin.eld[byte] = value & 255;
                    if (byte == 2) {
                        size = 4 + (value & 255) * 4;
                        if (size > NMHDA_ELD_BYTES || size < 20) break;
                    }
                    if (byte + 1 == size) pin.eldSize = size;
                }
                unsigned after;
                if (!topologyGet(codec, nid, 0xf09, 0, after) || after != sense) pin.eldSize = 0;
                // ELD validity is per pin. Keep this topology usable so another connected SST pin can still match;
                // NVRMFB will retry after the modeset that commits ELD for this display.
            }
        }
    }
    if (it) it->release();
    *result = OSData::withBytes(snapshot, sizeof *snapshot);
    IOFree(snapshot, sizeof *snapshot);
    return *result ? kIOReturnSuccess : kIOReturnNoMemory;
}

static void startTopologyService(IOService *controller)
{
    if (controller->getProperty("NMHDATopologyPublished")) return;
    auto service = new NMHDATopologyService;
    if (!service) return;
    if (service->init() && service->attach(controller)) {
        if (service->start(controller)) {
            controller->setProperty("NMHDATopologyPublished", true);
            service->registerService(kIOServiceAsynchronous);
        } else service->detach(controller);
    }
    service->release();
}
