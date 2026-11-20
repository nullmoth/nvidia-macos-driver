/*
 * PCI services adapted from NullMoth's public XNU bridge
 * gpu-port/xkpi/xnu/nmx_pci.cpp (commit c104f584b71515c8e30855e9048a902a012c3131).
 * This ABI-specific copy adds the current find_class entry point and bounds
 * input/enumeration. It uses IOKit's assigned PCI resources and interrupt
 * sources; it does not size BARs by writing them or program MSI registers.
 */
#include <stdint.h>
#include <stddef.h>

#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/IOInterruptEventSource.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOKitKeys.h>
#include <libkern/c++/OSIterator.h>
#include <libkern/c++/OSObject.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSNumber.h>
#include "nmx_xnu.h"

static constexpr uint32_t kNmxPCIConfigBytes = 4096;
static constexpr uint32_t kNmxPCIScanLimit = 4096;
static constexpr uint32_t kNmxInterruptScanLimit = 256;

static IOPCIDevice *nmx_pci_cast(void *opaque)
{
    return OSDynamicCast(IOPCIDevice, (OSObject *)opaque);
}

static bool nmx_pci_config_access_valid(int reg, int width)
{
    if (reg < 0 || (width != 1 && width != 2 && width != 4))
        return false;
    uint32_t offset = (uint32_t)reg;
    return offset < kNmxPCIConfigBytes && (uint32_t)width <= kNmxPCIConfigBytes - offset &&
           (offset & ((uint32_t)width - 1)) == 0;
}

static bool nmx_pci_valid_bar_reg(int barReg)
{
    return barReg >= 0x10 && barReg <= 0x24 && (barReg & 3) == 0;
}

static bool nmx_pci_domain_property(IORegistryEntry *entry, uint32_t *domain)
{
    static const char *const keys[] = {"pci-domain", "IOPCIDomain", "AAPL,segment"};
    for (unsigned i = 0; i < sizeof(keys) / sizeof(keys[0]); ++i) {
        OSNumber *number = OSDynamicCast(OSNumber, entry->getProperty(keys[i]));
        if (number != nullptr) {
            uint64_t value = number->unsigned64BitValue();
            if (value > UINT32_MAX)
                return false;
            *domain = (uint32_t)value;
            return true;
        }
    }
    return false;
}

/* IOPCIDevice has no public segment accessor. Apple x86 PCI machines use
 * segment zero when the registry tree does not publish an explicit segment. */
static uint32_t nmx_pci_domain(IOPCIDevice *pci)
{
    IORegistryEntry *entry = pci;
    for (unsigned depth = 0; entry != nullptr && depth < 16; ++depth) {
        uint32_t domain = 0;
        if (nmx_pci_domain_property(entry, &domain))
            return domain;
        entry = entry->getParentEntry(gIOServicePlane);
    }
    return 0;
}

static bool nmx_pci_class_matches(IOPCIDevice *pci, uint8_t cls, int sub)
{
    if (pci->extendedConfigRead8(0x0b) != cls)
        return false;
    return sub < 0 || sub > 0xff || pci->extendedConfigRead8(0x0a) == (uint8_t)sub;
}

static IOOptionBits nmx_pci_cache_option(int cache)
{
    switch (cache) {
    case NMX_CACHE_UC: return kIOMapInhibitCache;
    case NMX_CACHE_WC: return kIOMapWriteCombineCache;
    case NMX_CACHE_WT: return kIOMapWriteThruCache;
    case NMX_CACHE_WB: return kIOMapCopybackCache;
    default: return 0xffffffffU;
    }
}

extern "C" uint32_t nmx_xnu_pci_read(void *opaque, int reg, int width)
{
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || !nmx_pci_config_access_valid(reg, width))
        return width == 1 ? 0xffU : width == 2 ? 0xffffU : 0xffffffffU;
    switch (width) {
    case 1: return pci->extendedConfigRead8((IOByteCount)reg);
    case 2: return pci->extendedConfigRead16((IOByteCount)reg);
    case 4: return pci->extendedConfigRead32((IOByteCount)reg);
    default: return 0xffffffffU;
    }
}

