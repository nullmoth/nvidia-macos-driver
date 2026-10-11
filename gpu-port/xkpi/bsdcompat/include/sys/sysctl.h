/*
 * FreeBSD sysctl(9) on xnu, as LinuxKPI's sysfs uses it.
 *
 * LinuxKPI builds sysfs (kobjects, attribute files) as a FreeBSD sysctl tree: a node's children live in oid_children
 * while oid_arg1 carries the kobject. xnu's struct sysctl_oid has no separate children list, so this port keeps its
 * own FreeBSD-shaped tree in memory (src/nmx_sysctl.c). xnu's <sys/sysctl.h> is deliberately not included; the kernel
 * functions that share FreeBSD's names are renamed to nmx_ ones so they never bind to xnu's exports.
 */
#ifndef NMXKPI_SYS_SYSCTL_H
#define NMXKPI_SYS_SYSCTL_H
#ifndef _SYS_SYSCTL_H_
#define _SYS_SYSCTL_H_  /* xnu's guard: keep its header out */
#endif
#include <sys/types.h>
#include <stdint.h>
#include <sys/queue.h>
#include <sys/counter.h>

struct thread;
struct sysctl_req;
struct sysctl_oid;
SLIST_HEAD(sysctl_oid_list, sysctl_oid);

#define SYSCTL_HANDLER_ARGS struct sysctl_oid *oidp, void *arg1, intmax_t arg2, struct sysctl_req *req

struct sysctl_oid {
    struct sysctl_oid_list oid_children;
    struct sysctl_oid_list *oid_parent;
    SLIST_ENTRY(sysctl_oid) oid_link;
    int oid_number;
    u_int oid_kind;
    void *oid_arg1;
    intmax_t oid_arg2;
    const char *oid_name;
    int (*oid_handler)(SYSCTL_HANDLER_ARGS);
    const char *oid_fmt;
    const char *oid_descr;
    int oid_refcnt;
};

struct sysctl_req {
    struct thread *td;
    int lock;
    void *oldptr;
    size_t oldlen;
    size_t oldidx;
    int (*oldfunc)(struct sysctl_req *, const void *, size_t);
    const void *newptr;
    size_t newlen;
    size_t newidx;
    int (*newfunc)(struct sysctl_req *, void *, size_t);
    size_t validlen;
    int flags;
};

struct sysctl_ctx_entry {
    struct sysctl_oid *entry;
    TAILQ_ENTRY(sysctl_ctx_entry) link;
};
TAILQ_HEAD(sysctl_ctx_list, sysctl_ctx_entry);

#define SYSCTL_IN(r, p, l) ((r)->newfunc)((r), (p), (l))
#define SYSCTL_OUT(r, p, l) ((r)->oldfunc)((r), (p), (l))
#define SYSCTL_OUT_STR(r, p) ((r)->oldfunc)((r), (p), strlen(p) + 1)

#define CTLTYPE 0xf
#define CTLTYPE_NODE 1
#define CTLTYPE_INT 2
#define CTLTYPE_STRING 3
#define CTLTYPE_S64 4
#define CTLTYPE_OPAQUE 5
#define CTLTYPE_STRUCT CTLTYPE_OPAQUE
#define CTLTYPE_UINT 6
#define CTLTYPE_LONG 7
#define CTLTYPE_ULONG 8
#define CTLTYPE_U64 9
#define CTLTYPE_U8 0xa
#define CTLTYPE_U16 0xb
#define CTLTYPE_S8 0xc
#define CTLTYPE_S16 0xd
#define CTLTYPE_S32 0xe
#define CTLTYPE_U32 0xf
#define CTLFLAG_RD 0x80000000
#define CTLFLAG_WR 0x40000000
#define CTLFLAG_RW (CTLFLAG_RD | CTLFLAG_WR)
#define CTLFLAG_DORMANT 0x20000000
#define CTLFLAG_ANYBODY 0x10000000
#define CTLFLAG_SECURE 0x08000000
#define CTLFLAG_PRISON 0x04000000
#define CTLFLAG_DYN 0x02000000
#define CTLFLAG_SKIP 0x01000000
#define CTLFLAG_TUN 0x00080000
#define CTLFLAG_RDTUN (CTLFLAG_RD | CTLFLAG_TUN)
#define CTLFLAG_RWTUN (CTLFLAG_RW | CTLFLAG_TUN)
#define CTLFLAG_MPSAFE 0x00040000
#define CTLFLAG_VNET 0x00020000
#define CTLFLAG_DYING 0x00010000
#define CTLFLAG_CAPRD 0x00008000
#define CTLFLAG_CAPWR 0x00004000
#define CTLFLAG_STATS 0x00002000
#define CTLFLAG_NOFETCH 0x00001000
#define CTLFLAG_CAPRW (CTLFLAG_CAPRD | CTLFLAG_CAPWR)
#define CTLFLAG_NEEDGIANT 0x00000800
#define OID_AUTO (-1)

