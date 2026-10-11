/*
 * PCI services for the port on IOPCIDevice. Compiled with Apple's headers only.
 *
 * Interrupts: each vector is an IOInterruptEventSource on a work loop of its own, so the driver's handler runs on a
 * kernel thread (Linux "hard irq" handlers in amdgpu only read the interrupt ring and schedule work, which is safe
 * there, and the port's locks may be taken). Message-signalled vectors are preferred, as on Linux.
 */
#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/IOInterruptEventSource.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOKitKeys.h>
#include "nmx_xnu.h"

extern "C" uint32_t nmx_xnu_pci_read(void *pci, int reg, int width)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    if (d == nullptr)   /* reads like a device that is not there */
        return width == 1 ? 0xff : width == 2 ? 0xffff : 0xffffffff;
    switch (width) {
    case 1: return d->extendedConfigRead8((IOByteCount)reg);
    case 2: return d->extendedConfigRead16((IOByteCount)reg);
    default: return d->extendedConfigRead32((IOByteCount)reg);
    }
}

extern "C" void nmx_xnu_pci_write(void *pci, int reg, uint32_t val, int width)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    if (d == nullptr)
        return;
    switch (width) {
    case 1: d->extendedConfigWrite8((IOByteCount)reg, (UInt8)val); break;
    case 2: d->extendedConfigWrite16((IOByteCount)reg, (UInt16)val); break;
    default: d->extendedConfigWrite32((IOByteCount)reg, val); break;
    }
}

extern "C" void nmx_xnu_pci_location(void *pci, uint32_t *domain, uint8_t *bus, uint8_t *dev, uint8_t *func)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    *domain = 0;
    if (d == nullptr) {
        *bus = *dev = *func = 0xff;
        return;
    }
    *bus = d->getBusNumber();
    *dev = d->getDeviceNumber();
    *func = d->getFunctionNumber();
}

static IOOptionBits cache_option(int cache)
{
    switch (cache) {
    case NMX_CACHE_UC: return kIOMapInhibitCache;
    case NMX_CACHE_WC: return kIOMapWriteCombineCache;
    case NMX_CACHE_WT: return kIOMapWriteThruCache;
    default: return kIOMapDefaultCache;
    }
}

extern "C" void *nmx_xnu_pci_map_bar(void *pci, int bar_reg, int cache, uint64_t *phys, uint64_t *size, void **handle)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    *handle = nullptr;
    if (d == nullptr)
        return nullptr;
    IODeviceMemory *mem = d->getDeviceMemoryWithRegister((UInt8)bar_reg);
    if (mem == nullptr)
        return nullptr;
    IOMemoryMap *map = mem->createMappingInTask(kernel_task, 0, kIOMapAnywhere | cache_option(cache));
    if (map == nullptr)
        return nullptr;
    *phys = map->getPhysicalAddress();
    *size = map->getLength();
    *handle = map;
    return (void *)(uintptr_t)map->getVirtualAddress();
}

extern "C" int nmx_xnu_pci_bar_info(void *pci, int bar_reg, uint64_t *phys, uint64_t *size, int *is_io)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    if (d == nullptr)
        return 0;
    IODeviceMemory *mem = d->getDeviceMemoryWithRegister((UInt8)bar_reg);
    if (mem == nullptr)
        return 0;
    *phys = mem->getPhysicalAddress();
    *size = mem->getLength();
    *is_io = (d->configRead32((UInt8)bar_reg) & 1) != 0;
    return 1;
}

/* ---- interrupts --------------------------------------------------------------------------------------------------- */

/* the owner of an interrupt event source must be an OSObject; this one carries the driver's handler */
class NMXIrq : public OSObject {
    OSDeclareDefaultStructors(NMXIrq)
public:
    IOWorkLoop *wl;
    IOInterruptEventSource *es;
    void (*handler)(void *);
    void *arg;
    bool initIrq() { return init(); }
};
OSDefineMetaClassAndStructors(NMXIrq, OSObject)

static void irq_action(OSObject *owner, IOInterruptEventSource *src, int count)
{
    NMXIrq *irq = OSDynamicCast(NMXIrq, owner);
    if (irq)
        irq->handler(irq->arg);
}

/* the IOKit interrupt index of the n-th message-signalled vector (vector >= 0) or of the legacy line (vector < 0) */
static int interrupt_index(IOPCIDevice *d, int vector)
{
    int n = 0, legacy = -1;
    for (int index = 0; ; index++) {
        int type = 0;
        if (d->getInterruptType(index, &type) != kIOReturnSuccess)
            break;
        if (type & kIOInterruptTypePCIMessaged) {
            if (n == vector)
                return index;
            n++;
        } else if (legacy < 0) {
            legacy = index;
        }
    }
    return vector < 0 ? legacy : -1;
}