extern "C" void nmx_xnu_pci_write(void *opaque, int reg, uint32_t value, int width)
{
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || !nmx_pci_config_access_valid(reg, width))
        return;
    switch (width) {
    case 1: pci->extendedConfigWrite8((IOByteCount)reg, (UInt8)value); break;
    case 2: pci->extendedConfigWrite16((IOByteCount)reg, (UInt16)value); break;
    case 4: pci->extendedConfigWrite32((IOByteCount)reg, (UInt32)value); break;
    }
}

extern "C" void nmx_xnu_pci_location(void *opaque, uint32_t *domain, uint8_t *bus, uint8_t *dev, uint8_t *func)
{
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (domain != nullptr) *domain = pci == nullptr ? 0 : nmx_pci_domain(pci);
    if (pci == nullptr) {
        if (bus != nullptr) *bus = 0xff;
        if (dev != nullptr) *dev = 0xff;
        if (func != nullptr) *func = 0xff;
        return;
    }
    if (bus != nullptr) *bus = pci->getBusNumber();
    if (dev != nullptr) *dev = pci->getDeviceNumber();
    if (func != nullptr) *func = pci->getFunctionNumber();
}

extern "C" void *nmx_xnu_pci_map_bar(void *opaque, int barReg, int cache,
                                      uint64_t *phys, uint64_t *size, void **handle)
{
    if (phys != nullptr) *phys = 0;
    if (size != nullptr) *size = 0;
    if (handle != nullptr) *handle = nullptr;
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || !nmx_pci_valid_bar_reg(barReg) || handle == nullptr)
        return nullptr;
    IOOptionBits cacheMode = nmx_pci_cache_option(cache);
    if (cacheMode == 0xffffffffU)
        return nullptr;

    IODeviceMemory *mem = pci->getDeviceMemoryWithRegister((UInt8)barReg);
    if (mem == nullptr || mem->getLength() == 0)
        return nullptr;
    IOMemoryMap *map = mem->createMappingInTask(kernel_task, 0, kIOMapAnywhere | cacheMode);
    if (map == nullptr || map->getVirtualAddress() == 0) {
        if (map != nullptr) map->release();
        return nullptr;
    }
    if (phys != nullptr) *phys = (uint64_t)map->getPhysicalAddress();
    if (size != nullptr) *size = (uint64_t)map->getLength();
    *handle = map; /* released by nmx_xnu_unmap */
    return (void *)(uintptr_t)map->getVirtualAddress();
}

extern "C" int nmx_xnu_pci_bar_info(void *opaque, int barReg, uint64_t *phys, uint64_t *size, int *isIO)
{
    if (phys != nullptr) *phys = 0;
    if (size != nullptr) *size = 0;
    if (isIO != nullptr) *isIO = 0;
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || !nmx_pci_valid_bar_reg(barReg))
        return 0;
    uint32_t bar = pci->extendedConfigRead32((IOByteCount)barReg);
    if (bar == 0 || bar == 0xffffffffU)
        return 0;
    IODeviceMemory *mem = pci->getDeviceMemoryWithRegister((UInt8)barReg);
    if (mem == nullptr || mem->getLength() == 0)
        return 0;
    if (phys != nullptr) *phys = (uint64_t)mem->getPhysicalAddress();
    if (size != nullptr) *size = (uint64_t)mem->getLength();
    if (isIO != nullptr) *isIO = (bar & 1U) != 0;
    return 1;
}

/* Each vector is an IOInterruptEventSource on its own work loop. The OSObject
 * owner carries the callback across the IOKit C++ event-source boundary. */
class NMXIrq : public OSObject {
    OSDeclareDefaultStructors(NMXIrq)
public:
    IOPCIDevice *pci;
    IOWorkLoop *wl;
    IOInterruptEventSource *es;
    void (*handler)(void *);
    void *arg;
    int sourceIndex;
    bool initialized;

