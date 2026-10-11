/*
 * Walking the fences inside fence containers (arrays and chains), for the macOS port; written for this port
 * (drm-kmod's header is GPL and not used). Implemented in src/nmx_dma_fence_unwrap.c.
 */
#ifndef NMXKPI_LINUX_DMA_FENCE_UNWRAP_H
#define NMXKPI_LINUX_DMA_FENCE_UNWRAP_H
#include <linux/dma-fence.h>
struct dma_fence_unwrap {
    struct dma_fence *chain;    /* the chain being walked */
    struct dma_fence *array;    /* the array being walked inside it */
    unsigned int index;         /* next position in that array */
};
struct dma_fence *dma_fence_unwrap_first(struct dma_fence *head, struct dma_fence_unwrap *cursor);
struct dma_fence *dma_fence_unwrap_next(struct dma_fence_unwrap *cursor);
#define dma_fence_unwrap_for_each(fence, cursor, head) \
    for ((fence) = dma_fence_unwrap_first((head), (cursor)); (fence); (fence) = dma_fence_unwrap_next(cursor))
struct dma_fence *__dma_fence_unwrap_merge(unsigned int num_fences, struct dma_fence **fences, struct dma_fence_unwrap *cursors);
#define dma_fence_unwrap_merge(...) ({ struct dma_fence *__f[] = { __VA_ARGS__ }; \
    struct dma_fence_unwrap __c[sizeof(__f) / sizeof(__f[0])]; \
    __dma_fence_unwrap_merge(sizeof(__f) / sizeof(__f[0]), __f, __c); })
#endif
