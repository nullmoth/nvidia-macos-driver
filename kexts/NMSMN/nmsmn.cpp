// NMSMN: root-only, opt-in access to the SMU of an AMD Ryzen host bridge, for a power-limit tool (RyzenAdj).
//
// macOS gives a program no way to reach PCI configuration space, so a tool that sets a Ryzen's power limits through the
// SMU cannot run. This kext attaches to the AMD host bridge (bus 0, device 0, function 0) of a Zen-family CPU when the
// boot argument nmsmn=1 is present, and publishes one user client with three calls:
//   read   one SMN register,
//   write  one SMN register,
//   table  read the SMU's power table.
// What a client may do is decided by nmsmn_policy.h: only the SMU mailbox registers are accepted, a response register can
// only be cleared, and the power table can be read only at the address the SMU itself reported and at most 64 KiB of it.
// There is no call that takes a physical address.
//
// Who may connect: the caller must hold the administrator privilege (root), carry the entitlement
// com.nullmoth.ryzenadj, and be a process named ryzenadj. macOS offers a kext no public way to read the caller's
// executable path or to verify its signature, so the entitlement (read from the caller's code signature by IOKit) and
// the process name stand in for the path, and the installer puts the one binary that carries them in a root-owned
// directory. These checks are not the security boundary: root can already load any kext, so root is the real one; they
// keep a stray or hostile non-helper process from using a root-owned job it did not start. Every access is serialised,
// copies are bounded, and nothing returned to the client is a kernel pointer or a physical address.
// IOPCIDevice is deprecated in favour of PCIDriverKit, which cannot reach config space of a bridge from a driver extension
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <pexpert/pexpert.h>
#include <sys/proc.h>
#include <pexpert/i386/boot.h>
#include <mach/kmod.h>
#include <string.h>
#include "nmsmn_policy.h"

#define NMSMN_INDEX 0xB8
#define NMSMN_DATA 0xBC
#define NMSMN_CHUNK 4096u                       // one table read returns at most this many bytes

enum { kNMSMNRead = 0, kNMSMNWrite = 1, kNMSMNTable = 2, kNMSMNTableInit = 3, kNMSMNCount };

class NMSMN : public IOService {
    OSDeclareDefaultStructors(NMSMN)
public:
    IOPCIDevice *bridge;
    IOLock *lock;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn smn(struct nmsmn_state *st, uint32_t addr, uint32_t *value, bool write);
    IOReturn table(struct nmsmn_state *st, uint32_t off, uint32_t len, void *out);
    IOReturn tableInit(struct nmsmn_state *st, uint32_t *limit);
};
OSDefineMetaClassAndStructors(NMSMN, IOService)

class NMSMNClient : public IOUserClient {
    OSDeclareDefaultStructors(NMSMNClient)
    NMSMN *owner;
    struct nmsmn_state st;
public:
    bool initWithTask(task_t task, void *secID, UInt32 type) override;
    bool start(IOService *provider) override;
    IOReturn clientClose() override;
    IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
                            OSObject *target, void *reference) override;
};
OSDefineMetaClassAndStructors(NMSMNClient, IOUserClient)

// Zen CPUs (family 17h, 19h, 1Ah) carry the SMU this kext knows; anything else is left alone.
static bool cpuFamilyModel(unsigned *family, unsigned *model)
{
    uint32_t a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    unsigned fam = (a >> 8) & 0xF;
    unsigned mod = (a >> 4) & 0xF;
    if (fam == 0xF)
        fam += (a >> 20) & 0xFF;
    if (fam == 0xF || fam >= 6)
        mod |= ((a >> 16) & 0xF) << 4;
    *family = fam;
    *model = mod;
    return true;
}

static bool zenCpu()
{
    uint32_t a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0), "c"(0));
    if (b != 0x68747541u || d != 0x69746E65u || c != 0x444D4163u)       // "AuthenticAMD"
        return false;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    uint32_t fam = (a >> 8) & 0xF;
    if (fam == 0xF)
        fam += (a >> 20) & 0xFF;
    return fam == 0x17 || fam == 0x19 || fam == 0x1A;
}

bool NMSMN::start(IOService *provider)
{
    int on = 0;
    if (!PE_parse_boot_argn("nmsmn", &on, sizeof on) || on != 1)
        return false;                               // opt-in: nothing is attached without the boot argument
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || pci->getBusNumber() != 0 || pci->getDeviceNumber() != 0 || pci->getFunctionNumber() != 0)
        return false;
    if (pci->configRead16(0) != 0x1022 || !zenCpu())
        return false;
    if (!IOService::start(provider))
        return false;
    lock = IOLockAlloc();
    if (lock == nullptr)
        return false;
    bridge = pci;
    bridge->retain();
    registerService();
    IOLog("NMSMN: SMU access on host bridge 1022:%04x\n", pci->configRead16(2));
    return true;
}

