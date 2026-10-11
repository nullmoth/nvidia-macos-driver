/*
 * sync_file declarations for the macOS port, written for this port (drm-kmod's header is GPL and not used).
 * A sync_file wraps a fence in a file descriptor; kexts cannot create descriptors on macOS, so creating one fails
 * cleanly and looking one up finds no fence.
 */
#ifndef NMXKPI_LINUX_SYNC_FILE_H
#define NMXKPI_LINUX_SYNC_FILE_H
#include <linux/types.h>
#include <linux/file.h>
#include <linux/dma-fence.h>
#include <linux/dma-fence-array.h>
struct sync_file {
    struct linux_file *file;
    char user_name[32];
    unsigned long flags;
    struct dma_fence *fence;
    struct dma_fence_cb cb;
};
struct sync_file *sync_file_create(struct dma_fence *fence);
struct dma_fence *sync_file_get_fence(int fd);
char *sync_file_get_name(struct sync_file *sync_file, char *buf, int len);
#endif
