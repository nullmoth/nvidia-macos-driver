/*
 * XNU memory and DMA bridge for the NMX port.
 *
 * This follows the public Liberator XNU bridge ABI while using only the
 * public Kernel.framework interfaces declared by Apple's SDK.  In
 * particular, kernel virtual-to-physical translation is performed through
 * IOMemoryDescriptor rather than private pmap symbols.
 */
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IODMACommand.h>
#include <IOKit/IOMapper.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <kern/task.h>
#include <mach/vm_param.h>
#include <sys/errno.h>
#include <stdint.h>
#include <stddef.h>

#include "nmx_xnu.h"

struct nmx_dma_map {
    IODMACommand *cmd;
    IOMemoryDescriptor *md;
};

static bool add_overflow_u64(uint64_t a, uint64_t b, uint64_t *out)
{
    if (a > UINT64_MAX - b)
        return true;
    *out = a + b;
    return false;
}

static bool range_last(uint64_t base, size_t size, uint64_t *last)
{
    if (size == 0 || base > UINT64_MAX - ((uint64_t)size - 1))
        return false;
    *last = base + (uint64_t)size - 1;
    return true;
}

static bool power_of_two(size_t value)
{
    return value != 0 && (value & (value - 1)) == 0;
}

static uint64_t physical_mask_for_limit(uint64_t limit)
{
    if (limit == 0 || limit == UINT64_MAX)
        return UINT64_MAX;
    /* IOKit's physicalMask is a bit mask, not a numeric ceiling. Fill the
     * lower bits through the highest set bit, then validate the allocated
     * run against the exact inclusive ceiling below. */
    limit |= limit >> 1;
    limit |= limit >> 2;
    limit |= limit >> 4;
    limit |= limit >> 8;
    limit |= limit >> 16;
    limit |= limit >> 32;
    return limit;
}

static IOOptionBits cache_option(int cache)
{
    switch (cache) {
    case NMX_CACHE_UC: return kIOMapInhibitCache;
    case NMX_CACHE_WC: return kIOMapWriteCombineCache;
    case NMX_CACHE_WT: return kIOMapWriteThruCache;
    case NMX_CACHE_WB: return kIOMapCopybackCache;
    default: return UINT32_MAX;
    }
}

static IOMemoryDescriptor *physical_descriptor(uint64_t pa, size_t size, IODirection direction)
{
    IOAddressRange range = {};
    range.address = (IOVirtualAddress)pa;
    range.length = (IOByteCount)size;
    return IOMemoryDescriptor::withOptions(
        &range, 1, 0, nullptr,
        kIOMemoryTypePhysical64 | kIOMemoryMapperNone | direction,
        nullptr);
}

static bool system_mapper_present(void)
{
    /* The SDK helper waits for IOMapper's startup sentinel to be resolved. */
    IOMapper::checkForSystemMapper();
    return IOMapper::gSystem != nullptr;
}

extern "C" void *nmx_xnu_mem_alloc(size_t size, int contiguous, uint64_t max_phys, size_t align, void **handle)
{
    if (handle == nullptr)
        return nullptr;
    *handle = nullptr;
    if (size == 0)
        return nullptr;

    size_t requested_align = align;
    if (requested_align == 0)
        requested_align = PAGE_SIZE;
    if (!power_of_two(requested_align))
        return nullptr;
    if (requested_align < PAGE_SIZE)
        requested_align = PAGE_SIZE;

    IOBufferMemoryDescriptor *md = nullptr;
    if (contiguous) {
        /* IOBufferMemoryDescriptor takes a physical bit mask. Clearing the
         * alignment bits forces the returned run to meet the requested
         * physical alignment. The post-allocation check enforces max_phys as
         * an inclusive upper bound even when it is not a contiguous bit mask.
         */
        uint64_t mask = physical_mask_for_limit(max_phys);
        mask &= ~((uint64_t)requested_align - 1);
        md = IOBufferMemoryDescriptor::inTaskWithPhysicalMask(
            kernel_task,
            kIODirectionInOut | kIOMemoryPhysicallyContiguous | kIOMemoryKernelUserShared,
            (mach_vm_size_t)size,
            (mach_vm_address_t)mask);
    } else {
        md = IOBufferMemoryDescriptor::inTaskWithOptions(
            kernel_task,
            kIODirectionInOut | kIOMemoryKernelUserShared,
            (vm_size_t)size,
            (vm_offset_t)requested_align);
    }
    if (md == nullptr)
        return nullptr;

    if (md->prepare(kIODirectionInOut) != kIOReturnSuccess) {
        md->release();
        return nullptr;
    }

    void *kva = md->getBytesNoCopy();
    if (kva == nullptr) {
        md->complete(kIODirectionInOut);
        md->release();
        return nullptr;
    }

    if (contiguous) {
        IOByteCount segment_length = 0;
        uint64_t pa = (uint64_t)md->getPhysicalSegment(0, &segment_length, kIOMemoryMapperNone);
        uint64_t last = 0;
        if (pa == 0 || segment_length < size || (pa & (requested_align - 1)) != 0 ||
            !range_last(pa, size, &last) || (max_phys != 0 && last > max_phys)) {
            md->complete(kIODirectionInOut);
            md->release();
            return nullptr;
        }
    }

    *handle = md;
    return kva;
}

