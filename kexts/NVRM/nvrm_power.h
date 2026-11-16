/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once

// Optional per-service manager. Its address stays stable from start() to free();
// stop() drains it before RM teardown and leaves it permanently disarmed.
class NVRMPower;
NVRMPower *nvrm_power_create(void);
void nvrm_power_start(NVRMPower *, nv_state_t *);
void nvrm_power_suspend(NVRMPower *);
void nvrm_power_resume(NVRMPower *);
void nvrm_power_stop(NVRMPower *);
void nvrm_power_release(NVRMPower *);
void nvrm_power_fini(void);
