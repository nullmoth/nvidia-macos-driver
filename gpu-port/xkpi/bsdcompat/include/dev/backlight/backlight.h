/*
 * FreeBSD backlight(9): a driver registers a panel backlight and serves the backlight_get_status/update_status/
 * get_info device methods. FreeBSD exposes it as /dev/backlight/<name>; the port keeps a registry the kext reads to
 * offer the panel's brightness to macOS (nmx_backlight_next walks it).
 */
#pragma once
#include <sys/backlight.h>
#include <sys/bus.h>
struct cdev *backlight_register(const char *name, device_t dev);
int backlight_destroy(struct cdev *dev);
/* the registered backlight after 'prev' (NULL = first), and the device that serves its methods */
struct cdev *nmx_backlight_next(struct cdev *prev, device_t *devp);
