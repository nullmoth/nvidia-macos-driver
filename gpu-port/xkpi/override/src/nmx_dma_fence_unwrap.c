/*
 * Walking and merging the fences inside fence containers, for the macOS port; written for this port (drm-kmod's
 * dma-fence-unwrap.c is GPL-only and is not used). See override/include/linux/dma-fence-unwrap.h.
 *
 * A fence may be a chain (a timeline of links, each holding one fence) whose links hold arrays of fences; the walk
 * yields every plain fence inside, holding a reference on the chain link it is in. The merge collects the unsignaled
 * fences of several such trees, keeps only the latest fence of each fence context, and returns one fence that
 * signals when all of them have: the single remaining fence, a fence array, or the always-signaled stub.
 */
#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/dma-fence.h>
#include <linux/dma-fence-array.h>
#include <linux/dma-fence-chain.h>
#include <linux/dma-fence-unwrap.h>

/* the first fence of the chain link the cursor is on (the link itself when it holds a plain fence) */
static struct dma_fence *link_first(struct dma_fence_unwrap *cursor)
{
    cursor->array = dma_fence_chain_contained(cursor->chain);
    cursor->index = 0;
    return dma_fence_array_first(cursor->array);
}

struct dma_fence *dma_fence_unwrap_first(struct dma_fence *head, struct dma_fence_unwrap *cursor)
{
    cursor->chain = dma_fence_get(head);
    return link_first(cursor);
}

struct dma_fence *dma_fence_unwrap_next(struct dma_fence_unwrap *cursor)
{
    struct dma_fence *f = dma_fence_array_next(cursor->array, ++cursor->index);
    if (f != NULL)
        return f;
    /* the walk drops the reference on the link it leaves and takes one on the next */
    cursor->chain = dma_fence_chain_walk(cursor->chain);
    if (cursor->chain == NULL)
        return NULL;
    return link_first(cursor);
}

struct dma_fence *__dma_fence_unwrap_merge(unsigned int num_fences, struct dma_fence **fences,
                                           struct dma_fence_unwrap *cursors)
{
    struct dma_fence **out, *f, *result;
    unsigned int i, n = 0, count = 0;

    for (i = 0; i < num_fences; i++)
        dma_fence_unwrap_for_each(f, &cursors[i], fences[i])
            if (!dma_fence_is_signaled(f))
                count++;
    if (count == 0)
        return dma_fence_get_stub();

    out = kmalloc_array(count, sizeof(*out), GFP_KERNEL);
    if (out == NULL)
        return NULL;
    for (i = 0; i < num_fences; i++) {
        dma_fence_unwrap_for_each(f, &cursors[i], fences[i]) {
            unsigned int j;
            if (dma_fence_is_signaled(f))
                continue;
            for (j = 0; j < n; j++)
                if (out[j]->context == f->context)
                    break;
            if (j < n) {
                /* one fence per context: the later one implies the earlier */
                if (dma_fence_is_later(f, out[j])) {
                    dma_fence_put(out[j]);
                    out[j] = dma_fence_get(f);
                }
            } else if (n < count) {
                out[n++] = dma_fence_get(f);
            }
        }
    }

    if (n == 0) {
        kfree(out);
        return dma_fence_get_stub();
    }
    if (n == 1) {
        result = out[0];
        kfree(out);
        return result;
    }
    /* the array takes the fences and their references, and the storage */
    struct dma_fence_array *array = dma_fence_array_create((int)n, out, dma_fence_context_alloc(1), 1, false);
    if (array == NULL) {
        for (i = 0; i < n; i++)
            dma_fence_put(out[i]);
        kfree(out);
        return NULL;
    }
    return &array->base;
}
