#ifndef NM_KBD_BACKLIGHT_HID_HPP
#define NM_KBD_BACKLIGHT_HID_HPP

#include <IOKit/hid/IOHIDDevice.h>
#include "kbl_core.h"

class NMKbdBacklight;

/* A virtual built-in keyboard that carries the vendor element macOS's HID keyboard driver looks for, so macOS finds a keyboard
 * backlight on a PC the way it does on a T2-era Mac. It sends no key presses; it answers feature report 0xBF and takes output
 * report 0xB0, and passes the brightness to the laptop through the owner. */
class NMKbdBacklightHID : public IOHIDDevice {
    OSDeclareDefaultStructors(NMKbdBacklightHID)

public:
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
