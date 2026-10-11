/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Linux gen_pool for this port: a first-fit bitmap allocator over the address ranges added to the pool, in units of
 * 1 << min_alloc_order bytes. Intel's NPU driver carves its IPC transmit buffers out of one NPU-visible buffer with it.
 * Written for the port (LinuxKPI has none); the interface is Linux's.
 */
#ifndef _NMX_LINUX_GENALLOC_H_
#define _NMX_LINUX_GENALLOC_H_

#include <linux/types.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/bitmap.h>
#include <linux/device.h>
#include <linux/err.h>

struct gen_pool_chunk {
	struct gen_pool_chunk *next;
	unsigned long start;
	size_t units;
	unsigned long bits[];
};

struct gen_pool {
	spinlock_t lock;
	int min_alloc_order;
	struct gen_pool_chunk *chunks;
};

static inline struct gen_pool *gen_pool_create(int min_alloc_order, int nid)
{
	struct gen_pool *p = kzalloc(sizeof(*p), GFP_KERNEL);
	if (p == NULL)
		return NULL;
	spin_lock_init(&p->lock);
	p->min_alloc_order = min_alloc_order;
	return p;
}

static inline void gen_pool_destroy(struct gen_pool *p)
{
	while (p != NULL && p->chunks != NULL) {
		struct gen_pool_chunk *c = p->chunks;
		p->chunks = c->next;
		kfree(c);
	}
	kfree(p);
}

static inline void nmx_gen_pool_release(void *p)
{
	gen_pool_destroy(p);
}

static inline struct gen_pool *devm_gen_pool_create(struct device *dev, int min_alloc_order, int nid, const char *name)
{
	struct gen_pool *p = gen_pool_create(min_alloc_order, nid);
	if (p == NULL)
		return ERR_PTR(-ENOMEM);
	if (devm_add_action_or_reset(dev, nmx_gen_pool_release, p))
		return ERR_PTR(-ENOMEM);
	return p;
}

static inline int gen_pool_add(struct gen_pool *p, unsigned long addr, size_t size, int nid)
{
	size_t units = size >> p->min_alloc_order;
	struct gen_pool_chunk *c = kzalloc(sizeof(*c) + BITS_TO_LONGS(units) * sizeof(unsigned long), GFP_KERNEL);
	unsigned long flags;

	if (c == NULL)
		return -ENOMEM;
	c->start = addr;
	c->units = units;
	spin_lock_irqsave(&p->lock, flags);
	c->next = p->chunks;
	p->chunks = c;
	spin_unlock_irqrestore(&p->lock, flags);
	return 0;
}

/* the address of size bytes (rounded up to the pool's unit), or 0 when nothing fits */
static inline unsigned long gen_pool_alloc(struct gen_pool *p, size_t size)
{
	size_t need = (size + (1UL << p->min_alloc_order) - 1) >> p->min_alloc_order;
	unsigned long flags, addr = 0;

	if (need == 0)
		return 0;
	spin_lock_irqsave(&p->lock, flags);
	for (struct gen_pool_chunk *c = p->chunks; c != NULL && addr == 0; c = c->next) {
		for (size_t i = 0; i + need <= c->units; i++) {
			size_t j = 0;
			while (j < need && !test_bit(i + j, c->bits))
				j++;
			if (j == need) {
				for (j = 0; j < need; j++)
					__set_bit(i + j, c->bits);
				addr = c->start + (i << p->min_alloc_order);
				break;
			}
			i += j;		/* skip past the used unit that stopped the run */
		}
	}
	spin_unlock_irqrestore(&p->lock, flags);
	return addr;
}

static inline void gen_pool_free(struct gen_pool *p, unsigned long addr, size_t size)
{
	size_t n = (size + (1UL << p->min_alloc_order) - 1) >> p->min_alloc_order;
	unsigned long flags;

	spin_lock_irqsave(&p->lock, flags);
	for (struct gen_pool_chunk *c = p->chunks; c != NULL; c = c->next) {
		if (addr >= c->start && addr < c->start + (c->units << p->min_alloc_order)) {
			size_t first = (addr - c->start) >> p->min_alloc_order;
			for (size_t j = 0; j < n && first + j < c->units; j++)
				__clear_bit(first + j, c->bits);
			break;
		}
	}
	spin_unlock_irqrestore(&p->lock, flags);
}

#endif
