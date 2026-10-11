/* FreeBSD <sys/backlight.h>: backlight devices; macOS drives panel brightness through IODisplay parameters. */
#pragma once
#include <stdint.h>
#define BACKLIGHTMAXLEVELS 100
struct backlight_props { uint32_t brightness; uint32_t nlevels; uint32_t levels[BACKLIGHTMAXLEVELS]; };
struct backlight_info { char name[64]; uint32_t type; };
enum backlight_info_type { BACKLIGHT_TYPE_PANEL = 0, BACKLIGHT_TYPE_KEYBOARD };