extern "C" int nmx_xnu_pci_msi_count(void *pci)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    if (d == nullptr)
        return 0;
    int n = 0;
    for (int index = 0; ; index++) {
        int type = 0;
        if (d->getInterruptType(index, &type) != kIOReturnSuccess)
            break;
        if (type & kIOInterruptTypePCIMessaged)
            n++;
    }
    return n;
}

extern "C" int nmx_xnu_pci_irq_setup(void *pci, int vector, void (*handler)(void *), void *arg, void **cookie)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    *cookie = nullptr;
    if (d == nullptr)
        return kIOReturnNotFound;
    int index = interrupt_index(d, vector);
    if (index < 0)
        return kIOReturnNotFound;
    NMXIrq *irq = OSTypeAlloc(NMXIrq);
    if (irq == nullptr || !irq->initIrq()) {
        if (irq)
            irq->release();
        return kIOReturnNoMemory;
    }
    irq->handler = handler;
    irq->arg = arg;
    irq->wl = IOWorkLoop::workLoop();
    if (irq->wl == nullptr) {
        irq->release();
        return kIOReturnNoMemory;
    }
    irq->es = IOInterruptEventSource::interruptEventSource(irq, irq_action, d, index);
    if (irq->es == nullptr || irq->wl->addEventSource(irq->es) != kIOReturnSuccess) {
        if (irq->es)
            irq->es->release();
        irq->wl->release();
        irq->release();
        return kIOReturnError;
    }
    irq->es->enable();
    *cookie = irq;
    return 0;
}

static IOReturn drain_action(OSObject *, void *, void *, void *, void *)
{
    return kIOReturnSuccess;
}

/* the work loop gate is held while a handler runs, so taking it once waits for that handler */
extern "C" void nmx_xnu_pci_irq_drain(void *cookie)
{
    NMXIrq *irq = (NMXIrq *)cookie;
    if (irq != nullptr && irq->wl != nullptr)
        irq->wl->runAction(drain_action, irq);
}

extern "C" void nmx_xnu_pci_irq_enable(void *cookie, int on)
{
    NMXIrq *irq = (NMXIrq *)cookie;
    if (irq == nullptr)
        return;
    if (on)
        irq->es->enable();
    else
        irq->es->disable();
}

extern "C" void nmx_xnu_pci_irq_teardown(void *cookie)
{
    NMXIrq *irq = (NMXIrq *)cookie;
    if (irq == nullptr)
        return;
    irq->es->disable();
    irq->wl->removeEventSource(irq->es);
    irq->es->release();
    irq->wl->release();
    irq->release();
}

/* ---- lookup and ROM ----------------------------------------------------------------------------------------------- */

extern "C" void *nmx_xnu_pci_find(uint32_t domain, uint8_t bus, uint8_t dev, uint8_t func)
{
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (match == nullptr)
        return nullptr;
    IOPCIDevice *found = nullptr;
    OSIterator *it = IOService::getMatchingServices(match);
    match->release();
    if (it == nullptr)
        return nullptr;
    while (OSObject *o = it->getNextObject()) {
        IOPCIDevice *d = OSDynamicCast(IOPCIDevice, o);
        if (d && d->getBusNumber() == bus && d->getDeviceNumber() == dev && d->getFunctionNumber() == func) {
            found = d;
            break;
        }
    }
    it->release();
    return found;
}

/*
 * The expansion ROM: the platform firmware (and OpenCore) publish the image of a GPU's option ROM as the
 * "vbios" / "ATY,bin_image" properties on some systems; otherwise the ROM BAR is read with decoding enabled briefly.
 */
extern "C" size_t nmx_xnu_pci_rom(void *pci, void *buf, size_t max)
{
    IOPCIDevice *d = (IOPCIDevice *)pci;
    if (d == nullptr || buf == nullptr)
        return 0;
    static const char *const props[] = { "vbios", "ATY,bin_image", "rom", nullptr };
    for (int i = 0; props[i]; i++) {
        OSData *data = OSDynamicCast(OSData, d->getProperty(props[i]));
        if (data && data->getLength() > 0) {
            size_t n = data->getLength() < max ? data->getLength() : max;
            memcpy(buf, data->getBytesNoCopy(), n);
            return n;
        }
    }
    uint32_t rombar = d->configRead32(kIOPCIConfigExpansionROMBase);
    uint64_t base = rombar & ~0x7ffULL;
    if (base == 0)
        return 0;
    d->configWrite32(kIOPCIConfigExpansionROMBase, rombar | 1);
    void *h = nullptr;
    void *va = nmx_xnu_map_phys(base, max, NMX_CACHE_UC, &h);
    size_t n = 0;
    if (va) {
        const uint8_t *p = (const uint8_t *)va;
        if (p[0] == 0x55 && p[1] == 0xaa) {
            n = (size_t)p[2] * 512;
            if (n == 0 || n > max)
                n = max;
            memcpy(buf, va, n);
        }
        nmx_xnu_unmap(h);
    }
    d->configWrite32(kIOPCIConfigExpansionROMBase, rombar);
    return n;
}
