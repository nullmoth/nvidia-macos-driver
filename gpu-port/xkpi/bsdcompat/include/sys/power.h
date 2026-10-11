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

/*
 * Sleep types and the suspend/resume notifications (FreeBSD's names and values). The kext's IOKit power-state
 * changes invoke power_suspend_early / power_suspend / power_resume with the sleep type macOS is entering.
 */
#include <sys/eventhandler.h>
enum power_stype {
    POWER_STYPE_AWAKE,
    POWER_STYPE_STANDBY,
    POWER_STYPE_FW_SUSPEND,
    POWER_STYPE_SUSPEND_TO_IDLE,
    POWER_STYPE_FW_HIBERNATE,
    POWER_STYPE_POWEROFF,
    POWER_STYPE_UNKNOWN,
    POWER_STYPE_COUNT = POWER_STYPE_UNKNOWN,
};
typedef void (*power_change_fn)(void *, enum power_stype stype);
EVENTHANDLER_DECLARE(power_resume, power_change_fn);
EVENTHANDLER_DECLARE(power_suspend, power_change_fn);
EVENTHANDLER_DECLARE(power_suspend_early, power_change_fn);