void NMSMN::stop(IOService *provider)
{
    if (bridge) { bridge->release(); bridge = nullptr; }
    if (lock) { IOLockFree(lock); lock = nullptr; }
    IOService::stop(provider);
}

IOReturn NMSMN::smn(struct nmsmn_state *, uint32_t addr, uint32_t *value, bool write)
{
    if (bridge == nullptr || lock == nullptr)
        return kIOReturnNotReady;
    IOLockLock(lock);
    IOReturn r = kIOReturnNotPermitted;
    if (write ? nmsmn_check_write(addr, *value) == 0 : nmsmn_allowed(addr)) {
        bridge->configWrite32(NMSMN_INDEX, addr);
        if (write) {
            bridge->configWrite32(NMSMN_DATA, *value);
        } else {
            *value = bridge->configRead32(NMSMN_DATA);
        }
        r = kIOReturnSuccess;
    }
    IOLockUnlock(lock);
    return r;
}

IOReturn NMSMN::table(struct nmsmn_state *s, uint32_t off, uint32_t len, void *out)
{
    if (len > NMSMN_CHUNK)
        return kIOReturnBadArgument;
    IOLockLock(lock);
    IOReturn r = kIOReturnNotPermitted;
    if (nmsmn_table_range_ok(s, off, len)) {
        // the page range that covers [table, table + NMSMN_PM_MAX): the descriptor cannot reach past what the SMU named
        IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)s->table, s->limit, kIODirectionIn);
        r = kIOReturnNoMemory;
        if (md != nullptr) {
            if (md->prepare() == kIOReturnSuccess) {
                r = md->readBytes(off, out, len) == len ? kIOReturnSuccess : kIOReturnIOError;
                md->complete();
            }
            md->release();
        }
    }
    IOLockUnlock(lock);
    return r;
}

// The raw register access of the kext itself (the policy applies to clients, not to the kext's own exchange).
static uint32_t rawRead(IOPCIDevice *b, uint32_t addr) { b->configWrite32(NMSMN_INDEX, addr); return b->configRead32(NMSMN_DATA); }
static void rawWrite(IOPCIDevice *b, uint32_t addr, uint32_t v) { b->configWrite32(NMSMN_INDEX, addr); b->configWrite32(NMSMN_DATA, v); }

// Reads the firmware's memory map (a physical copy boot.efi left for the kernel) into a buffer; the caller frees it.
static unsigned char *readEfiMap(uint32_t *size, uint32_t *stride)
{
    boot_args *ba = (boot_args *)PE_state.bootArgs;
    if (ba == nullptr || ba->MemoryMap == 0 || ba->MemoryMapSize == 0 || ba->MemoryMapSize > (4u << 20) || ba->MemoryMapDescriptorSize < 40)
        return nullptr;
    unsigned char *buf = (unsigned char *)IOMallocData(ba->MemoryMapSize);
    if (buf == nullptr)
        return nullptr;
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)ba->MemoryMap, ba->MemoryMapSize, kIODirectionIn);
    bool ok = false;
    if (md != nullptr) {
        if (md->prepare() == kIOReturnSuccess) {
            ok = md->readBytes(0, buf, ba->MemoryMapSize) == ba->MemoryMapSize;
            md->complete();
        }
        md->release();
    }
    if (!ok) {
        IOFreeData(buf, ba->MemoryMapSize);
        return nullptr;
    }
    *size = ba->MemoryMapSize;
    *stride = ba->MemoryMapDescriptorSize;
    return buf;
}

