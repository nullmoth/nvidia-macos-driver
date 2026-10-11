/*
 * dma-buf for the macOS port, written for this port from the interface drm_prime.c, amdgpu_dma_buf.c and TTM use
 * (see override/include/linux/dma-buf.h; drm-kmod's own implementation is GPL and is not used).
 *
 * A dma-buf here is a kernel object only: it can be exported, referenced, attached, pinned, mapped for a device and
 * vmapped, with the exporter's reservation object as the lock, exactly as drivers expect. What macOS does not allow
 * a kext is the file descriptor behind it, so dma_buf_fd() and dma_buf_get() report that and nothing else changes.
 */
#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/err.h>
#include <linux/mm.h>
#include <linux/dma-buf.h>
#include <linux/dma-resv.h>
#include <linux/sync_file.h>

MALLOC_DEFINE(M_DMABUF, "dmabuf", "dma-buf objects");

/* exported buffers whose reservation object the port allocated (the exporter passed none) */
struct nmx_dma_buf {
    struct dma_buf buf;
    struct dma_resv own_resv;
    bool owns_resv;
};

struct dma_buf *dma_buf_export(const struct dma_buf_export_info *info)
{
    if (info == NULL || info->ops == NULL || info->ops->map_dma_buf == NULL || info->ops->unmap_dma_buf == NULL ||
        info->ops->release == NULL || info->size == 0)
        return ERR_PTR(-EINVAL);
    /* pin and unpin come as a pair: a dynamic exporter must provide both */
    if ((info->ops->pin == NULL) != (info->ops->unpin == NULL))
        return ERR_PTR(-EINVAL);
    struct nmx_dma_buf *n = kzalloc(sizeof(*n), GFP_KERNEL);
    if (n == NULL)
        return ERR_PTR(-ENOMEM);
    struct dma_buf *b = &n->buf;
    b->size = info->size;
    b->ops = info->ops;
    b->priv = info->priv;
    b->exp_name = info->exp_name;
    b->owner = info->owner;
    if (info->resv != NULL) {
        b->resv = info->resv;
    } else {
        dma_resv_init(&n->own_resv);
        n->owns_resv = true;
        b->resv = &n->own_resv;
    }
    INIT_LIST_HEAD(&b->attachments);
    INIT_LIST_HEAD(&b->list_node);
    mutex_init(&b->lock);
    init_waitqueue_head(&b->poll);
    b->refcount = 1;
    return b;
}

int dma_buf_fd(struct dma_buf *dmabuf, int flags)
{
    return -EOPNOTSUPP;     /* a kext cannot create a file descriptor on macOS */
}

struct dma_buf *dma_buf_get(int fd)
{
    return ERR_PTR(-EBADF); /* no descriptor ever names a dma-buf on macOS */
}

void get_dma_buf(struct dma_buf *dmabuf)
{
    __atomic_fetch_add(&dmabuf->refcount, 1, __ATOMIC_ACQ_REL);
}

void dma_buf_put(struct dma_buf *dmabuf)
{
    if (dmabuf == NULL || IS_ERR(dmabuf))
        return;
    if (__atomic_sub_fetch(&dmabuf->refcount, 1, __ATOMIC_ACQ_REL) != 0)
        return;
    struct nmx_dma_buf *n = container_of(dmabuf, struct nmx_dma_buf, buf);
    WARN_ON(!list_empty(&dmabuf->attachments));
    WARN_ON(dmabuf->vmapping_counter != 0);
    dmabuf->ops->release(dmabuf);
    if (n->owns_resv)
        dma_resv_fini(&n->own_resv);
    mutex_destroy(&dmabuf->lock);
    kfree(n);
}

