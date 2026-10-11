/*
 * Wired memory and kernel mappings for the port, on IOKit memory descriptors. Compiled with Apple's headers only.
 *
 * Allocations are IOBufferMemoryDescriptors (wired, kernel-mapped); a physically contiguous one is requested with
 * kIOMemoryPhysicallyContiguous and a physical mask derived from the caller's address limit and alignment.
 * Mappings of physical memory (GPU BARs, vmap of scattered pages) are IOMemoryDescriptor mappings into the kernel
 * with the requested cache mode, which is how IOKit drivers get write-combined or uncached views.
 */
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <libkern/OSAtomic.h>
#include "nmx_xnu.h"

extern "C" {
extern void *kernel_pmap;
extern ppnum_t pmap_find_phys(void *pmap, addr64_t va);
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

extern "C" void *nmx_xnu_mem_alloc(size_t size, int contiguous, uint64_t max_phys, size_t align, void **handle)
{
    IOBufferMemoryDescriptor *md;
    *handle = nullptr;
    if (contiguous) {
        /* the mask's low bits force alignment, its high bits bound the physical address */
        mach_vm_address_t mask = (max_phys ? (mach_vm_address_t)max_phys : ~0ULL);
        if (align > PAGE_SIZE)
            mask &= ~((mach_vm_address_t)align - 1);
        else
            mask &= ~((mach_vm_address_t)PAGE_SIZE - 1);
        md = IOBufferMemoryDescriptor::inTaskWithPhysicalMask(kernel_task,
                 kIODirectionInOut | kIOMemoryPhysicallyContiguous | kIOMemoryKernelUserShared, size, mask);
    } else {
        md = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut | kIOMemoryKernelUserShared,
                                                         size, PAGE_SIZE);
    }
    if (md == nullptr)
        return nullptr;
    if (md->prepare() != kIOReturnSuccess) {
        md->release();
        return nullptr;
    }
    *handle = md;
    return md->getBytesNoCopy();
}

extern "C" void nmx_xnu_mem_free(void *handle)
{
    IOBufferMemoryDescriptor *md = (IOBufferMemoryDescriptor *)handle;
    if (md == nullptr)
        return;
    md->complete();
    md->release();
}

extern "C" uint64_t nmx_xnu_kva_to_phys(const void *va)
{
    ppnum_t pn = pmap_find_phys(kernel_pmap, (addr64_t)(uintptr_t)va);
    if (pn == 0)
        return 0;
    return ((uint64_t)pn << PAGE_SHIFT) | ((uintptr_t)va & PAGE_MASK);
}

extern "C" void *nmx_xnu_map_phys(uint64_t pa, size_t size, int cache, void **handle)
{
    *handle = nullptr;
    uint64_t base = pa & ~(uint64_t)PAGE_MASK;
    size_t len = (size_t)(((pa + size + PAGE_MASK) & ~(uint64_t)PAGE_MASK) - base);
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)base, len, kIODirectionInOut);
    if (md == nullptr)
        return nullptr;
    IOMemoryMap *map = md->createMappingInTask(kernel_task, 0, kIOMapAnywhere | cache_option(cache));
    md->release();      /* the map keeps the descriptor */
    if (map == nullptr)
        return nullptr;
    *handle = map;
    return (void *)(uintptr_t)(map->getVirtualAddress() + (pa - base));
}

extern "C" void *nmx_xnu_map_pages(const uint64_t *pages, size_t npages, int cache, void **handle)
{
    *handle = nullptr;
    if (npages == 0)
        return nullptr;
    IOAddressRange *ranges = (IOAddressRange *)IOMalloc(sizeof(IOAddressRange) * npages);
    if (ranges == nullptr)
        return nullptr;
    /* coalesce physically adjacent pages into one range each */
    size_t n = 0;
    for (size_t i = 0; i < npages; i++) {
        if (n > 0 && ranges[n - 1].address + ranges[n - 1].length == pages[i]) {
            ranges[n - 1].length += PAGE_SIZE;
        } else {
            ranges[n].address = pages[i];
            ranges[n].length = PAGE_SIZE;
            n++;
        }
    }
    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddressRanges(ranges, (UInt32)n,
                                 kIODirectionInOut | kIOMemoryTypePhysical64, nullptr);
    IOFree(ranges, sizeof(IOAddressRange) * npages);
    if (md == nullptr)
        return nullptr;
    IOMemoryMap *map = md->createMappingInTask(kernel_task, 0, kIOMapAnywhere | cache_option(cache));
    md->release();
    if (map == nullptr)
        return nullptr;
    *handle = map;
    return (void *)(uintptr_t)map->getVirtualAddress();
}

extern "C" void nmx_xnu_unmap(void *handle)
{
    IOMemoryMap *map = (IOMemoryMap *)handle;
    if (map)
        map->release();
}
