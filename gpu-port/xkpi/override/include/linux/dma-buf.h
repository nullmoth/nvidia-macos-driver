/*
 * Buffer sharing (dma-buf) declarations for the macOS port. Written for this port from the interface its MIT and BSD
 * users rely on (drm_prime.c, amdgpu_dma_buf.c, TTM); drm-kmod's own header for this is GPL and is not used.
 *
 * On macOS a kext cannot create file descriptors, so a dma-buf lives only inside the kernel: drivers can export,
 * attach, pin and map one, while dma_buf_fd() and dma_buf_get() report that descriptor passing is unsupported.
 */
#ifndef NMXKPI_LINUX_DMA_BUF_H
#define NMXKPI_LINUX_DMA_BUF_H
#include <linux/types.h>
#include <linux/list.h>
#include <linux/wait.h>
#include <linux/mutex.h>
#include <linux/dma-mapping.h>
#include <linux/dma-fence.h>
#include <linux/iosys-map.h>
#include <linux/scatterlist.h>
struct device;
struct module;
struct dma_resv;
struct vm_area_struct;
struct dma_buf;
struct dma_buf_attachment;

struct dma_buf_ops {
    bool cache_sgt_mapping;
    int (*attach)(struct dma_buf *, struct dma_buf_attachment *);
    void (*detach)(struct dma_buf *, struct dma_buf_attachment *);
    int (*pin)(struct dma_buf_attachment *);
    void (*unpin)(struct dma_buf_attachment *);
    struct sg_table *(*map_dma_buf)(struct dma_buf_attachment *, enum dma_data_direction);
    void (*unmap_dma_buf)(struct dma_buf_attachment *, struct sg_table *, enum dma_data_direction);
    void (*release)(struct dma_buf *);
    int (*begin_cpu_access)(struct dma_buf *, enum dma_data_direction);
    int (*end_cpu_access)(struct dma_buf *, enum dma_data_direction);
    int (*mmap)(struct dma_buf *, struct vm_area_struct *);
    int (*vmap)(struct dma_buf *, struct iosys_map *);
    void (*vunmap)(struct dma_buf *, struct iosys_map *);
};

struct dma_buf_attach_ops {
    bool allow_peer2peer;
    void (*move_notify)(struct dma_buf_attachment *);
};

struct dma_buf_poll_cb_t {
    struct dma_fence_cb cb;
    wait_queue_head_t *poll;
    __poll_t active;
};

struct dma_buf {
    size_t size;
    const struct dma_buf_ops *ops;
    void *priv;
    struct dma_resv *resv;
    struct list_head attachments;
    const char *exp_name;
    struct module *owner;
    struct list_head list_node;
    struct mutex lock;
    unsigned vmapping_counter;
    struct iosys_map vmap_ptr;
    wait_queue_head_t poll;
    struct dma_buf_poll_cb_t cb_excl, cb_shared;
    struct linux_file *file;        /* never set on macOS: there is no descriptor behind it */
    volatile u_int refcount;        /* references held by the kernel (get_dma_buf / dma_buf_put) */
};

struct dma_buf_attachment {
    struct dma_buf *dmabuf;
    struct device *dev;
    struct list_head node;
    struct sg_table *sgt;
    enum dma_data_direction dir;
    bool peer2peer;
    const struct dma_buf_attach_ops *importer_ops;
    void *importer_priv;
    void *priv;
};

struct dma_buf_export_info {
    const char *exp_name;
    struct module *owner;
    const struct dma_buf_ops *ops;
    size_t size;
    int flags;
    struct dma_resv *resv;
    void *priv;
};
#define DEFINE_DMA_BUF_EXPORT_INFO(name) struct dma_buf_export_info name = { .exp_name = KBUILD_MODNAME, .owner = NULL }

struct dma_buf *dma_buf_export(const struct dma_buf_export_info *exp_info);
int dma_buf_fd(struct dma_buf *dmabuf, int flags);
struct dma_buf *dma_buf_get(int fd);
void dma_buf_put(struct dma_buf *dmabuf);
void get_dma_buf(struct dma_buf *dmabuf);
struct dma_buf_attachment *dma_buf_attach(struct dma_buf *dmabuf, struct device *dev);
struct dma_buf_attachment *dma_buf_dynamic_attach(struct dma_buf *dmabuf, struct device *dev,
    const struct dma_buf_attach_ops *importer_ops, void *importer_priv);
void dma_buf_detach(struct dma_buf *dmabuf, struct dma_buf_attachment *attach);
int dma_buf_pin(struct dma_buf_attachment *attach);
void dma_buf_unpin(struct dma_buf_attachment *attach);
struct sg_table *dma_buf_map_attachment(struct dma_buf_attachment *attach, enum dma_data_direction dir);
struct sg_table *dma_buf_map_attachment_unlocked(struct dma_buf_attachment *attach, enum dma_data_direction dir);
void dma_buf_unmap_attachment(struct dma_buf_attachment *attach, struct sg_table *sgt, enum dma_data_direction dir);
void dma_buf_unmap_attachment_unlocked(struct dma_buf_attachment *attach, struct sg_table *sgt, enum dma_data_direction dir);
void dma_buf_move_notify(struct dma_buf *dmabuf);
int dma_buf_begin_cpu_access(struct dma_buf *dmabuf, enum dma_data_direction dir);
int dma_buf_end_cpu_access(struct dma_buf *dmabuf, enum dma_data_direction dir);
int dma_buf_mmap(struct dma_buf *dmabuf, struct vm_area_struct *vma, unsigned long pgoff);
int dma_buf_vmap(struct dma_buf *dmabuf, struct iosys_map *map);
void dma_buf_vunmap(struct dma_buf *dmabuf, struct iosys_map *map);
int dma_buf_vmap_unlocked(struct dma_buf *dmabuf, struct iosys_map *map);
void dma_buf_vunmap_unlocked(struct dma_buf *dmabuf, struct iosys_map *map);
static inline bool dma_buf_is_dynamic(struct dma_buf *dmabuf) { return dmabuf->ops->pin != NULL; }
static inline bool dma_buf_attachment_is_dynamic(struct dma_buf_attachment *attach) { return attach->importer_ops != NULL; }
#endif
