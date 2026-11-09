#ifndef NM_KBD_BACKLIGHT_HID_HPP
#define NM_KBD_BACKLIGHT_HID_HPP

// the SDK's IOEventSource::init hides OSObject::init; scoped to this header (OSObject.h's own unpopped ignore used to
// cover it, and with it this file's classes)
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Woverloaded-virtual"
#include <IOKit/hid/IOHIDDevice.h>
#pragma clang diagnostic pop
#include "kbl_core.h"

class NMKbdBacklight;

/* A virtual built-in keyboard that carries the vendor element macOS's HID keyboard driver looks for, so macOS finds a keyboard
 * backlight on a PC the way it does on a T2-era Mac. It sends no key presses; it answers feature report 0xBF and takes output
 * report 0xB0, and passes the brightness to the laptop through the owner. */
class NMKbdBacklightHID : public IOHIDDevice {
    OSDeclareDefaultStructors(NMKbdBacklightHID)

public:
    using IOHIDDevice::init;   // the inherited init(OSDictionary *) stays visible beside the owner form below
    bool init(NMKbdBacklight *owner, OSDictionary *dict = nullptr);

    OSString *newTransportString() const override;
    OSString *newManufacturerString() const override;
    OSString *newProductString() const override;
    OSString *newSerialNumberString() const override;
    OSNumber *newVendorIDNumber() const override;
    OSNumber *newProductIDNumber() const override;
    OSNumber *newVersionNumber() const override;
    OSNumber *newLocationIDNumber() const override;
    IOReturn newReportDescriptor(IOMemoryDescriptor **descriptor) const override;
    IOReturn getReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options) override;
    IOReturn setReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options) override;

private:
    NMKbdBacklight *owner_ {nullptr};
    int hidBrightness_ {0};
};

#endif
