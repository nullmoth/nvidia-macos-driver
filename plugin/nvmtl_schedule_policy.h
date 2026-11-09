/* nvmtl_schedule_policy.h - how much of a command buffer's surface writes are published before it reports "scheduled".
   Modes: 3 two-plane surfaces written through private images, 2 every private image, 1 every surface write, 0 none.
   Pure, so tools/test_schedule_policy.c proves it on the host. */
#ifndef NVMTL_SCHEDULE_POLICY_H
#define NVMTL_SCHEDULE_POLICY_H
#include <string.h>

/* The Chromium family hands layers to another process from its scheduled callbacks, and that process has no queue
   dependency on ours, so these processes publish every surface write first. */
static inline int nvmtl_schedule_is_chromium_family(const char *process)
{
    if (!process) return 0;
    return strstr(process, "Google Chrome") || strstr(process, "Chromium") || strstr(process, "Microsoft Edge");
}
/* `setting` is NVMTL_SCHEDULE_AFTER_SURFACE_WRITES (may be NULL). Only a single digit 0..3 is an override; anything else
   is ignored rather than turned into a mode number that no code path knows. */
static inline int nvmtl_schedule_after_mode(const char *setting, const char *process)
{
    if (setting && setting[0] >= '0' && setting[0] <= '3' && !setting[1]) return setting[0] - '0';
    return nvmtl_schedule_is_chromium_family(process) ? 1 : 3;
}
#endif
