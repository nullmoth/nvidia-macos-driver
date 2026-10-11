/*
 * Device-visible (DMA) addresses for the port, through IODMACommand, so the address a GPU is given is correct
 * whether or not the system's IOMMU (VT-d / AMD-Vi, xnu's IOMapper) is active. Compiled with Apple's headers only.
 *
 * Without an active IOMapper the device address of system memory is its physical address, and mapping is free.
 * With one, each mapping is an IODMACommand prepared on a physical-range descriptor, and the handle keeps both
 * alive until the driver unmaps.
 */
#include <IOKit/IODMACommand.h>
#include <IOKit/IOMapper.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/IOLib.h>
#include <sys/errno.h>
#include "nmx_xnu.h"

struct nmx_dma_map {
    IODMACommand *cmd;
    IOMemoryDescriptor *md;
};

extern "C" int nmx_xnu_iommu_active(void)
{
    return IOMapper::gSystem != nullptr;
}

extern "C" int nmx_xnu_dma_map(void *pci, uint64_t pa, size_t size, uint64_t dma_mask, uint64_t *dma_addr, void **handle)
{
    *handle = nullptr;
    if (IOMapper::gSystem == nullptr) {
        if (dma_mask && (pa + size - 1) > dma_mask)
            return EFBIG;     /* beyond what the device can address; nothing is bounced */
        *dma_addr = pa;
        return 0;
    }
    UInt8 bits = 64;
    while (bits > 32 && dma_mask && !(dma_mask & (1ULL << (bits - 1))))
        bits--;
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)pa, size, kIODirectionInOut);
    if (md == nullptr)
        return ENOMEM;
    IODMACommand *cmd = IODMACommand::withSpecification(IODMACommand::OutputHost64, bits, 0,
                            IODMACommand::kMapped, 0, 1, IOMapper::gSystem, nullptr);
    if (cmd == nullptr) {
        md->release();
        return ENOMEM;
    }
    if (cmd->setMemoryDescriptor(md, true) != kIOReturnSuccess) {
        cmd->release();
        md->release();
        return EFBIG;
    }
    IODMACommand::Segment64 seg;
    UInt64 offset = 0;
    UInt32 nseg = 1;
    IOReturn r = cmd->gen64IOVMSegments(&offset, &seg, &nseg);
    if (r != kIOReturnSuccess || nseg != 1 || seg.fLength < size) {
        /* a mapping the IOMMU split in two cannot be handed to the GPU as one address */
        cmd->clearMemoryDescriptor(true);
        cmd->release();
        md->release();
        return EFBIG;
    }
    nmx_dma_map *m = (nmx_dma_map *)IOMalloc(sizeof(*m));
    if (m == nullptr) {
        cmd->clearMemoryDescriptor(true);
        cmd->release();
        md->release();
        return ENOMEM;
    }
    m->cmd = cmd;
    m->md = md;
    *dma_addr = seg.fIOVMAddr;
    *handle = m;
    return 0;
}

extern "C" void nmx_xnu_dma_unmap(void *handle)
{
    nmx_dma_map *m = (nmx_dma_map *)handle;
    if (m == nullptr)
        return;
    m->cmd->clearMemoryDescriptor(true);
    m->cmd->release();
    m->md->release();
    IOFree(m, sizeof(*m));
}
