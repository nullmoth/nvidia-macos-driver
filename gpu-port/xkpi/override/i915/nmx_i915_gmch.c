/*
 * The GMCH global GTT (Intel graphics before Gen6, reached through FreeBSD's agp(4) driver) for i915 on macOS. The
 * port drives Gen9 and later parts, whose GTT is i915's own; these entry points answer that no GMCH GTT exists, as
 * i915's own header does when it is built without that support.
 */
#include <linux/errno.h>
#include "i915_drv.h"
#include "gt/intel_ggtt_gmch.h"

void intel_ggtt_gmch_flush(void)
{
}

int intel_ggtt_gmch_enable_hw(struct drm_i915_private *i915)
{
    return -ENODEV;
}

int intel_ggtt_gmch_probe(struct i915_ggtt *ggtt)
{
    return -ENODEV;
}