    bool initIrq()
    {
        if (!init())
            return false;
        pci = nullptr;
        wl = nullptr;
        es = nullptr;
        handler = nullptr;
        arg = nullptr;
        sourceIndex = -1;
        initialized = true;
        return true;
    }

    void cleanup()
    {
        if (!initialized)
            return;
        if (es != nullptr) {
            es->disable();
            if (pci != nullptr && sourceIndex >= 0)
                (void)pci->disableInterrupt(sourceIndex);
            if (wl != nullptr)
                (void)wl->runAction(&NMXIrq::barrierAction, this);
            if (wl != nullptr)
                (void)wl->removeEventSource(es);
            es->release();
            es = nullptr;
        }
        if (wl != nullptr) {
            wl->release();
            wl = nullptr;
        }
        if (pci != nullptr) {
            pci->release();
            pci = nullptr;
        }
        handler = nullptr;
        arg = nullptr;
        initialized = false;
    }

    static void action(OSObject *owner, IOInterruptEventSource *, int)
    {
        NMXIrq *irq = OSDynamicCast(NMXIrq, owner);
        if (irq != nullptr && irq->handler != nullptr)
            irq->handler(irq->arg);
    }

    static IOReturn barrierAction(OSObject *, void *, void *, void *, void *)
    {
        return kIOReturnSuccess;
    }

protected:
    void free() APPLE_KEXT_OVERRIDE
    {
        cleanup();
        OSObject::free();
    }
};
OSDefineMetaClassAndStructors(NMXIrq, OSObject)

/* IOKit indexes message-signalled and legacy sources in the provider's list.
 * Translate FreeBSD's zero-based MSI vector ordinal to that source index. */
static int nmx_pci_interrupt_index(IOPCIDevice *pci, int vector)
{
    int messageOrdinal = 0;
    int legacy = -1;
    for (int index = 0; index < (int)kNmxInterruptScanLimit; ++index) {
        int type = 0;
        if (pci->getInterruptType(index, &type) != kIOReturnSuccess)
            break;
        if (((UInt32)type & (kIOInterruptTypePCIMessaged | kIOInterruptTypePCIMessagedX)) != 0) {
            if (vector >= 0 && messageOrdinal == vector)
                return index;
            ++messageOrdinal;
        } else if (legacy < 0) {
            legacy = index;
        }
    }
    return vector < 0 ? legacy : -1;
}

extern "C" int nmx_xnu_pci_msi_count(void *opaque)
{
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr)
        return 0;
    int count = 0;
    for (int index = 0; index < (int)kNmxInterruptScanLimit; ++index) {
        int type = 0;
        if (pci->getInterruptType(index, &type) != kIOReturnSuccess)
            break;
        if (((UInt32)type & (kIOInterruptTypePCIMessaged | kIOInterruptTypePCIMessagedX)) != 0)
            ++count;
    }
    return count;
}

extern "C" int nmx_xnu_pci_irq_setup(void *opaque, int vector, void (*handler)(void *), void *arg, void **cookie)
{
    if (cookie != nullptr) *cookie = nullptr;
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || cookie == nullptr || handler == nullptr || vector < -1)
        return kIOReturnBadArgument;
    int index = nmx_pci_interrupt_index(pci, vector);
    if (index < 0)
        return kIOReturnNotFound;

    NMXIrq *irq = OSTypeAlloc(NMXIrq);
    if (irq == nullptr || !irq->initIrq()) {
        if (irq != nullptr) irq->release();
        return kIOReturnNoMemory;
    }
    irq->handler = handler;
    irq->arg = arg;
    irq->sourceIndex = index;
    irq->pci = pci;
    pci->retain();
    irq->wl = IOWorkLoop::workLoop();
    if (irq->wl == nullptr) {
        irq->cleanup();
        irq->release();
        return kIOReturnNoMemory;
    }
    irq->es = IOInterruptEventSource::interruptEventSource(irq, NMXIrq::action, pci, index);
    if (irq->es == nullptr) {
        irq->cleanup();
        irq->release();
        return kIOReturnNoResources;
    }
    if (irq->wl->addEventSource(irq->es) != kIOReturnSuccess) {
        /* Event source owns a retain on irq after creation; release it after
         * detaching so the callback owner cannot outlive the cookie. */
        irq->es->release();
        irq->es = nullptr;
        irq->cleanup();
        irq->release();
        return kIOReturnError;
    }
    irq->es->enable();
    *cookie = irq;
    return kIOReturnSuccess;
}

