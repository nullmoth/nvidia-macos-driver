/*
 * The kernel services nmx_snd_compat.h declares, for the FreeBSD HD Audio driver on xnu.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/kernel.h>
#include <stdarg.h>
#include "nmx_snd_compat.h"
#include "nmx_kobj.h"

int snd_verbose;
MALLOC_DEFINE(M_NMXSND, "nmxsnd", "FreeBSD sound port");

/* sysctl: one node per device, created on first use and kept for the device's lifetime (devices are few and live as
 * long as the kext) */
#define NMX_SND_MAXDEV 32
static struct { device_t dev; struct sysctl_ctx_list ctx; struct sysctl_oid *tree; } snd_sysctl[NMX_SND_MAXDEV];
static struct sysctl_ctx_list snd_root_ctx;
static struct sysctl_oid *snd_root;
static struct mtx snd_sysctl_mtx;
MTX_SYSINIT(nmx_snd_sysctl, &snd_sysctl_mtx, "nmxsnd sysctl", MTX_DEF);

static int snd_slot(device_t dev)
{
	int free_slot = -1;
	for (int i = 0; i < NMX_SND_MAXDEV; i++) {
		if (snd_sysctl[i].dev == dev)
			return i;
		if (free_slot < 0 && snd_sysctl[i].dev == NULL)
			free_slot = i;
	}
	if (free_slot < 0)
		return -1;
	if (snd_root == NULL) {
		sysctl_ctx_init(&snd_root_ctx);
		snd_root = SYSCTL_ADD_NODE(&snd_root_ctx, SYSCTL_STATIC_CHILDREN(_hw), OID_AUTO, "nmxaudio",
		    CTLFLAG_RD | CTLFLAG_MPSAFE, NULL, "HD Audio devices");
	}
	snd_sysctl[free_slot].dev = dev;
	sysctl_ctx_init(&snd_sysctl[free_slot].ctx);
	snd_sysctl[free_slot].tree = SYSCTL_ADD_NODE(&snd_sysctl[free_slot].ctx, SYSCTL_CHILDREN(snd_root), OID_AUTO,
	    device_get_nameunit(dev), CTLFLAG_RD | CTLFLAG_MPSAFE, NULL, "");
	return free_slot;
}

struct sysctl_ctx_list *device_get_sysctl_ctx(device_t dev)
{
	mtx_lock(&snd_sysctl_mtx);
	int i = snd_slot(dev);
	mtx_unlock(&snd_sysctl_mtx);
	return i < 0 ? &snd_root_ctx : &snd_sysctl[i].ctx;
}

struct sysctl_oid *device_get_sysctl_tree(device_t dev)
{
	mtx_lock(&snd_sysctl_mtx);
	int i = snd_slot(dev);
	mtx_unlock(&snd_sysctl_mtx);
	return i < 0 ? snd_root : snd_sysctl[i].tree;
}

int resource_int_value(const char *name, int unit, const char *resname, int *result) { return ENOENT; }
int resource_string_value(const char *name, int unit, const char *resname, const char **result) { return ENOENT; }

struct sbuf *sbuf_new_for_sysctl(struct sbuf *s, char *buf, int length, struct sysctl_req *req)
{
	/* the caller finishes with sbuf_finish + SYSCTL_OUT of the text; an auto-extending buffer is enough */
	return sbuf_new(s, buf, length, SBUF_AUTOEXTEND);
}

int device_set_descf(device_t dev, const char *fmt, ...)
{
	char buf[128];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);
	device_set_desc_copy(dev, buf);
	return 0;
}

int bus_print_child_header(device_t dev, device_t child) { return printf("%s", device_get_nameunit(child)); }
int bus_print_child_footer(device_t dev, device_t child) { return printf(" on %s\n", device_get_nameunit(dev)); }

static int call_children(device_t dev, const char *method)
{
	device_t *kids;
	int n, error = 0;
	if (device_get_children(dev, &kids, &n) != 0)
		return 0;
	for (int i = 0; i < n && error == 0; i++) {
		int (*fn)(device_t) = (int (*)(device_t))nmx_device_method(kids[i], method);
		if (fn != NULL)
			error = fn(kids[i]);
	}
	free(kids, M_TEMP);
	return error;
}
int bus_generic_suspend(device_t dev) { return call_children(dev, "device_suspend"); }
int bus_generic_resume(device_t dev) { return call_children(dev, "device_resume"); }
int bus_generic_shutdown(device_t dev) { return call_children(dev, "device_shutdown"); }

int config_intrhook_establish(struct intr_config_hook *hook)
{
	hook->ich_func(hook->ich_arg);
	return 0;
}
void config_intrhook_disestablish(struct intr_config_hook *hook) { }

int device_delete_children(device_t dev)
{
	device_t *kids;
	int n, error = 0;
	if (device_get_children(dev, &kids, &n) != 0)
		return 0;
	for (int i = n - 1; i >= 0 && error == 0; i--)
		error = device_delete_child(dev, kids[i]);
	free(kids, M_TEMP);
	return error;
}

#undef bus_get_dma_tag
bus_dma_tag_t nmx_snd_bus_get_dma_tag(device_t dev)
{
	typedef bus_dma_tag_t get_dma_tag_t(device_t, device_t);
	for (device_t child = dev, p = device_get_parent(dev); p != NULL; child = p, p = device_get_parent(p)) {
		get_dma_tag_t *f = (get_dma_tag_t *)nmx_device_method(p, "bus_get_dma_tag");
		if (f != NULL)
			return f(p, child);
	}
	return bus_get_dma_tag(dev);
}
