/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// How large a user client's table of token mappings is. The table used to be allocated at its limit (4096 entries) by
// the first token mapping a client made, whatever it went on to hold. A client is cheap to open and any local user may
// hold thousands of them, so one mapping per client pinned the whole table for each: about 1.8 GB of kernel memory
// across the clients an unprivileged user can open. The table now starts small and doubles when it is full, so what a
// client costs follows the mappings it really holds. Pure, so tools/test_nvrm_token_table.py runs it on the host.
#pragma once
enum { kNvrmTokFirst = 8 };
// The capacity the table must have to take one more entry: `have` while there is room, the next size when it is full,
// 0 when `held` has reached `max` (the caller refuses the mapping).
static inline unsigned nvrm_tok_next_capacity(unsigned have, unsigned held, unsigned max)
{
    if (held >= max) return 0;
    if (held < have) return have;
    const unsigned next = have ? have * 2 : kNvrmTokFirst;
    return next > max || next < have ? max : next;
}