extern "C" void nmx_xnu_pci_irq_drain(void *cookie)
{
    NMXIrq *irq = OSDynamicCast(NMXIrq, (OSObject *)cookie);
    if (irq != nullptr && irq->wl != nullptr)
        (void)irq->wl->runAction(&NMXIrq::barrierAction, irq);
}

extern "C" void nmx_xnu_pci_irq_enable(void *cookie, int on)
{
    NMXIrq *irq = OSDynamicCast(NMXIrq, (OSObject *)cookie);
    if (irq == nullptr || irq->es == nullptr)
        return;
    if (on)
        irq->es->enable();
    else
        irq->es->disable();
}

extern "C" void nmx_xnu_pci_irq_teardown(void *cookie)
{
    NMXIrq *irq = OSDynamicCast(NMXIrq, (OSObject *)cookie);
    if (irq == nullptr)
        return;
    irq->cleanup();
    irq->release();
}

extern "C" void *nmx_xnu_pci_find(uint32_t domain, uint8_t bus, uint8_t dev, uint8_t func)
{
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (match == nullptr)
        return nullptr;
    OSIterator *iterator = IOService::getMatchingServices(match);
    match->release();
    if (iterator == nullptr)
        return nullptr;
    IOPCIDevice *found = nullptr;
    for (uint32_t examined = 0; examined < kNmxPCIScanLimit; ++examined) {
        IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, iterator->getNextObject());
        if (pci == nullptr)
            break;
        if (pci->getBusNumber() == bus && pci->getDeviceNumber() == dev &&
            pci->getFunctionNumber() == func && nmx_pci_domain(pci) == domain) {
            found = pci;
            break;
        }
    }
    iterator->release();
    return found;
}

extern "C" void *nmx_xnu_pci_find_class(uint8_t cls, int sub, void *after)
{
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (match == nullptr)
        return nullptr;
    OSIterator *iterator = IOService::getMatchingServices(match);
    match->release();
    if (iterator == nullptr)
        return nullptr;

    bool passedAfter = after == nullptr;
    IOPCIDevice *found = nullptr;
    for (uint32_t examined = 0; examined < kNmxPCIScanLimit; ++examined) {
        IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, iterator->getNextObject());
        if (pci == nullptr)
            break;
        if (!passedAfter) {
            if (pci == after)
                passedAfter = true;
            continue;
        }
        if (nmx_pci_class_matches(pci, cls, sub)) {
            found = pci;
            break;
        }
    }
    iterator->release();
    return found;
}

/* Only firmware-published ROM data is safe in this bridge. In particular,
 * never toggle the ROM BAR or map an unassigned physical address. */
extern "C" size_t nmx_xnu_pci_rom(void *opaque, void *buf, size_t max)
{
    IOPCIDevice *pci = nmx_pci_cast(opaque);
    if (pci == nullptr || buf == nullptr || max == 0)
        return 0;
    static const char *const properties[] = {"vbios", "ATY,bin_image", "rom", nullptr};
    for (unsigned i = 0; properties[i] != nullptr; ++i) {
        OSData *data = OSDynamicCast(OSData, pci->getProperty(properties[i]));
        if (data == nullptr || data->getLength() == 0)
            continue;
        size_t length = (size_t)data->getLength();
        if (length > max)
            length = max;
        memcpy(buf, data->getBytesNoCopy(), length);
        return length;
    }
    return 0;
}
