/*
 * SPDX-License-Identifier: MIT
 *
 * Copyright © 2021 Intel Corporation
 */

#include <drm/drm_drv.h>

#include "gem/i915_gem_context.h"
#include "gem/i915_gem_object.h"
#include "i915_active.h"
#include "i915_driver.h"
#include "i915_params.h"
#include "i915_pci.h"
#include "i915_perf.h"
#include "i915_request.h"
#include "i915_scheduler.h"
#include "i915_selftest.h"
#include "i915_vma.h"
#include "i915_vma_resource.h"

#ifdef __FreeBSD__
/*
 * intel_graphics_stolen_* are defined in sys/dev/pci/pcivar.h
 * and set at early boot from machdep.c. Copy over the values
 * here to a Linux resource struct.
 */
struct resource intel_graphics_stolen_res;
#endif

static int i915_check_nomodeset(void)
{
	bool use_kms = true;

	/*
	 * Enable KMS by default, unless explicitly overriden by
	 * either the i915.modeset parameter or by the
	 * nomodeset boot option.
	 */

	if (i915_modparams.modeset == 0)
		pr_warn("i915.modeset=0 is deprecated. Please use the 'nomodeset' kernel parameter instead.\n");
	else if (i915_modparams.modeset != -1)
		pr_warn("i915.modeset=%d is deprecated. Please remove it and the 'nomodeset' kernel parameter instead.\n",
			i915_modparams.modeset);

	if (i915_modparams.modeset == 0)
		use_kms = false;

	if (drm_firmware_drivers_only() && i915_modparams.modeset == -1)
		use_kms = false;

	if (!use_kms) {
		DRM_DEBUG_DRIVER("KMS disabled.\n");
		return -ENODEV;
	}

	return 0;
}

static const struct {
   int (*init)(void);
   void (*exit)(void);
} init_funcs[] = {
	{ .init = i915_check_nomodeset },
	{ .init = i915_active_module_init,
	  .exit = i915_active_module_exit },
	{ .init = i915_context_module_init,
	  .exit = i915_context_module_exit },
	{ .init = i915_gem_context_module_init,
	  .exit = i915_gem_context_module_exit },
	{ .init = i915_objects_module_init,
	  .exit = i915_objects_module_exit },
	{ .init = i915_request_module_init,
	  .exit = i915_request_module_exit },
	{ .init = i915_scheduler_module_init,
	  .exit = i915_scheduler_module_exit },
	{ .init = i915_vma_module_init,
	  .exit = i915_vma_module_exit },
	{ .init = i915_vma_resource_module_init,
	  .exit = i915_vma_resource_module_exit },
	{ .init = i915_mock_selftests },
	{ .init = i915_pmu_init,
	  .exit = i915_pmu_exit },
	{ .init = i915_pci_register_driver,
	  .exit = i915_pci_unregister_driver },
#ifdef __linux__
	{ .init = i915_perf_sysctl_register,
	  .exit = i915_perf_sysctl_unregister },
#endif
};
static int init_progress;

static int __init i915_init(void)
{
	int err, i;

#ifdef __FreeBSD__
#if defined(__amd64__)
	intel_graphics_stolen_res = (struct resource)
		DEFINE_RES_MEM(intel_graphics_stolen_base,
		    intel_graphics_stolen_size);
	DRM_INFO("Got Intel graphics stolen memory base 0x%x, size 0x%x\n",
	    intel_graphics_stolen_res.start,
	    resource_size(&intel_graphics_stolen_res));
#endif
#endif

	for (i = 0; i < ARRAY_SIZE(init_funcs); i++) {
		err = init_funcs[i].init();
		if (err != 0)
			printf("NullMoth i915: init step %d of %d returned %d\n", i, (int)ARRAY_SIZE(init_funcs), err);
		if (err < 0) {
			while (i--) {
				if (init_funcs[i].exit)
					init_funcs[i].exit();
			}
			return err;
		} else if (err > 0) {
			/*
			 * Early-exit success is reserved for things which
			 * don't have an exit() function because we have no
			 * idea how far they got or how to partially tear
			 * them down.
			 */
			WARN_ON(init_funcs[i].exit);
			break;
		}
	}

	init_progress = i;
	printf("NullMoth i915: %d of %d init steps ran\n", i, (int)ARRAY_SIZE(init_funcs));

	return 0;
}

static void __exit i915_exit(void)
{
	int i;

	for (i = init_progress - 1; i >= 0; i--) {
		GEM_BUG_ON(i >= ARRAY_SIZE(init_funcs));
		if (init_funcs[i].exit)
			init_funcs[i].exit();
	}
}

#ifdef __linux__
module_init(i915_init);
module_exit(i915_exit);
#endif

MODULE_AUTHOR("Tungsten Graphics, Inc.");
MODULE_AUTHOR("Intel Corporation");

MODULE_DESCRIPTION(DRIVER_DESC);
MODULE_LICENSE("GPL and additional rights");

/* BSD stuff */
#ifdef __FreeBSD__
LKPI_DRIVER_MODULE(i915kms, i915_init, i915_exit);
MODULE_DEPEND(i915kms, drmn, 2, 2, 2);
MODULE_DEPEND(i915kms, ttm, 1, 1, 1);
MODULE_DEPEND(i915kms, agp, 1, 1, 1);
MODULE_DEPEND(i915kms, linuxkpi, 1, 1, 1);
MODULE_DEPEND(i915kms, linuxkpi_video, 1, 1, 1);
MODULE_DEPEND(i915kms, dmabuf, 1, 1, 1);
MODULE_DEPEND(i915kms, firmware, 1, 1, 1);
#ifdef CONFIG_DEBUG_FS
MODULE_DEPEND(i915kms, lindebugfs, 1, 1, 1);
#endif
#endif
