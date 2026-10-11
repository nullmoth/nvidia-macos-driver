/* FreeBSD <sys/memrange.h>: MTRR control. On macOS write-combining comes from the mapping attribute (PAT), so no MTRR is ever programmed. */
#pragma once
struct mem_range_desc { uint64_t mr_base; uint64_t mr_len; int mr_flags; char mr_owner[8]; };
#define MDF_WRITECOMBINE 0x00000004
#define MEMRANGE_SET_UPDATE 0
#define MEMRANGE_SET_REMOVE 1
#define mem_range_attr_set(d, a) EOPNOTSUPP