// Learns the power table's address from the SMU, entirely inside the kext: no client value reaches the mailbox, nothing the
// client wrote is read back, and the answer is kept only if nmsmn_table_accept approves it.
IOReturn NMSMN::tableInit(struct nmsmn_state *s, uint32_t *limit)
{
    unsigned family = 0, model = 0;
    uint32_t msg = 0, arg0 = 0;
    int wide = 0;
    cpuFamilyModel(&family, &model);
    if (nmsmn_table_message(family, model, &msg, &arg0, &wide) != 0)
        return kIOReturnUnsupported;
    if (bridge == nullptr || lock == nullptr)
        return kIOReturnNotReady;
    const struct nmsmn_mailbox *mb = &kNmsmnMailboxes[4];      // the platform SMU's mailbox
    IOLockLock(lock);
    nmsmn_state_init(s);
    rawWrite(bridge, mb->rsp, 0);
    rawWrite(bridge, mb->arg, arg0);
    for (uint32_t i = 1; i < NMSMN_ARGS; i++)
        rawWrite(bridge, mb->arg + 4u * i, 0);
    rawWrite(bridge, mb->msg, msg);
    uint32_t rsp = 0;
    for (int tries = 0; tries < 2000 && rsp == 0; tries++) {   // at most about 100 ms
        rsp = rawRead(bridge, mb->rsp);
        if (rsp == 0)
            IODelay(50);
    }
    IOReturn r = kIOReturnTimeout;
    if (rsp == NMSMN_RESP_OK) {
        uint64_t addr = rawRead(bridge, mb->arg);
        if (wide)
            addr |= (uint64_t)rawRead(bridge, mb->arg + 4u) << 32;
        uint32_t size = 0, stride = 0;
        unsigned char *map = readEfiMap(&size, &stride);
        r = kIOReturnNotPermitted;
        if (map != nullptr) {
            if (nmsmn_table_accept(s, addr, map, size, stride) == 0) {
                *limit = s->limit;
                r = kIOReturnSuccess;
            }
            IOFreeData(map, size);
        }
    } else if (rsp != 0) {
        r = kIOReturnError;
    }
    IOLockUnlock(lock);
    return r;
}

bool NMSMNClient::initWithTask(task_t task, void *secID, UInt32 type)
{
    if (!IOUserClient::initWithTask(task, secID, type))
        return false;
    nmsmn_state_init(&st);
    owner = nullptr;
    // 1. root
    if (clientHasPrivilege(task, kIOClientPrivilegeAdministrator) != kIOReturnSuccess)
        return false;
    // 2. the entitlement in the caller's code signature
    OSObject *ent = copyClientEntitlement(task, "com.nullmoth.ryzenadj");
    bool entitled = ent == kOSBooleanTrue;
    if (ent) ent->release();
    if (!entitled)
        return false;
    // 3. the calling process is the installed helper (this runs in the caller's context)
    char name[MAXCOMLEN + 1] = {};
    proc_selfname(name, sizeof name);
    return strncmp(name, "ryzenadj", sizeof name) == 0;
}

bool NMSMNClient::start(IOService *provider)
{
    owner = OSDynamicCast(NMSMN, provider);
    return owner != nullptr && IOUserClient::start(provider);
}

IOReturn NMSMNClient::clientClose()
{
    owner = nullptr;
    terminate();
    return kIOReturnSuccess;
}

IOReturn NMSMNClient::externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *,
                                     OSObject *, void *)
{
    if (owner == nullptr || args == nullptr)
        return kIOReturnNotReady;
    if (selector == kNMSMNRead && args->scalarInputCount == 1 && args->scalarOutputCount == 1) {
        uint32_t v = 0;
        IOReturn r = owner->smn(&st, (uint32_t)args->scalarInput[0], &v, false);
        if (r == kIOReturnSuccess)
            args->scalarOutput[0] = v;
        return r;
    }
    if (selector == kNMSMNWrite && args->scalarInputCount == 2 && args->scalarOutputCount == 0) {
        uint32_t v = (uint32_t)args->scalarInput[1];
        return owner->smn(&st, (uint32_t)args->scalarInput[0], &v, true);
    }
    if (selector == kNMSMNTableInit && args->scalarInputCount == 0 && args->scalarOutputCount == 1) {
        uint32_t limit = 0;
        IOReturn r = owner->tableInit(&st, &limit);
        if (r == kIOReturnSuccess)
            args->scalarOutput[0] = limit;       // the size of the readable window, never the address
        return r;
    }
    if (selector == kNMSMNTable && args->scalarInputCount == 2 && args->structureOutput != nullptr) {
        uint32_t off = (uint32_t)args->scalarInput[0], len = (uint32_t)args->scalarInput[1];
        if (len > args->structureOutputSize)
            return kIOReturnBadArgument;
        IOReturn r = owner->table(&st, off, len, args->structureOutput);
        if (r == kIOReturnSuccess)
            args->structureOutputSize = len;
        return r;
    }
    return kIOReturnBadArgument;
}

extern "C" kern_return_t NMSMN_start(kmod_info_t *, void *) { return KERN_SUCCESS; }
extern "C" kern_return_t NMSMN_stop(kmod_info_t *, void *) { return KERN_SUCCESS; }
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMSMN, "1.0.0", NMSMN_start, NMSMN_stop) }
