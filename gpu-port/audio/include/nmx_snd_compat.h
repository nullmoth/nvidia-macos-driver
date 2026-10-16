/*
 * Kernel services the FreeBSD HD Audio driver uses that the FreeBSD-on-xnu layer (bsdcompat) does not carry, because
 * NMXGPU's drivers never needed them. Implemented in nmx_snd_compat.c. Force-included after bsdcompat's prelude.
 */
#ifndef NMX_SND_COMPAT_H
#define NMX_SND_COMPAT_H
#include <sys/types.h>
#include <sys/bus.h>
#include <sys/interrupt.h>
#include <machine/resource.h>
#include <sys/sysctl.h>
#include <sys/sbuf.h>

extern int snd_verbose;			/* FreeBSD's hw.snd.verbose; 0 unless the boot-arg nmxaudio_verbose sets it */

/* per-device sysctl nodes, under hw.nmxaudio.<nameunit> */
struct sysctl_ctx_list *device_get_sysctl_ctx(device_t dev);
struct sysctl_oid *device_get_sysctl_tree(device_t dev);

/* kernel environment hints (FreeBSD's hint.<name>.<unit>.<res>): none on macOS, every lookup misses */
int resource_int_value(const char *name, int unit, const char *resname, int *result);
int resource_string_value(const char *name, int unit, const char *resname, const char **result);

struct sbuf *sbuf_new_for_sysctl(struct sbuf *s, char *buf, int length, struct sysctl_req *req);
int device_set_descf(device_t dev, const char *fmt, ...) __printflike(2, 3);
int bus_print_child_header(device_t dev, device_t child);
int bus_print_child_footer(device_t dev, device_t child);
int bus_generic_suspend(device_t dev);
int bus_generic_resume(device_t dev);
int bus_generic_shutdown(device_t dev);

/* FreeBSD runs these after interrupts are enabled at boot; a kext starts later, so the hook runs at once */
struct intr_config_hook {
	void (*ich_func)(void *arg);
	void *ich_arg;
};
int config_intrhook_establish(struct intr_config_hook *hook);
void config_intrhook_disestablish(struct intr_config_hook *hook);

#define BUS_IVARS_PRIVATE 0x1000
int device_delete_children(device_t dev);
/* FreeBSD registers drivers with DRIVER_MODULE at module load; here each line leaves a record the kext registers with
 * the bus class at start (nmx_audio_kext.cpp): hdac on pci, hdacc on hdac, hdaa on hdacc, pcm on hdaa */
struct nmx_drvmod { const char *bus; driver_t *driver; };
#undef DRIVER_MODULE
#undef DRIVER_MODULE_ORDERED
#define DRIVER_MODULE(name, busname, driver, evh, arg) \
	struct nmx_drvmod nmx_drvmod_ ## name ## _ ## busname = { #busname, &driver };
#define DRIVER_MODULE_ORDERED(name, busname, driver, evh, arg, order) DRIVER_MODULE(name, busname, driver, evh, arg)
/* module metadata: the kext is one module, so FreeBSD's module dependencies have nothing to say */
#ifndef MODULE_DEPEND
#define MODULE_DEPEND(mod, dep, min, pref, max)
#endif
#ifndef MODULE_VERSION
#define MODULE_VERSION(mod, ver)
#endif
#endif