extern "C" void nmx_xnu_mem_free(void *handle)
{
    IOBufferMemoryDescriptor *md = (IOBufferMemoryDescriptor *)handle;
    if (md == nullptr)
        return;
    md->complete(kIODirectionInOut);
    md->release();
}

extern "C" uint64_t nmx_xnu_kva_to_phys(const void *va)
{
    if (va == nullptr)
        return 0;

    /* withAddress() describes a kernel virtual range. Preparing it makes
     * the translation valid for the duration of getPhysicalSegment(). */
    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddress(
        (void *)va, 1, kIODirectionNone);
    if (md == nullptr)
        return 0;

    uint64_t pa = 0;
    if (md->prepare(kIODirectionNone) == kIOReturnSuccess) {
        IOByteCount segment_length = 0;
        pa = (uint64_t)md->getPhysicalSegment(0, &segment_length, kIOMemoryMapperNone);
        if (segment_length == 0)
            pa = 0;
        md->complete(kIODirectionNone);
    }
    md->release();
    return pa;
}

extern "C" void *nmx_xnu_map_phys(uint64_t pa, size_t size, int cache, void **handle)
{
    if (handle == nullptr)
        return nullptr;
    *handle = nullptr;
    if (size == 0)
        return nullptr;
    IOOptionBits cache_bits = cache_option(cache);
    if (cache_bits == UINT32_MAX)
        return nullptr;

    uint64_t last = 0;
    if (!range_last(pa, size, &last) || last == UINT64_MAX)
        return nullptr;
    uint64_t base = pa & ~((uint64_t)PAGE_MASK);
    uint64_t rounded_last = last | (uint64_t)PAGE_MASK;
    if (rounded_last < base || rounded_last - base == UINT64_MAX)
        return nullptr;
    uint64_t map_length = rounded_last - base + 1;
    uint64_t offset = pa - base;

    IOMemoryDescriptor *md = physical_descriptor(base, (size_t)map_length, kIODirectionInOut);
    if (md == nullptr)
        return nullptr;
    IOMemoryMap *map = md->createMappingInTask(
        kernel_task, 0, kIOMapAnywhere | cache_bits);
    md->release();
    if (map == nullptr)
        return nullptr;

    mach_vm_address_t address = map->getAddress();
    if (address == 0 || address > UINT64_MAX - offset) {
        map->release();
        return nullptr;
    }
    *handle = map;
    return (void *)(uintptr_t)(address + offset);
}

extern "C" void *nmx_xnu_map_pages(const uint64_t *pages, size_t npages, int cache, void **handle)
{
    if (handle == nullptr)
        return nullptr;
    *handle = nullptr;
    if (pages == nullptr || npages == 0 || npages > UINT32_MAX ||
        npages > SIZE_MAX / sizeof(IOAddressRange))
        return nullptr;
    IOOptionBits cache_bits = cache_option(cache);
    if (cache_bits == UINT32_MAX)
        return nullptr;

    size_t ranges_bytes = npages * sizeof(IOAddressRange);
    IOAddressRange *ranges = (IOAddressRange *)IOMalloc((vm_size_t)ranges_bytes);
    if (ranges == nullptr)
        return nullptr;

    size_t nranges = 0;
    for (size_t i = 0; i < npages; ++i) {
        uint64_t page = pages[i];
        if ((page & (uint64_t)PAGE_MASK) != 0 || page > UINT64_MAX - PAGE_SIZE) {
            IOFree(ranges, (vm_size_t)ranges_bytes);
            return nullptr;
        }
        if (nranges != 0) {
            uint64_t prior_end = 0;
            bool prior_overflow = add_overflow_u64((uint64_t)ranges[nranges - 1].address,
                (uint64_t)ranges[nranges - 1].length, &prior_end);
            if (!prior_overflow && prior_end == page &&
                ranges[nranges - 1].length <= UINT64_MAX - PAGE_SIZE) {
                ranges[nranges - 1].length += PAGE_SIZE;
                continue;
            }
        }
        ranges[nranges].address = (IOVirtualAddress)page;
        ranges[nranges].length = PAGE_SIZE;
        ++nranges;
    }

    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddressRanges(
        ranges, (UInt32)nranges,
        kIODirectionInOut | kIOMemoryTypePhysical64 | kIOMemoryMapperNone, nullptr);
    IOFree(ranges, (vm_size_t)ranges_bytes);
    if (md == nullptr)
        return nullptr;

    IOMemoryMap *map = md->createMappingInTask(
        kernel_task, 0, kIOMapAnywhere | cache_bits);
    md->release();
    if (map == nullptr)
        return nullptr;

    mach_vm_address_t address = map->getAddress();
    if (address == 0) {
        map->release();
        return nullptr;
    }
    *handle = map;
    return (void *)(uintptr_t)address;
}

