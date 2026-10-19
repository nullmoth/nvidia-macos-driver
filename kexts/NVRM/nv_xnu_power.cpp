/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Where the power comes from, for the GPU firmware.
 *
 * A mobile GPU's firmware asks whether the machine is on mains or on battery and picks its clock and power policy
 * from the answer: it reads it while starting and again on every wake, and feeds it to RmPowerSourceChangeEvent.
 * Both calls used to land on a stub that said "not supported", so on a laptop the firmware ran its mains policy no
 * matter what the machine was doing, and the boot log said so once per start.
 *
 * The answer comes from the same place the rest of the system reads it: the battery service in the registry
 * publishes ExternalConnected, true while the charger is attached. A machine with no such service has no battery,
 * which is itself the answer.
 *
 * The firmware asks from inside its own locks. Its source says so where it asks on resume: "As we have already
 * acquired API and GPU lock here". Walking the registry there would take IOKit's locks underneath the firmware's,
 * which is how a wake hangs. So the registry is walked only from this driver's own safe points -- starting up, and
 * the power-state change this driver handles itself -- and what the firmware calls is a read of what was found.
 */

#include "nv-xnu.h"
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <libkern/OSAtomic.h>

// The services that publish a battery. AppleSmartBattery is what a Mac laptop has; IOPMPowerSource is the generic
// class it is a kind of, and looking for that as well keeps this working where the battery comes up under another
// driver.
static const char *const kBatteryServices[] = { "AppleSmartBattery", "IOPMPowerSource" };

// What the last walk found. Written only by nv_xnu_power_refresh (this driver's own threads), read by the firmware.
// One word each, so a reader never sees half an update.
static volatile int gPowerAcPlugged = 1;        // assume mains until a battery says otherwise
static volatile int gPowerHaveBattery = 0;
static volatile int gPowerKnown = 0;

// Walk the registry and remember what it says. Safe points only: never with the GPU firmware's locks held.
extern "C" void nv_xnu_power_refresh(void)
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
            if (OSBoolean *b = OSDynamicCast(OSBoolean, v)) { found = true; ac = b->isTrue(); }
            if (v) v->release();
            if (found) break;
        }
        it->release();
    }
    const int wasKnown = gPowerKnown, wasAc = gPowerAcPlugged;
    gPowerHaveBattery = found ? 1 : 0;
    gPowerAcPlugged = found ? (ac ? 1 : 0) : 1;   // no battery at all: this machine runs on mains
    gPowerKnown = 1;
    if (!wasKnown || wasAc != gPowerAcPlugged)
        IOLog("NVRM-xnu: power source: %s (battery %s)\n", gPowerAcPlugged ? "mains" : "battery",
              gPowerHaveBattery ? "present" : "none - desktop");
}

// What the firmware's two questions are answered from. No allocation, no locks, no registry: just what the last
// walk found, so this is safe to call from inside the firmware's own locks.
extern "C" void nv_xnu_power_state(bool *acPlugged, bool *haveBattery)
{
    if (acPlugged) *acPlugged = gPowerAcPlugged != 0;
    if (haveBattery) *haveBattery = gPowerHaveBattery != 0;
}
