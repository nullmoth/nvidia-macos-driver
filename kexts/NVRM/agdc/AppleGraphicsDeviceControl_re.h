/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
struct AGDCClientState_t;
struct IOExternalMethodArguments;
class AppleGraphicsDeviceControl : public IOService {
    OSDeclareDefaultStructors(AppleGraphicsDeviceControl)
    uint8_t _agdc_private[0x110 - sizeof(IOService)];
public:
    virtual bool     start(IOService *provider) APPLE_KEXT_OVERRIDE;
    virtual void     stop(IOService *provider) APPLE_KEXT_OVERRIDE;
    virtual IOReturn message(UInt32 type, IOService *provider, void *argument = 0) APPLE_KEXT_OVERRIDE;
    virtual bool     terminate(IOOptionBits options = 0) APPLE_KEXT_OVERRIDE;
    virtual IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties, IOUserClient **handler) APPLE_KEXT_OVERRIDE;
    virtual IOReturn vendor_doDeviceAttribute(unsigned int cmd, unsigned long *in, unsigned long inSize,
                                              unsigned long *out, unsigned long *outSize, IOExternalMethodArguments *args) = 0;
    virtual IOReturn vendor_doDeviceAttribute(unsigned int cmd, unsigned long *in, unsigned long inSize,
                                              unsigned long *out, unsigned long *outSize, AGDCClientState_t *state);
    // The eight reserved slots exist from 10.11 on; on 10.9 and 10.10 Apple's class ends at vendor_doDeviceAttribute
    // (same slots before it), and naming slots the class does not have leaves them unresolved at load.
#if !defined(NM_ERA) || NM_ERA >= 1011
    virtual void _RESERVEDAppleGraphicsDeviceControl0(); virtual void _RESERVEDAppleGraphicsDeviceControl1();
    virtual void _RESERVEDAppleGraphicsDeviceControl2(); virtual void _RESERVEDAppleGraphicsDeviceControl3();
    virtual void _RESERVEDAppleGraphicsDeviceControl4(); virtual void _RESERVEDAppleGraphicsDeviceControl5();
    virtual void _RESERVEDAppleGraphicsDeviceControl6(); virtual void _RESERVEDAppleGraphicsDeviceControl7();
#endif
};
static_assert(sizeof(AppleGraphicsDeviceControl) == 0x110, "AppleGraphicsDeviceControl is 0x110 bytes in 15.7.9");
