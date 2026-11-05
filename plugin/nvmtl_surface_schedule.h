/* Chromium hands rendered IOSurfaces to another process from scheduled callbacks.
 * Publish these writes before reporting scheduled: the consumer has no Vulkan
 * queue dependency on this process, and shared VRAM can still be in flight. */
#pragma once
#include <string.h>

static inline int nvmtl_surface_schedule_after(const char *setting, const char *process)
{
    /* Preserve the existing explicit 0..3 diagnostic policy. */
    if (setting && *setting) return setting[0] - '0';
    if (process && (strstr(process, "Chrome") || strstr(process, "Chromium")
                    || !strncmp(process, "Microsoft Edge", 14))) return 1;
    return 3;
}
