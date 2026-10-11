// SPDX-License-Identifier: MIT
/*
 * xe's module glue for this port: its parameters with their defaults, and the init/exit sequence that brings up the
 * driver's caches and registers it for PCI devices. Written for the port (Linux's xe_module.c is GPL-2.0).
 *
 * Which Intel GPUs xe drives here: Lunar Lake and Battlemage (xe probes them by default), and Panther Lake, which
 * 6.14 xe only probes when its device ids are forced, so the default force_probe list names them. Every older part
 * xe also knows (Tiger Lake through Arrow Lake, DG1/DG2) stays with i915, which already drives it in this kext;
 * the two drivers never claim the same device.
 */
#include <linux/module.h>
#include <linux/moduleparam.h>

#include <drm/drm_drv.h>

#include "xe_hw_fence.h"
#include "xe_module.h"
#include "xe_observation.h"
#include "xe_pci.h"
#include "xe_sched_job.h"

/* Panther Lake (Linux 6.14 pciids.h INTEL_PTL_IDS): forced so xe takes it; i915 does not know it */
#define NMX_XE_FORCE_PROBE "b080,b081,b082,b090,b091,b092,b0a0,b0a1,b0a2,b0b0"

struct xe_modparam xe_modparam = {
	.force_execlist = false,
	.probe_display = true,		/* xe lights the outputs; the port's display bridge needs that */
	.force_vram_bar_size = 0,	/* the BAR as the firmware sized it */
	.guc_log_level = 0,		/* no GuC log buffer: nothing on macOS reads it */
	.guc_firmware_path = NULL,
	.huc_firmware_path = NULL,
	.gsc_firmware_path = NULL,
	.force_probe = NMX_XE_FORCE_PROBE,
	.wedged_mode = 1,		/* wedge the device on a critical error rather than keep feeding a hung GPU */
};

module_param_named(force_execlist, xe_modparam.force_execlist, bool, 0444);
module_param_named(probe_display, xe_modparam.probe_display, bool, 0444);
module_param_named(vram_bar_size, xe_modparam.force_vram_bar_size, uint, 0600);
module_param_named(guc_log_level, xe_modparam.guc_log_level, int, 0600);
/* the string parameters (firmware paths, force_probe) keep their defaults: LinuxKPI has no string sysctl params */
module_param_named(wedged_mode, xe_modparam.wedged_mode, int, 0600);

/* drivers only take over a display when the firmware framebuffer is not the only one wanted (nomodeset) */
static int nmx_xe_check_modeset(void)
{
	return drm_firmware_drivers_only() ? -ENODEV : 0;
}

static const struct {
	int (*init)(void);
	void (*exit)(void);
} nmx_xe_steps[] = {
	{ nmx_xe_check_modeset, NULL },
	{ xe_hw_fence_module_init, xe_hw_fence_module_exit },
	{ xe_sched_job_module_init, xe_sched_job_module_exit },
	{ xe_register_pci_driver, xe_unregister_pci_driver },
	{ xe_observation_sysctl_register, xe_observation_sysctl_unregister },
};

static int nmx_xe_done;	/* how many steps succeeded, so exit undoes exactly those */

static void nmx_xe_exit(void)
{
	while (nmx_xe_done > 0) {
		nmx_xe_done--;
		if (nmx_xe_steps[nmx_xe_done].exit != NULL)
			nmx_xe_steps[nmx_xe_done].exit();
	}
}

static int nmx_xe_init(void)
{
	for (nmx_xe_done = 0; nmx_xe_done < (int)ARRAY_SIZE(nmx_xe_steps); nmx_xe_done++) {
		int err = nmx_xe_steps[nmx_xe_done].init();
		if (err) {
			printf("NullMoth xe: init step %d failed (%d)\n", nmx_xe_done, err);
			nmx_xe_exit();
			return err;
		}
	}
	return 0;
}

LKPI_DRIVER_MODULE(xe, nmx_xe_init, nmx_xe_exit);
MODULE_DEPEND(xe, drmn, 2, 2, 2);
MODULE_DEPEND(xe, ttm, 1, 1, 1);
MODULE_DEPEND(xe, linuxkpi, 1, 1, 1);
MODULE_DEPEND(xe, dmabuf, 1, 1, 1);
MODULE_DEPEND(xe, firmware, 1, 1, 1);
