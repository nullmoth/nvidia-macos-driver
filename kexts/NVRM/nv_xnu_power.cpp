/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Where the power comes from, for the GPU firmware.
 *
 * A mobile GPU's firmware asks whether the machine is on mains or on battery and picks its clock and power policy
 * from the answer: it reads it once while starting (osinit.c) and again on every wake (dynamic-power.c), and feeds
 * it to RmPowerSourceChangeEvent. Both calls used to land on a stub that answered "not supported", so on a laptop
 * the firmware ran its mains policy no matter what the machine was doing. The boot log said so once per start:
 * "STUB nv_acpi_get_powersource".
 *
 * The answer comes from the same place the rest of macOS reads it: the battery service in the registry publishes
 * ExternalConnected, true while the charger is attached. A desktop has no such service, which is itself the answer
 * — it is always on mains.
 */

#include "nv-xnu.h"
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>

// The services that publish a battery on this platform. AppleSmartBattery is what a Mac laptop has; the second is
// the generic class the first is a kind of, and matching it as well keeps this working on a machine whose battery
// comes up under a different driver.
static const char *const kBatteryServices[] = { "AppleSmartBattery", "IOPMPowerSource" };

// true when a battery service exists and says the charger is attached; also reports whether a battery was found at
// all, so a desktop can be told apart from a laptop running on its battery.
static bool nv_xnu_read_power_source(bool *acPlugged, bool *haveBattery)
{
    bool found = false, ac = false;
    for (unsigned i = 0; i < sizeof kBatteryServices / sizeof *kBatteryServices && !found; i++) {
        OSDictionary *match = IOService::serviceMatching(kBatteryServices[i]);
        if (!match) continue;
        OSIterator *it = IOService::getMatchingServices(match);
        match->release();
        if (!it) continue;
        while (OSObject *o = it->getNextObject()) {
            IORegistryEntry *e = OSDynamicCast(IORegistryEntry, o);
            if (!e) continue;
            OSObject *v = e->copyProperty("ExternalConnected");
            OSBoolean *b = OSDynamicCast(OSBoolean, v);
            if (b) { found = true; ac = b->isTrue(); }
            if (v) v->release();
            if (found) break;
        }
        it->release();
    }
    *haveBattery = found;
    *acPlugged = found ? ac : true;          // no battery at all: this machine runs on mains
    return true;
}

extern "C" {

// NV_OK with *ac_plugged 1 on mains, 0 on battery. The firmware calls this on a mobile GPU only.
NV_STATUS NV_API_CALL nv_acpi_get_powersource(NvU32 *ac_plugged)
{
    if (!ac_plugged) return NV_ERR_INVALID_ARGUMENT;
    bool ac = true, haveBattery = false;
    nv_xnu_read_power_source(&ac, &haveBattery);
    *ac_plugged = ac ? 1 : 0;
    // Once per change, not once per call: the firmware asks on every wake and this would otherwise fill the log.
    static int lastReported = -1;
    const int now = ac ? 1 : 0;
    if (now != lastReported) {
        lastReported = now;
        IOLog("NVRM-xnu: power source: %s (battery %s)\n", ac ? "mains" : "battery",
              haveBattery ? "present" : "none - desktop");
    }
    return NV_OK;
}

NvBool NV_API_CALL nv_acpi_is_battery_present(void)
{
    bool ac = true, haveBattery = false;
    nv_xnu_read_power_source(&ac, &haveBattery);
    return haveBattery ? NV_TRUE : NV_FALSE;
}

}
