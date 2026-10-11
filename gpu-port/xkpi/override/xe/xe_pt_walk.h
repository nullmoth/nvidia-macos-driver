/* SPDX-License-Identifier: MIT */
/*
 * Page-table walker for xe's GPU page tables.
 *
 * Written for this port from the interface xe_pt.c uses (Linux's own version of this file is GPL-2.0 and is not
 * used). A walk visits, level by level, every entry of a multi-level table that an address range touches and hands
 * each one to the caller's callbacks, which decide whether to descend into the entry's child table.
 */
#ifndef _XE_PT_WALK_H_
#define _XE_PT_WALK_H_

#include <linux/types.h>

/* what to do with an entry after its callback: descend into its child, move on, or run the callback again */
#ifndef NMX_PAGE_WALK_ACTION
#define NMX_PAGE_WALK_ACTION
enum page_walk_action {
	ACTION_SUBTREE = 0,
	ACTION_CONTINUE = 1,
	ACTION_AGAIN = 2,
};
#endif

/* a node of the tree; a directory (level > 0) has one child slot per entry, live and staged */
struct xe_ptw {
	struct xe_ptw **children;
	struct xe_ptw **staging;
};

struct xe_pt_walk;

typedef int (*xe_pt_entry_fn)(struct xe_ptw *parent, pgoff_t offset, unsigned int level, u64 addr, u64 next,
			      struct xe_ptw **child, enum page_walk_action *action, struct xe_pt_walk *walk);

struct xe_pt_walk_ops {
	/* called for every entry the range touches, before any descent into it */
	xe_pt_entry_fn pt_entry;
	/* called for an entry after its subtree was walked (only when the entry's action was ACTION_SUBTREE) */
	xe_pt_entry_fn pt_post_descend;
};

struct xe_pt_walk {
	const struct xe_pt_walk_ops *ops;
	/* address bit at which each level's entries start: an entry at level l covers 1 << shifts[l] bytes */
	const u64 *shifts;
	unsigned int max_level;
	/* visit only the entries a range covers partly (those page tables other ranges share) */
	bool shared_pt_mode;
	/* hand callbacks the staged child slots instead of the live ones */
	bool staging;
};

/* the entry bytes at 'level' */
static inline u64 xe_pt_entry_size(unsigned int level, const struct xe_pt_walk *walk)
{
	return 1ull << walk->shifts[level];
}

/*
 * The index of addr's entry in its table at 'level'. A table's width is the gap between its shift and the next
 * level's (9 bits normally, 5 for the compact 64K layout); the top level is a full 512-entry table.
 */
static inline pgoff_t xe_pt_offset(u64 addr, unsigned int level, const struct xe_pt_walk *walk)
{
	unsigned int bits = level < walk->max_level ? walk->shifts[level + 1] - walk->shifts[level] : 9;
	return (addr >> walk->shifts[level]) & ((1ull << bits) - 1);
}

/* how many entries at 'level' the range [addr, end) touches, counting partly covered ones */
static inline pgoff_t xe_pt_num_entries(u64 addr, u64 end, unsigned int level, const struct xe_pt_walk *walk)
{
	u64 size = xe_pt_entry_size(level, walk);
	u64 first = addr & ~(size - 1);
	u64 last = (end + size - 1) & ~(size - 1);
	return (last - first) >> walk->shifts[level];
}

/* the end of the entry at 'level' that holds addr, or 'end' if the range stops first */
static inline u64 xe_pt_addr_end(u64 addr, u64 end, unsigned int level, const struct xe_pt_walk *walk)
{
	u64 size = xe_pt_entry_size(level, walk);
	u64 boundary = (addr & ~(size - 1)) + size;
	/* boundary - 1 < end - 1 also holds when boundary wrapped to 0 at the top of the address space */
	return (boundary - 1 < end - 1) ? boundary : end;
}

/* true when [addr, end) is exactly one whole entry at 'level' */
static inline bool xe_pt_covers(u64 addr, u64 end, unsigned int level, const struct xe_pt_walk *walk)
{
	u64 size = xe_pt_entry_size(level, walk);
	return (addr & (size - 1)) == 0 && end - addr == size;
}

/* true when [addr, end) touches 'level' entries it does not fully cover (so those tables are shared) */
static inline bool xe_pt_partial(u64 addr, u64 end, unsigned int level, const struct xe_pt_walk *walk)
{
	u64 size = xe_pt_entry_size(level, walk);
	return (addr & (size - 1)) != 0 || (end & (size - 1)) != 0;
}

int xe_pt_walk_range(struct xe_ptw *parent, unsigned int level, u64 addr, u64 end, struct xe_pt_walk *walk);

int xe_pt_walk_shared(struct xe_ptw *parent, unsigned int level, u64 addr, u64 end, struct xe_pt_walk *walk);

#endif
