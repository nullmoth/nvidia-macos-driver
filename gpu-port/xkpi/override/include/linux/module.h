/*
 * drm-kmod's LKPI_DRIVER_MODULE with the init function's result printed. The original calls init() and drops what it
 * returns, so a driver whose module init fails (i915, amdgpu, xe, the NPU drivers) leaves no trace in the kernel log:
 * the Intel iGPU logs only showed "no driver took 8086:<id>" with nothing saying i915 had never registered.
 */
#ifndef _NMX_OVERRIDE_LINUX_MODULE_H_
#define _NMX_OVERRIDE_LINUX_MODULE_H_

#include_next <linux/module.h>
#include <sys/systm.h>

#undef LKPI_DRIVER_MODULE
#define	LKPI_DRIVER_MODULE(mod, init, exit)				\
	static int mod##_evh(module_t m, int e, void *a)		\
	{								\
		int nmx_rc;						\
		switch (e) {						\
		case MOD_LOAD:						\
			nmx_rc = (init)();				\
			printf("NullMoth xkpi: module %s init %s (%d)\n", \
			    #mod, nmx_rc == 0 ? "ok" : "FAILED", nmx_rc); \
			break;						\
		case MOD_UNLOAD:					\
			(exit)();					\
			break;						\
		}							\
		return (0);						\
	}								\
	static moduledata_t mod##_md =					\
		{							\
		 .name = #mod,						\
		 .evhand = mod##_evh,					\
		};							\
	DECLARE_MODULE(mod, mod##_md, SI_SUB_DRIVERS, SI_ORDER_ANY);

#endif
