/* FreeBSD <sys/power.h>: power-state change notifications; macOS delivers them through IOKit power management. */
#pragma once
#define POWER_PM_TYPE_ACPI 1
#define POWER_PM_TYPE_NONE 2
#define POWER_SLEEP_STATE_STANDBY 0
#define POWER_SLEEP_STATE_SUSPEND 1
#define POWER_SLEEP_STATE_HIBERNATE 2
#define power_pm_get_type() POWER_PM_TYPE_ACPI
/* power profile: the GPU drivers raise clocks on PERFORMANCE; macOS reports AC or battery power through IOPM */
#define POWER_PROFILE_PERFORMANCE 0
#define POWER_PROFILE_ECONOMY 1
int power_profile_get_state(void);
