/* FreeBSD <sys/backlight.h>: panel backlight records (FreeBSD's layout; on macOS brightness reaches them through the kext) */
#pragma once
#include <sys/types.h>
#include <sys/ioccom.h>
#define BACKLIGHTMAXLEVELS 100
struct backlight_props {
    uint32_t brightness;
    uint32_t nlevels;
    uint32_t levels[BACKLIGHTMAXLEVELS];
};
enum backlight_info_type {
    BACKLIGHT_TYPE_PANEL = 0,
    BACKLIGHT_TYPE_KEYBOARD
};
#define BACKLIGHTMAXNAMELENGTH 64
struct backlight_info {
    char name[BACKLIGHTMAXNAMELENGTH];
    enum backlight_info_type type;
};
#define BACKLIGHTGETSTATUS _IOWR('G', 0, struct backlight_props)
#define BACKLIGHTUPDATESTATUS _IOWR('G', 1, struct backlight_props)
#define BACKLIGHTGETINFO _IOWR('G', 2, struct backlight_info)
