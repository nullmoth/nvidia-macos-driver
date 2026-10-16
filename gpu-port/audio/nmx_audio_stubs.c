/*
 * The FreeBSD-on-xnu layer's runtime (srcobj, xnuobj) also serves LinuxKPI drivers: its devfs nodes call LinuxKPI's
 * character devices, port attach can prepare a display, and thread records can carry a Linux task. NMXAudio links the
 * runtime without LinuxKPI and has none of those, so each entry point here refuses or does nothing.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>

struct task_struct;

int nmx_cdev_open(void *cookie, int open_id, int fflags) { return ENXIO; }
int nmx_cdev_close(int open_id, int fflags) { return 0; }
int nmx_cdev_ioctl(int open_id, unsigned long cmd, void *data, int fflag) { return ENOTTY; }
int nmx_cdev_read(int open_id, uint64_t uaddr, size_t len, size_t *done, int nonblock) { *done = 0; return ENXIO; }
int nmx_cdev_poll(int open_id, int events) { return 0; }
void nmx_fb_before_attach(void) { }
/* only reached for a thread that has a Linux task, which no thread here ever gets */
void nmx_task_get(struct task_struct *ts) { }
