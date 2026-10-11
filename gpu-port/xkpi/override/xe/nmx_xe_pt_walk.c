// SPDX-License-Identifier: MIT
/*
 * Page-table walker for xe's GPU page tables (see xe_pt_walk.h), written for this port.
 *
 * The contract, as xe_pt.c relies on it:
 *  - every entry of 'parent' (at 'level') that [addr, end) touches is handed to ops->pt_entry with the sub-range it
 *    covers, [addr, next), and a pointer to its child slot (staged or live, per walk->staging), so a callback may
 *    create the child; the action starts as ACTION_SUBTREE;
 *  - ACTION_SUBTREE descends into the child afterwards (directories only, and only when the child exists), then calls
 *    ops->pt_post_descend for the entry; ACTION_CONTINUE moves on; ACTION_AGAIN calls pt_entry again;
 *  - in shared mode, entries the range covers whole are skipped: only the tables other ranges also use are visited;
 *  - the first error a callback returns stops the walk and is returned.
 */
#include "xe_pt_walk.h"

static struct xe_ptw **child_slot(struct xe_ptw *parent, pgoff_t offset, unsigned int level,
				  const struct xe_pt_walk *walk, struct xe_ptw **scratch)
{
	struct xe_ptw **slots = walk->staging ? parent->staging : parent->children;

	/* a level-0 table holds PTEs, not children: callbacks get an empty slot of their own */
	if (level == 0 || slots == NULL) {
		*scratch = NULL;
		return scratch;
	}
	return &slots[offset];
}

int xe_pt_walk_range(struct xe_ptw *parent, unsigned int level, u64 addr, u64 end, struct xe_pt_walk *walk)
{
	while (addr < end) {
		u64 next = xe_pt_addr_end(addr, end, level, walk);
		pgoff_t offset = xe_pt_offset(addr, level, walk);
		struct xe_ptw *scratch, **child;
		enum page_walk_action action;
		int err;

		if (walk->shared_pt_mode && !xe_pt_partial(addr, next, level, walk))
			goto skip;

		child = child_slot(parent, offset, level, walk, &scratch);
		do {
			action = ACTION_SUBTREE;
			err = walk->ops->pt_entry(parent, offset, level, addr, next, child, &action, walk);
			if (err)
				return err;
		} while (action == ACTION_AGAIN);

		if (action == ACTION_SUBTREE) {
			if (level > 0 && *child != NULL) {
				err = xe_pt_walk_range(*child, level - 1, addr, next, walk);
				if (err)
					return err;
			}
			if (walk->ops->pt_post_descend) {
				err = walk->ops->pt_post_descend(parent, offset, level, addr, next, child, &action,
								 walk);
				if (err)
					return err;
			}
		}
skip:
		/* next == 0 means the entry ended at the top of the address space */
		if (next == 0)
			break;
		addr = next;
	}
	return 0;
}

/*
 * Visit the shared tables of [addr, end) under the root table 'parent' at 'level'. The root is handed to the
 * callbacks as the child of a virtual entry one level up, with parent == *child, so they treat it like any other
 * table (xe_pt.c recognises the root that way and never frees it).
 */
int xe_pt_walk_shared(struct xe_ptw *parent, unsigned int level, u64 addr, u64 end, struct xe_pt_walk *walk)
{
	struct xe_ptw *root = parent;
	enum page_walk_action action;
	int err;

	walk->shared_pt_mode = true;
	do {
		action = ACTION_SUBTREE;
		err = walk->ops->pt_entry(root, 0, level + 1, addr, end, &root, &action, walk);
		if (err)
			return err;
	} while (action == ACTION_AGAIN);

	if (action != ACTION_SUBTREE)
		return 0;
	err = xe_pt_walk_range(root, level, addr, end, walk);
	if (err)
		return err;
	if (walk->ops->pt_post_descend)
		err = walk->ops->pt_post_descend(root, 0, level + 1, addr, end, &root, &action, walk);
	return err;
}