extern "C" void nmx_xnu_unmap(void *handle)
{
    IOMemoryMap *map = (IOMemoryMap *)handle;
    if (map != nullptr)
        map->release();
}

extern "C" int nmx_xnu_iommu_active(void)
{
    return system_mapper_present() ? 1 : 0;
}

static UInt8 dma_address_bits(uint64_t mask)
{
    if (mask == 0 || mask == UINT64_MAX)
        return 0; /* no additional address-width restriction */
    UInt8 bits = 0;
    while (mask != 0) {
        ++bits;
        mask >>= 1;
    }
    return bits;
}

extern "C" int nmx_xnu_dma_map(void *pci, uint64_t pa, size_t size, uint64_t dma_mask,
                                uint64_t *dma_addr, void **handle)
{
    if (handle == nullptr || dma_addr == nullptr)
        return EINVAL;
    *handle = nullptr;
    *dma_addr = 0;

    uint64_t last = 0;
    if (!range_last(pa, size, &last))
        return EINVAL;

    bool system_mapper = system_mapper_present();
    IOMapper *device_mapper = nullptr;
    if (pci != nullptr)
        device_mapper = IOMapper::copyMapperForDevice((IOService *)pci);

    /* Physical addresses are device addresses only when no system or
     * per-device mapper is attached. Never fall back to identity after a
     * mapper has been found or a mapped command fails. */
    if (!system_mapper && device_mapper == nullptr) {
        if (dma_mask != 0 && last > dma_mask)
            return EFBIG;
        *dma_addr = pa;
        return 0;
    }

    IOMemoryDescriptor *md = physical_descriptor(pa, size, kIODirectionInOut);
    if (md == nullptr) {
        if (device_mapper != nullptr)
            device_mapper->release();
        return ENOMEM;
    }

    IOMapper *mapper = device_mapper != nullptr ? device_mapper : IOMapper::gSystem;
    IODMACommand *cmd = IODMACommand::withSpecification(
        IODMACommand::OutputHost64, dma_address_bits(dma_mask), (UInt64)size,
        IODMACommand::kMapped, (UInt64)size, 1, mapper, nullptr);
    if (device_mapper != nullptr)
        device_mapper->release();
    if (cmd == nullptr) {
        md->release();
        return ENOMEM;
    }

    if (cmd->setMemoryDescriptor(md, true) != kIOReturnSuccess) {
        if (cmd->getMemoryDescriptor() != nullptr)
            cmd->clearMemoryDescriptor(true);
        cmd->release();
        md->release();
        return EFBIG;
    }

    IODMACommand::Segment64 seg = {};
    UInt64 offset = 0;
    UInt32 nseg = 1;
    IOReturn result = cmd->gen64IOVMSegments(&offset, &seg, &nseg);
    uint64_t dma_last = 0;
    if (result != kIOReturnSuccess || nseg != 1 || seg.fLength < size ||
        !range_last(seg.fIOVMAddr, size, &dma_last) ||
        (dma_mask != 0 && dma_last > dma_mask)) {
        cmd->clearMemoryDescriptor(true);
        cmd->release();
        md->release();
        return EFBIG;
    }

    nmx_dma_map *mapping = (nmx_dma_map *)IOMalloc(sizeof(*mapping));
    if (mapping == nullptr) {
        cmd->clearMemoryDescriptor(true);
        cmd->release();
        md->release();
        return ENOMEM;
    }
    mapping->cmd = cmd;
    mapping->md = md;
    *dma_addr = (uint64_t)seg.fIOVMAddr;
    *handle = mapping;
    return 0;
}

extern "C" void nmx_xnu_dma_unmap(void *handle)
{
    nmx_dma_map *mapping = (nmx_dma_map *)handle;
    if (mapping == nullptr)
        return;
    mapping->cmd->clearMemoryDescriptor(true);
    mapping->cmd->release();
    mapping->md->release();
    IOFree(mapping, sizeof(*mapping));
}
