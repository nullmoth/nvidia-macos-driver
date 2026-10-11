/*
 * Linux mempool: an allocator that keeps min_nr elements in reserve so an allocation can still succeed when the
 * general allocator cannot (xe's GuC relay answers the GuC from interrupt context with it). Allocation tries the
 * general allocator first and falls back to the reserve; a free refills the reserve before releasing memory.
 * Only the kmalloc-backed form is provided, the one the drivers in this port use.
 */
#ifndef _NMX_LINUX_MEMPOOL_H_
#define _NMX_LINUX_MEMPOOL_H_

#include <linux/slab.h>
#include <linux/spinlock.h>

typedef struct mempool {
	spinlock_t lock;
	int min_nr;
	int curr_nr;
	size_t size;
	void **elements;
} mempool_t;

static inline bool mempool_initialized(mempool_t *pool)
{
	return pool->elements != NULL;
}

static inline void mempool_exit(mempool_t *pool)
{
	if (pool->elements == NULL)
		return;
	while (pool->curr_nr > 0)
		kfree(pool->elements[--pool->curr_nr]);
	kfree(pool->elements);
	pool->elements = NULL;
}

static inline int mempool_init_kmalloc_pool(mempool_t *pool, int min_nr, size_t size)
{
	memset(pool, 0, sizeof(*pool));
	spin_lock_init(&pool->lock);
	pool->elements = kcalloc(min_nr > 0 ? min_nr : 1, sizeof(void *), GFP_KERNEL);
	if (pool->elements == NULL)
		return -ENOMEM;
	pool->min_nr = min_nr;
	pool->size = size;
	while (pool->curr_nr < min_nr) {
		void *e = kmalloc(size, GFP_KERNEL);
		if (e == NULL) {
			mempool_exit(pool);
			return -ENOMEM;
		}
		pool->elements[pool->curr_nr++] = e;
	}
	return 0;
}

static inline void *mempool_alloc(mempool_t *pool, gfp_t gfp)
{
	unsigned long flags;
	void *e = kmalloc(pool->size, gfp | __GFP_NOWARN);

	if (e != NULL)
		return e;
	spin_lock_irqsave(&pool->lock, flags);
	if (pool->curr_nr > 0)
		e = pool->elements[--pool->curr_nr];
	spin_unlock_irqrestore(&pool->lock, flags);
	return e;
}

static inline void mempool_free(void *e, mempool_t *pool)
{
	unsigned long flags;

	if (e == NULL)
		return;
	spin_lock_irqsave(&pool->lock, flags);
	if (pool->curr_nr < pool->min_nr) {
		pool->elements[pool->curr_nr++] = e;
		e = NULL;
	}
	spin_unlock_irqrestore(&pool->lock, flags);
	kfree(e);
}

#endif