#define sysctl_handle_int nmx_sysctl_handle_int
#define sysctl_handle_bool nmx_sysctl_handle_bool
#define sysctl_handle_8 nmx_sysctl_handle_8
#define sysctl_handle_16 nmx_sysctl_handle_16
#define sysctl_handle_32 nmx_sysctl_handle_32
#define sysctl_handle_64 nmx_sysctl_handle_64
#define sysctl_handle_long nmx_sysctl_handle_long
#define sysctl_handle_string nmx_sysctl_handle_string
#define sysctl_handle_opaque nmx_sysctl_handle_opaque
int sysctl_handle_int(SYSCTL_HANDLER_ARGS);
int sysctl_handle_bool(SYSCTL_HANDLER_ARGS);
int sysctl_handle_8(SYSCTL_HANDLER_ARGS);
int sysctl_handle_16(SYSCTL_HANDLER_ARGS);
int sysctl_handle_32(SYSCTL_HANDLER_ARGS);
int sysctl_handle_64(SYSCTL_HANDLER_ARGS);
int sysctl_handle_long(SYSCTL_HANDLER_ARGS);
int sysctl_handle_string(SYSCTL_HANDLER_ARGS);
int sysctl_handle_opaque(SYSCTL_HANDLER_ARGS);

int sysctl_ctx_init(struct sysctl_ctx_list *clist);
int sysctl_ctx_free(struct sysctl_ctx_list *clist);
struct sysctl_oid *sysctl_add_oid(struct sysctl_ctx_list *clist, struct sysctl_oid_list *parent, int nbr, const char *name,
    int kind, void *arg1, intmax_t arg2, int (*handler)(SYSCTL_HANDLER_ARGS), const char *fmt, const char *descr,
    const char *label);
int sysctl_remove_oid(struct sysctl_oid *oidp, int del, int recurse);
int sysctl_remove_name(struct sysctl_oid *parent, const char *name, int del, int recurse);
void sysctl_rename_oid(struct sysctl_oid *oidp, const char *name);
int sysctl_move_oid(struct sysctl_oid *oidp, struct sysctl_oid_list *parent);
/*
 * Port additions, for answering sysfs reads and writes from userland. nmx_sysctl_find returns the leaf at 'path'
 * ("name" or "node/name") under 'list', or NULL. The leaf must outlive the call: callers hold its owner (a device).
 * nmx_sysctl_read runs its handler into buf (NUL-terminated, *len without the NUL): 0, ENOMEM (longer than max) or the
 * handler's errno. nmx_sysctl_write hands it a new value of len bytes: 0 or the handler's errno.
 */
struct sysctl_oid *nmx_sysctl_find(struct sysctl_oid_list *list, const char *path);
int nmx_sysctl_read(struct sysctl_oid *o, char *buf, size_t max, size_t *len);
int nmx_sysctl_write(struct sysctl_oid *o, const void *val, size_t len);

#define SYSCTL_CHILDREN(oid_ptr) (&(oid_ptr)->oid_children)
/* the port's own sysctl tree (not xnu's): its root, which LinuxKPI hangs "sys" (its sysfs) from */
extern struct sysctl_oid_list sysctl__children;
#define SYSCTL_ADD_ROOT_NODE(ctx, nbr, name, access, handler, descr) \
    SYSCTL_ADD_NODE((ctx), &sysctl__children, (nbr), (name), (access), (handler), (descr))
