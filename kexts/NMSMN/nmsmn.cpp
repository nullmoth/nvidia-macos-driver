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
// directory. Every access is serialised, copies are bounded, and nothing returned to the client is a kernel pointer.
// IOPCIDevice is deprecated in favour of PCIDriverKit, which cannot reach config space of a bridge from a driver extension
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <pexpert/pexpert.h>
#include <sys/proc.h>
#include <mach/kmod.h>
#include <string.h>
#include "nmsmn_policy.h"

#define NMSMN_INDEX 0xB8
#define NMSMN_DATA 0xBC
#define NMSMN_CHUNK 4096u                       // one table read returns at most this many bytes

enum { kNMSMNRead = 0, kNMSMNWrite = 1, kNMSMNTable = 2, kNMSMNCount };

class NMSMN : public IOService {
    OSDeclareDefaultStructors(NMSMN)
public:
    IOPCIDevice *bridge;
    IOLock *lock;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn smn(struct nmsmn_state *st, uint32_t addr, uint32_t *value, bool write);
    IOReturn table(struct nmsmn_state *st, uint32_t off, uint32_t len, void *out);
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

IOReturn NMSMN::smn(struct nmsmn_state *s, uint32_t addr, uint32_t *value, bool write)
{
    if (bridge == nullptr || lock == nullptr)
        return kIOReturnNotReady;
    IOLockLock(lock);
    IOReturn r = kIOReturnNotPermitted;
    if (write ? nmsmn_check_write(s, addr, *value) == 0 : nmsmn_allowed(addr)) {
        bridge->configWrite32(NMSMN_INDEX, addr);
        if (write) {
            bridge->configWrite32(NMSMN_DATA, *value);
        } else {
            *value = bridge->configRead32(NMSMN_DATA);
            nmsmn_note_read(s, addr, *value);
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
        IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)s->table, NMSMN_PM_MAX, kIODirectionIn);
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
