// NMSMN: root-only, opt-in access to the SMU mailboxes of an AMD Ryzen host bridge, for power-limit tools.
//
// macOS gives a program no way to reach PCI configuration space, so a tool that sets a Ryzen's power limits through the
// SMU (RyzenAdj) cannot run. This kext attaches to the AMD host bridge (bus 0, device 0, function 0) when the boot
// argument nmsmn=1 is present, and publishes a user client with two calls: read and write one SMN register through the
// bridge's index/data pair (config offsets 0xB8 and 0xBC). Only the SMU mailbox registers in nmsmn_allow.h are
// accepted, only a client holding the administrator privilege may connect, and every access is serialised. There is no
// physical-memory access.
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <pexpert/pexpert.h>
#include <mach/kmod.h>
#include "nmsmn_allow.h"

#define NMSMN_INDEX 0xB8
#define NMSMN_DATA 0xBC

enum { kNMSMNRead = 0, kNMSMNWrite = 1, kNMSMNCount };

class NMSMN : public IOService {
    OSDeclareDefaultStructors(NMSMN)
public:
    IOPCIDevice *bridge;
    IOLock *lock;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn access(uint32_t addr, uint32_t *value, bool write);
};
OSDefineMetaClassAndStructors(NMSMN, IOService)

class NMSMNClient : public IOUserClient {
    OSDeclareDefaultStructors(NMSMNClient)
    NMSMN *owner;
public:
    bool initWithTask(task_t task, void *secID, UInt32 type) override;
    bool start(IOService *provider) override;
    IOReturn clientClose() override;
    IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
                            OSObject *target, void *reference) override;
};
OSDefineMetaClassAndStructors(NMSMNClient, IOUserClient)

bool NMSMN::start(IOService *provider)
{
    int on = 0;
    if (!PE_parse_boot_argn("nmsmn", &on, sizeof on) || on != 1)
        return false;                               // opt-in: nothing is attached without the boot argument
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || pci->getBusNumber() != 0 || pci->getDeviceNumber() != 0 || pci->getFunctionNumber() != 0)
        return false;
    if (!IOService::start(provider))
        return false;
    lock = IOLockAlloc();
    if (lock == nullptr)
        return false;
    bridge = pci;
    bridge->retain();
    registerService();
    IOLog("NMSMN: SMU mailbox access on %04x:%04x\n", pci->configRead16(0), pci->configRead16(2));
    return true;
}

void NMSMN::stop(IOService *provider)
{
    if (bridge) { bridge->release(); bridge = nullptr; }
    if (lock) { IOLockFree(lock); lock = nullptr; }
    IOService::stop(provider);
}

IOReturn NMSMN::access(uint32_t addr, uint32_t *value, bool write)
{
    if (!nmsmn_allowed(addr))
        return kIOReturnNotPermitted;
    if (bridge == nullptr || lock == nullptr)
        return kIOReturnNotReady;
    IOLockLock(lock);
    bridge->configWrite32(NMSMN_INDEX, addr);
    if (write)
        bridge->configWrite32(NMSMN_DATA, *value);
    else
        *value = bridge->configRead32(NMSMN_DATA);
    IOLockUnlock(lock);
    return kIOReturnSuccess;
}

bool NMSMNClient::initWithTask(task_t task, void *secID, UInt32 type)
{
    if (!IOUserClient::initWithTask(task, secID, type))
        return false;
    // administrator (root) only
    return clientHasPrivilege(task, kIOClientPrivilegeAdministrator) == kIOReturnSuccess;
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
        IOReturn r = owner->access((uint32_t)args->scalarInput[0], &v, false);
        if (r == kIOReturnSuccess)
            args->scalarOutput[0] = v;
        return r;
    }
    if (selector == kNMSMNWrite && args->scalarInputCount == 2 && args->scalarOutputCount == 0) {
        uint32_t v = (uint32_t)args->scalarInput[1];
        return owner->access((uint32_t)args->scalarInput[0], &v, true);
    }
    return kIOReturnBadArgument;
}

extern "C" kern_return_t NMSMN_start(kmod_info_t *, void *) { return KERN_SUCCESS; }
extern "C" kern_return_t NMSMN_stop(kmod_info_t *, void *) { return KERN_SUCCESS; }
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMSMN, "1.0.0", NMSMN_start, NMSMN_stop) }
