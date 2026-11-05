#pragma once
#include "../nvrm_telemetry_abi.h"
class IOService;
struct NVRMSMCBridge;

NVRMSMCBridge *nvrmSMCCreate(IOService *owner);
// Called only on the existing telemetry work loop, outside the RM/cache lock.
void nvrmSMCTick(NVRMSMCBridge *, const NVRMTelemetry &, unsigned long long fanArrivalNs);
void nvrmSMCTemperature(NVRMSMCBridge *, unsigned celsius);
void nvrmSMCStop(NVRMSMCBridge *);
bool nvrmSMCCanUnload();
