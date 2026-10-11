/*
 * Linux MMU notifiers: how a driver learns that CPU mappings it mirrored into the GPU changed.
 *
 * xnu has no such hook. The port's get_user_pages wires the pages it returns, so pages a GPU mapping refers to stay
 * allocated until the driver releases them; what cannot be seen is a process remapping that range afterwards (the GPU
 * would keep the old pages). So an interval notifier registers as accepted, its sequence never advances, and no
 * invalidation is ever delivered. Drivers then treat every userptr as valid until they unpin it themselves.
 */
#ifndef _NMX_LINUX_MMU_NOTIFIER_H_
#define _NMX_LINUX_MMU_NOTIFIER_H_

#include <linux/types.h>

struct mm_struct;
struct mmu_notifier {
};

enum mmu_notifier_event {
	MMU_NOTIFY_UNMAP = 0,
	MMU_NOTIFY_CLEAR,
	MMU_NOTIFY_PROTECTION_VMA,
	MMU_NOTIFY_PROTECTION_PAGE,
	MMU_NOTIFY_SOFT_DIRTY,
	MMU_NOTIFY_RELEASE,
	MMU_NOTIFY_MIGRATE,
	MMU_NOTIFY_EXCLUSIVE,
};

struct mmu_notifier_range {
	struct mm_struct *mm;
	unsigned long start;
	unsigned long end;
	unsigned flags;
	enum mmu_notifier_event event;
	void *owner;
};

#define MMU_NOTIFIER_RANGE_BLOCKABLE (1 << 0)

static inline bool mmu_notifier_range_blockable(const struct mmu_notifier_range *range)
{
	return (range->flags & MMU_NOTIFIER_RANGE_BLOCKABLE) != 0;
}

struct mmu_interval_notifier;

struct mmu_interval_notifier_ops {
	bool (*invalidate)(struct mmu_interval_notifier *interval_sub, const struct mmu_notifier_range *range,
			   unsigned long cur_seq);
};

struct mmu_interval_notifier {
	const struct mmu_interval_notifier_ops *ops;
	struct mm_struct *mm;
	unsigned long start;
	unsigned long last;
	unsigned long invalidate_seq;
};

static inline int mmu_interval_notifier_insert(struct mmu_interval_notifier *sub, struct mm_struct *mm,
					       unsigned long start, unsigned long length,
					       const struct mmu_interval_notifier_ops *ops)
{
	sub->ops = ops;
	sub->mm = mm;
	sub->start = start;
	sub->last = start + length - 1;
	sub->invalidate_seq = 2;	/* even: no invalidation in progress, and never one */
	return 0;
}

static inline void mmu_interval_notifier_remove(struct mmu_interval_notifier *sub)
{
	sub->mm = NULL;
}

static inline unsigned long mmu_interval_read_begin(struct mmu_interval_notifier *sub)
{
	return READ_ONCE(sub->invalidate_seq);
}

static inline bool mmu_interval_read_retry(struct mmu_interval_notifier *sub, unsigned long seq)
{
	return READ_ONCE(sub->invalidate_seq) != seq;
}

static inline bool mmu_interval_check_retry(struct mmu_interval_notifier *sub, unsigned long seq)
{
	return READ_ONCE(sub->invalidate_seq) != seq;
}

static inline void mmu_interval_set_seq(struct mmu_interval_notifier *sub, unsigned long cur_seq)
{
	WRITE_ONCE(sub->invalidate_seq, cur_seq);
}

/* registration with the mmap lock already held: the same, as nothing is registered with xnu */
static inline int mmu_interval_notifier_insert_locked(struct mmu_interval_notifier *sub, struct mm_struct *mm,
						      unsigned long start, unsigned long length,
						      const struct mmu_interval_notifier_ops *ops)
{
	return mmu_interval_notifier_insert(sub, mm, start, length, ops);
}
#endif
