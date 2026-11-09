#ifndef NM_KBD_BACKLIGHT_HPP
#define NM_KBD_BACKLIGHT_HPP

#include <Headers/kern_util.hpp>
#include <IOKit/IOService.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <VirtualSMCSDK/kern_vsmcapi.hpp>
#include "kbl_core.h"

/* Publishes the keyboard backlight the way an Intel MacBook does, through the SMC key LKSB, and forwards it to the
 * laptop's own ACPI WMI method. Opt-in: nothing happens unless the boot argument nmkbl=1 is present. */
class NMKbdBacklight : public IOService {
    OSDeclareDefaultStructors(NMKbdBacklight)

public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;

    /* the backend, called from the SMC value */
    bool setLevel(int level);
    bool getLevel(int *level);
    int maxLevel() const { return KBL_ASUS_MAX_LEVEL; }

private:
    static bool vsmcNotificationHandler(void *sensors, void *refCon, IOService *vsmc, IONotifier *notifier);
    bool callAsus(uint32_t method, uint32_t argument, uint32_t *result);

    IOACPIPlatformDevice *wmi {nullptr};
    IONotifier *vsmcNotifier {nullptr};
    VirtualSMCAPI::Plugin vsmcPlugin {
        xStringify(PRODUCT_NAME), parseModuleVersion(xStringify(MODULE_VERSION)), VirtualSMCAPI::Version,
    };
};

#endif