struct dma_buf_attachment *dma_buf_dynamic_attach(struct dma_buf *dmabuf, struct device *dev,
                                                  const struct dma_buf_attach_ops *importer_ops, void *importer_priv)
{
    if (dmabuf == NULL || dev == NULL)
        return ERR_PTR(-EINVAL);
    /* a dynamic importer must be told when the buffer moves */
    if (importer_ops != NULL && importer_ops->move_notify == NULL)
        return ERR_PTR(-EINVAL);
    struct dma_buf_attachment *a = kzalloc(sizeof(*a), GFP_KERNEL);
    if (a == NULL)
        return ERR_PTR(-ENOMEM);
    a->dmabuf = dmabuf;
    a->dev = dev;
    a->importer_ops = importer_ops;
    a->importer_priv = importer_priv;
    a->peer2peer = importer_ops != NULL && importer_ops->allow_peer2peer;
    if (dmabuf->ops->attach != NULL) {
        int r = dmabuf->ops->attach(dmabuf, a);
        if (r) {
            kfree(a);
            return ERR_PTR(r);
        }
    }
    dma_resv_lock(dmabuf->resv, NULL);
    list_add(&a->node, &dmabuf->attachments);
    dma_resv_unlock(dmabuf->resv);
    return a;
}

struct dma_buf_attachment *dma_buf_attach(struct dma_buf *dmabuf, struct device *dev)
{
    return dma_buf_dynamic_attach(dmabuf, dev, NULL, NULL);
}

void dma_buf_detach(struct dma_buf *dmabuf, struct dma_buf_attachment *a)
{
    if (dmabuf == NULL || a == NULL)
        return;
    dma_resv_lock(dmabuf->resv, NULL);
    list_del(&a->node);
    dma_resv_unlock(dmabuf->resv);
    if (dmabuf->ops->detach != NULL)
        dmabuf->ops->detach(dmabuf, a);
    kfree(a);
}

int dma_buf_pin(struct dma_buf_attachment *a)
{
    dma_resv_assert_held(a->dmabuf->resv);
    return a->dmabuf->ops->pin ? a->dmabuf->ops->pin(a) : 0;
}

void dma_buf_unpin(struct dma_buf_attachment *a)
{
    dma_resv_assert_held(a->dmabuf->resv);
    if (a->dmabuf->ops->unpin)
        a->dmabuf->ops->unpin(a);
}

/* an importer that cannot follow moves gets a buffer pinned for as long as it holds a mapping */
static bool needs_pin(struct dma_buf_attachment *a)
{
    return dma_buf_is_dynamic(a->dmabuf) && !dma_buf_attachment_is_dynamic(a);
}

struct sg_table *dma_buf_map_attachment(struct dma_buf_attachment *a, enum dma_data_direction dir)
{
    if (a == NULL || a->dmabuf == NULL)
        return ERR_PTR(-EINVAL);
    dma_resv_assert_held(a->dmabuf->resv);
    if (needs_pin(a)) {
        int r = a->dmabuf->ops->pin(a);
        if (r)
            return ERR_PTR(r);
    }
    struct sg_table *sgt = a->dmabuf->ops->map_dma_buf(a, dir);
    if (sgt == NULL)
        sgt = ERR_PTR(-ENOMEM);
    if (IS_ERR(sgt) && needs_pin(a))
        a->dmabuf->ops->unpin(a);
    return sgt;
}

struct sg_table *dma_buf_map_attachment_unlocked(struct dma_buf_attachment *a, enum dma_data_direction dir)
{
    dma_resv_lock(a->dmabuf->resv, NULL);
    struct sg_table *sgt = dma_buf_map_attachment(a, dir);
    dma_resv_unlock(a->dmabuf->resv);
    return sgt;
}

void dma_buf_unmap_attachment(struct dma_buf_attachment *a, struct sg_table *sgt, enum dma_data_direction dir)
{
    if (a == NULL || sgt == NULL || IS_ERR(sgt))
        return;
    dma_resv_assert_held(a->dmabuf->resv);
    a->dmabuf->ops->unmap_dma_buf(a, sgt, dir);
    if (needs_pin(a))
        a->dmabuf->ops->unpin(a);
}

void dma_buf_unmap_attachment_unlocked(struct dma_buf_attachment *a, struct sg_table *sgt, enum dma_data_direction dir)
{
    dma_resv_lock(a->dmabuf->resv, NULL);
    dma_buf_unmap_attachment(a, sgt, dir);
    dma_resv_unlock(a->dmabuf->resv);
}

