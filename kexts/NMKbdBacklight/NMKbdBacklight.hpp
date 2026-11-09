#ifndef NM_KBD_BACKLIGHT_HPP
#define NM_KBD_BACKLIGHT_HPP

// Lilu's and VirtualSMC's SDK headers (third-party) carry their own conversion, shadowing and struct/class tag warnings;
// the exception is scoped to these includes and this plugin's own code stays checked.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wsign-conversion"
#pragma clang diagnostic ignored "-Wshadow"
#pragma clang diagnostic ignored "-Wmismatched-tags"
#include <Headers/kern_util.hpp>
#include <IOKit/IOService.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <VirtualSMCSDK/kern_vsmcapi.hpp>
#pragma clang diagnostic pop
#include "kbl_core.h"

/* Publishes the keyboard backlight the way an Intel MacBook does, through the SMC key LKSB, and forwards it to the
 * laptop's own ACPI method. Opt-in: nothing happens unless the boot argument nmkbl=1 is present. */
class NMKbdBacklight : public IOService {
    OSDeclareDefaultStructors(NMKbdBacklight)

public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;

    /* the backend, called from the SMC value */
    bool setLevel(int level);
    bool getLevel(int *level);
    int maxLevel() const { return maxLevel_; }

private:
    enum Backend { None, Asus, IdeaPad, IdeaPadPlain, ThinkPad, Dell, Msi };

    friend class NMKbdBacklightHID;
    static bool vsmcNotificationHandler(void *sensors, void *refCon, IOService *vsmc, IONotifier *notifier);
    IOACPIPlatformDevice *find(const char *nameMatch, const char *method);
    bool callAsus(uint32_t method, uint32_t argument, uint32_t *result);
    bool evalInt(const char *method, uint32_t arg, uint32_t *result, bool noArgument = false);
    bool probeAsus();
    bool probeIdeaPad();
    bool probeThinkPad();
    bool probeDell();
    bool probeMsi();
    bool ecRead(uint8_t addr, uint8_t *v);
    bool ecWrite(uint8_t addr, uint8_t v);
    bool dellCall(uint32_t in0, uint32_t in1, uint32_t in2, uint32_t *out1, uint32_t *out2);

    IOACPIPlatformDevice *dev {nullptr};
    IOService *hidDevice {nullptr};
    bool forced_ {false}, skipSmc_ {false};
    Backend backend {None};
    int lenovoType {0};
    char dellMethod[8] {};
    const struct kbl_msi_row *msiRow {nullptr};
    unsigned dellBuf {4096};
    int maxLevel_ {KBL_ASUS_MAX_LEVEL};
    IONotifier *vsmcNotifier {nullptr};
    VirtualSMCAPI::Plugin vsmcPlugin {
        xStringify(PRODUCT_NAME), parseModuleVersion(xStringify(MODULE_VERSION)), VirtualSMCAPI::Version, {}, {},
    };
};

#endif