#define SYSCTL_PARENT(oid_ptr) NULL
#define SYSCTL_FOREACH(oidp, list) SLIST_FOREACH((oidp), (list), oid_link)
#define SYSCTL_ADD_OID(ctx, parent, nbr, name, kind, a1, a2, handler, fmt, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), (kind), (a1), (a2), (handler), (fmt), (descr), NULL)
#define SYSCTL_ADD_NODE(ctx, parent, nbr, name, access, handler, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_NODE | (access), NULL, 0, (handler), "N", (descr), NULL)
#define SYSCTL_ADD_NODE_WITH_LABEL(ctx, parent, nbr, name, access, handler, descr, label) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_NODE | (access), NULL, 0, (handler), "N", (descr), (label))
#define SYSCTL_ADD_PROC(ctx, parent, nbr, name, access, ptr, arg, handler, fmt, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), (access), (ptr), (arg), (handler), (fmt), (descr), NULL)
#define SYSCTL_ADD_INT(ctx, parent, nbr, name, access, ptr, val, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_INT | (access), (ptr), (val), sysctl_handle_int, "I", (descr), NULL)
#define SYSCTL_ADD_UINT(ctx, parent, nbr, name, access, ptr, val, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_UINT | (access), (ptr), (val), sysctl_handle_int, "IU", (descr), NULL)
#define SYSCTL_ADD_LONG(ctx, parent, nbr, name, access, ptr, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_LONG | (access), (ptr), 0, sysctl_handle_long, "L", (descr), NULL)
#define SYSCTL_ADD_ULONG(ctx, parent, nbr, name, access, ptr, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_ULONG | (access), (ptr), 0, sysctl_handle_long, "LU", (descr), NULL)
#define SYSCTL_ADD_STRING(ctx, parent, nbr, name, access, arg, len, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_STRING | (access), (arg), (len), sysctl_handle_string, "A", (descr), NULL)
#define SYSCTL_ADD_BOOL(ctx, parent, nbr, name, access, ptr, val, descr) \
    sysctl_add_oid((ctx), (parent), (nbr), (name), CTLTYPE_U8 | (access), (ptr), (val), sysctl_handle_bool, "CU", (descr), NULL)

/*
 * Static (compile-time) sysctls: module parameters and fixed nodes. Their variables keep their compiled-in values;
 * the declarations exist so the names resolve, and roots used as parents are defined in src/nmx_sysctl.c.
 */
#define SYSCTL_DECL(name) extern struct sysctl_oid_list sysctl_##name##_children
#define SYSCTL_STATIC_CHILDREN(oid_name) (&sysctl_##oid_name##_children)
#define SYSCTL_NODE(parent, nbr, name, access, handler, descr) \
    struct sysctl_oid_list sysctl_##parent##_##name##_children
#define SYSCTL_NODE_WITH_LABEL(parent, nbr, name, access, handler, descr, label) \
    SYSCTL_NODE(parent, nbr, name, access, handler, descr)
#define SYSCTL_INT(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_UINT(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_LONG(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_ULONG(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_BOOL(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_STRING(parent, nbr, name, access, arg, len, descr) struct __hack
#define SYSCTL_PROC(parent, nbr, name, access, ptr, arg, handler, fmt, descr) struct __hack
#define SYSCTL_OPAQUE(parent, nbr, name, access, ptr, len, fmt, descr) struct __hack
#define SYSCTL_COUNTER_U64(parent, nbr, name, access, ptr, descr) struct __hack
#define SYSCTL_U8(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_U16(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_U32(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_U64(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_S8(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_S16(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_S32(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_S64(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_QUAD(parent, nbr, name, access, ptr, val, descr) struct __hack
#define SYSCTL_UQUAD(parent, nbr, name, access, ptr, val, descr) struct __hack

SYSCTL_DECL(_hw);
SYSCTL_DECL(_kern);
SYSCTL_DECL(_debug);
SYSCTL_DECL(_sys);
SYSCTL_DECL(_compat);
SYSCTL_DECL(_dev);
SYSCTL_DECL(_vm);

#endif
