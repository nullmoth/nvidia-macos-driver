/*
 * FreeBSD firmware(9) on xnu: an image is a GPU firmware file read from /Library/NullMoth/firmware by its Linux name
 * (see nmx_xnu_firmware_load). Images are reference counted and freed when the last holder puts them.
 */
#pragma once
#include <sys/types.h>
struct firmware {
    const char *name;
    const void *data;
    size_t datasize;
    unsigned int version;
    const struct firmware *parent;
};
#define FIRMWARE_UNLOAD 0x0001
#define FIRMWARE_GET_NOWARN 0x0001
const struct firmware *firmware_get(const char *imagename);
const struct firmware *firmware_get_flags(const char *imagename, uint32_t flags);
void firmware_put(const struct firmware *fp, int flags);
