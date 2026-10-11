/* i915's GT debugfs registration: macOS has no debugfs, so there is nothing to register (CONFIG_DEBUG_FS is off) */
#include "i915_drv.h"
#include "gt/intel_gt_debugfs.h"

void intel_gt_debugfs_register(struct intel_gt *gt)
{
}

void intel_gt_debugfs_register_files(struct dentry *root, const struct intel_gt_debugfs_file *files,
                                     unsigned long count, void *data)
{
}
