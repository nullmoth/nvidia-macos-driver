/* FreeBSD firmware(9) on xnu: images are GPU firmware files read by nmx_xnu_firmware_load, reference counted. */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/firmware.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_NMXFW, "nmxfw", "firmware image records");

struct nmx_fw {
    struct firmware fw;             /* first: a const struct firmware * is a struct nmx_fw * */
    char name[128];
    void *data;
    size_t size;
    int refs;
    struct nmx_fw *next;
};
static struct nmx_fw *fw_list;
static struct mtx fw_mtx;
MTX_SYSINIT(nmx_fw, &fw_mtx, "firmware images", MTX_DEF);

const struct firmware *firmware_get_flags(const char *imagename, uint32_t flags)
{
    mtx_lock(&fw_mtx);
    for (struct nmx_fw *f = fw_list; f; f = f->next) {
        if (strcmp(f->name, imagename) == 0) {
            f->refs++;
            mtx_unlock(&fw_mtx);
            return &f->fw;
        }
    }
    mtx_unlock(&fw_mtx);

    void *data = NULL;
    size_t size = 0;
    int err = nmx_xnu_firmware_load(imagename, &data, &size);
    if (err) {
        if (!(flags & FIRMWARE_GET_NOWARN))
            printf("NullMoth xkpi: firmware %s not loaded (error %d); expected under /Library/NullMoth/firmware/\n",
                   imagename, err);
        return NULL;
    }
    struct nmx_fw *f = malloc(sizeof(*f), M_NMXFW, M_WAITOK | M_ZERO);
    if (f == NULL) {
        nmx_xnu_firmware_free(data, size);
        return NULL;
    }
    strlcpy(f->name, imagename, sizeof(f->name));
    f->data = data;
    f->size = size;
    f->refs = 1;
    f->fw.name = f->name;
    f->fw.data = data;
    f->fw.datasize = size;
    mtx_lock(&fw_mtx);
    f->next = fw_list;
    fw_list = f;
    mtx_unlock(&fw_mtx);
    return &f->fw;
}

const struct firmware *firmware_get(const char *imagename)
{
    return firmware_get_flags(imagename, 0);
}

void firmware_put(const struct firmware *fp, int flags)
{
    if (fp == NULL)
        return;
    struct nmx_fw *target = (struct nmx_fw *)(uintptr_t)fp;
    mtx_lock(&fw_mtx);
    for (struct nmx_fw **pp = &fw_list; *pp; pp = &(*pp)->next) {
        if (*pp == target) {
            if (--target->refs > 0) {
                mtx_unlock(&fw_mtx);
                return;
            }
            *pp = target->next;
            mtx_unlock(&fw_mtx);
            nmx_xnu_firmware_free(target->data, target->size);
            free(target, M_NMXFW);
            return;
        }
    }
    mtx_unlock(&fw_mtx);
}