void dma_buf_move_notify(struct dma_buf *dmabuf)
{
    struct dma_buf_attachment *a;
    dma_resv_assert_held(dmabuf->resv);
    list_for_each_entry(a, &dmabuf->attachments, node)
        if (a->importer_ops != NULL)
            a->importer_ops->move_notify(a);
}

int dma_buf_begin_cpu_access(struct dma_buf *dmabuf, enum dma_data_direction dir)
{
    return dmabuf->ops->begin_cpu_access ? dmabuf->ops->begin_cpu_access(dmabuf, dir) : 0;
}

int dma_buf_end_cpu_access(struct dma_buf *dmabuf, enum dma_data_direction dir)
{
    return dmabuf->ops->end_cpu_access ? dmabuf->ops->end_cpu_access(dmabuf, dir) : 0;
}

int dma_buf_mmap(struct dma_buf *dmabuf, struct vm_area_struct *vma, unsigned long pgoff)
{
    if (dmabuf == NULL || vma == NULL || dmabuf->ops->mmap == NULL)
        return -EINVAL;
    /* the mapping must lie inside the buffer */
    if (pgoff + vma_pages(vma) < pgoff || pgoff + vma_pages(vma) > (dmabuf->size >> PAGE_SHIFT))
        return -EINVAL;
    vma->vm_pgoff = pgoff;
    return dmabuf->ops->mmap(dmabuf, vma);
}

/* one kernel mapping is shared by every vmap caller and kept while any of them holds it */
int dma_buf_vmap(struct dma_buf *dmabuf, struct iosys_map *map)
{
    iosys_map_clear(map);
    if (dmabuf == NULL || dmabuf->ops->vmap == NULL)
        return -EINVAL;
    dma_resv_assert_held(dmabuf->resv);
    if (dmabuf->vmapping_counter) {
        dmabuf->vmapping_counter++;
        *map = dmabuf->vmap_ptr;
        return 0;
    }
    struct iosys_map ptr;
    iosys_map_clear(&ptr);
    int r = dmabuf->ops->vmap(dmabuf, &ptr);
    if (r)
        return r;
    if (iosys_map_is_null(&ptr))
        return -EINVAL;
    dmabuf->vmap_ptr = ptr;
    dmabuf->vmapping_counter = 1;
    *map = ptr;
    return 0;
}

void dma_buf_vunmap(struct dma_buf *dmabuf, struct iosys_map *map)
{
    if (dmabuf == NULL || map == NULL)
        return;
    dma_resv_assert_held(dmabuf->resv);
    if (WARN_ON(dmabuf->vmapping_counter == 0) || WARN_ON(!iosys_map_is_equal(&dmabuf->vmap_ptr, map)))
        return;
    if (--dmabuf->vmapping_counter == 0) {
        if (dmabuf->ops->vunmap)
            dmabuf->ops->vunmap(dmabuf, map);
        iosys_map_clear(&dmabuf->vmap_ptr);
    }
}

int dma_buf_vmap_unlocked(struct dma_buf *dmabuf, struct iosys_map *map)
{
    dma_resv_lock(dmabuf->resv, NULL);
    int r = dma_buf_vmap(dmabuf, map);
    dma_resv_unlock(dmabuf->resv);
    return r;
}

void dma_buf_vunmap_unlocked(struct dma_buf *dmabuf, struct iosys_map *map)
{
    dma_resv_lock(dmabuf->resv, NULL);
    dma_buf_vunmap(dmabuf, map);
    dma_resv_unlock(dmabuf->resv);
}

/*
 * sync_file: a fence behind a file descriptor. Every caller obtains the descriptor first (get_unused_fd_flags),
 * which a kext cannot do on macOS, so creating one is never reached; looking one up by descriptor finds nothing.
 */
struct sync_file *sync_file_create(struct dma_fence *fence)
{
    return NULL;
}

struct dma_fence *sync_file_get_fence(int fd)
{
    return NULL;
}

char *sync_file_get_name(struct sync_file *sync_file, char *buf, int len)
{
    if (len > 0)
        buf[0] = '\0';
    return buf;
}
