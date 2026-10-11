/* FreeBSD Capsicum does not exist on macOS. */
#pragma once
typedef struct { uint64_t cr_rights[2]; } cap_rights_t;
#define cap_rights_init(r, ...) (r)
extern cap_rights_t cap_no_rights;
#define CAP_READ 0x1ULL
#define CAP_WRITE 0x2ULL
#define CAP_MMAP 0x4ULL
