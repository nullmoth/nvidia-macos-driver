#include "NMKbdBacklight.hpp"
#include "NMKbdBacklightHID.hpp"

OSDefineMetaClassAndStructors(NMKbdBacklightHID, IOHIDDevice)

bool NMKbdBacklightHID::init(NMKbdBacklight *owner, OSDictionary *dict) {
    if (!IOHIDDevice::init(dict)) return false;
    owner_ = owner;
    setProperty("Built-In", kOSBooleanTrue);
    setProperty("KeyboardBacklightBuiltIn", kOSBooleanTrue);
    return true;
}

OSString *NMKbdBacklightHID::newTransportString() const { return OSString::withCString("Virtual"); }
OSString *NMKbdBacklightHID::newManufacturerString() const { return OSString::withCString("NullMoth"); }
OSString *NMKbdBacklightHID::newProductString() const { return OSString::withCString("Keyboard Backlight"); }
OSString *NMKbdBacklightHID::newSerialNumberString() const { return OSString::withCString("NMKBL0001"); }
OSNumber *NMKbdBacklightHID::newVendorIDNumber() const { return OSNumber::withNumber(0x4E4D, 32); }
OSNumber *NMKbdBacklightHID::newProductIDNumber() const { return OSNumber::withNumber(0x0001, 32); }
OSNumber *NMKbdBacklightHID::newVersionNumber() const { return OSNumber::withNumber(0x0100, 32); }
OSNumber *NMKbdBacklightHID::newLocationIDNumber() const { return OSNumber::withNumber(0x00000001, 32); }

IOReturn NMKbdBacklightHID::newReportDescriptor(IOMemoryDescriptor **descriptor) const {
    IOBufferMemoryDescriptor *d = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionOutIn, sizeof kKblHidDescriptor);
    if (!d) return kIOReturnNoMemory;
    d->writeBytes(0, kKblHidDescriptor, sizeof kKblHidDescriptor);
    *descriptor = d;
    return kIOReturnSuccess;
}

IOReturn NMKbdBacklightHID::getReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options) {
    if (!report) return kIOReturnBadArgument;
    uint8_t id = (uint8_t)(options & 0xFF);
    if (reportType != kIOHIDReportTypeFeature || id != KBL_HID_REPORT_CONFIG) return kIOReturnUnsupported;
    uint8_t buf[KBL_HID_CONFIG_BYTES];
    kbl_hid_config_report(buf);
    if (report->getLength() < sizeof buf) return kIOReturnBadArgument;
    report->writeBytes(0, buf, sizeof buf);
    return kIOReturnSuccess;
}

IOReturn NMKbdBacklightHID::setReport(IOMemoryDescriptor *report, IOHIDReportType reportType, IOOptionBits options) {
    if (!report || !owner_) return kIOReturnBadArgument;
    if (reportType != kIOHIDReportTypeOutput) return kIOReturnUnsupported;
    uint8_t buf[16];
    unsigned n = (unsigned)report->getLength();
    if (n > sizeof buf) n = sizeof buf;
    report->readBytes(0, buf, n);
    int brightness = 0, rate = 0;
    if (!kbl_hid_parse_set(buf, n, &brightness, &rate)) return kIOReturnBadArgument;
    hidBrightness_ = brightness;
    return owner_->setLevel(kbl_smc_to_level(kbl_hid_to_v12(brightness), owner_->maxLevel())) ? kIOReturnSuccess : kIOReturnIOError;
}
