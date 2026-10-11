/*
 * The port's in-memory sysctl tree (see sys/sysctl.h): LinuxKPI's sysfs and module parameters create nodes and
 * attribute leaves here. The tree is not yet mirrored into xnu's sysctl; diagnostics read it through the port.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/sysctl.h>
#include <sys/lock.h>
#include <sys/mutex.h>

MALLOC_DEFINE(M_SYSCTLOID, "sysctloid", "sysctl dynamic nodes");

struct sysctl_oid_list sysctl__children;
struct sysctl_oid_list sysctl__hw_children;
struct sysctl_oid_list sysctl__kern_children;
struct sysctl_oid_list sysctl__debug_children;
struct sysctl_oid_list sysctl__sys_children;
struct sysctl_oid_list sysctl__compat_children;
struct sysctl_oid_list sysctl__dev_children;
struct sysctl_oid_list sysctl__vm_children;

static struct mtx sysctl_tree_mtx;
MTX_SYSINIT(sysctl_tree, &sysctl_tree_mtx, "sysctl tree", MTX_DEF);

int sysctl_ctx_init(struct sysctl_ctx_list *clist)
{
    TAILQ_INIT(clist);
    return 0;
}

struct sysctl_oid *sysctl_add_oid(struct sysctl_ctx_list *clist, struct sysctl_oid_list *parent, int nbr,
                                  const char *name, int kind, void *arg1, intmax_t arg2,
                                  int (*handler)(SYSCTL_HANDLER_ARGS), const char *fmt, const char *descr,
                                  const char *label)
{
    if (parent == NULL || name == NULL)
        return NULL;
    mtx_lock(&sysctl_tree_mtx);
    struct sysctl_oid *o;
    SLIST_FOREACH(o, parent, oid_link) {
        if (strcmp(o->oid_name, name) == 0) {
            /* FreeBSD allows re-adding a node; it is shared */
            if ((o->oid_kind & CTLTYPE) == CTLTYPE_NODE && (kind & CTLTYPE) == CTLTYPE_NODE) {
                o->oid_refcnt++;
                mtx_unlock(&sysctl_tree_mtx);
                return o;
            }
            mtx_unlock(&sysctl_tree_mtx);
            printf("NullMoth xkpi: sysctl %s already exists\n", name);
            return NULL;
        }
    }
    mtx_unlock(&sysctl_tree_mtx);

    o = malloc(sizeof(*o), M_SYSCTLOID, M_WAITOK | M_ZERO);
    if (o == NULL)
        return NULL;
    size_t n = strlen(name) + 1;
    char *nm = malloc(n, M_SYSCTLOID, M_WAITOK);
    if (nm == NULL) {
        free(o, M_SYSCTLOID);
        return NULL;
    }
    memcpy(nm, name, n);
    SLIST_INIT(&o->oid_children);
    o->oid_parent = parent;
    o->oid_number = nbr;
    o->oid_kind = (u_int)kind | CTLFLAG_DYN;
    o->oid_arg1 = arg1;
    o->oid_arg2 = arg2;
    o->oid_name = nm;
    o->oid_handler = handler;
    o->oid_fmt = fmt;
    o->oid_descr = descr;
    o->oid_refcnt = 1;

    mtx_lock(&sysctl_tree_mtx);
    SLIST_INSERT_HEAD(parent, o, oid_link);
    mtx_unlock(&sysctl_tree_mtx);

    if (clist) {
        struct sysctl_ctx_entry *e = malloc(sizeof(*e), M_SYSCTLOID, M_WAITOK);
        if (e) {
            e->entry = o;
            TAILQ_INSERT_HEAD(clist, e, link);
        }
    }
    return o;
}

static void oid_free(struct sysctl_oid *o)
{
    free((void *)(uintptr_t)o->oid_name, M_SYSCTLOID);
    free(o, M_SYSCTLOID);
}

static int remove_locked(struct sysctl_oid *o, int del, int recurse)
{
    if ((o->oid_kind & CTLTYPE) == CTLTYPE_NODE && !SLIST_EMPTY(&o->oid_children)) {
        if (!recurse)
            return ENOTEMPTY;
        struct sysctl_oid *c;
        while ((c = SLIST_FIRST(&o->oid_children)) != NULL) {
            SLIST_REMOVE_HEAD(&o->oid_children, oid_link);
            c->oid_parent = NULL;
            (void)remove_locked(c, del, recurse);
        }
    }
    if (--o->oid_refcnt > 0)
        return 0;
    if (o->oid_parent)
        SLIST_REMOVE(o->oid_parent, o, sysctl_oid, oid_link);
    if (del)
        oid_free(o);
    return 0;
}

int sysctl_remove_oid(struct sysctl_oid *oidp, int del, int recurse)
{
    if (oidp == NULL)
        return EINVAL;
    mtx_lock(&sysctl_tree_mtx);
    int err = remove_locked(oidp, del, recurse);
    mtx_unlock(&sysctl_tree_mtx);
    return err;
}

int sysctl_remove_name(struct sysctl_oid *parent, const char *name, int del, int recurse)
{
    struct sysctl_oid *o, *found = NULL;
    mtx_lock(&sysctl_tree_mtx);
    SLIST_FOREACH(o, &parent->oid_children, oid_link)
        if (strcmp(o->oid_name, name) == 0) { found = o; break; }
    int err = found ? remove_locked(found, del, recurse) : ENOENT;
    mtx_unlock(&sysctl_tree_mtx);
    return err;
}

int sysctl_ctx_free(struct sysctl_ctx_list *clist)
{
    struct sysctl_ctx_entry *e;
    while ((e = TAILQ_FIRST(clist)) != NULL) {
        TAILQ_REMOVE(clist, e, link);
        (void)sysctl_remove_oid(e->entry, 1, 0);
        free(e, M_SYSCTLOID);
    }
    return 0;
}

void sysctl_rename_oid(struct sysctl_oid *oidp, const char *name)
{
    size_t n = strlen(name) + 1;
    char *nm = malloc(n, M_SYSCTLOID, M_WAITOK);
    if (nm == NULL)
        return;
    memcpy(nm, name, n);
    mtx_lock(&sysctl_tree_mtx);
    const char *old = oidp->oid_name;
    oidp->oid_name = nm;
    mtx_unlock(&sysctl_tree_mtx);
    free((void *)(uintptr_t)old, M_SYSCTLOID);
}

int sysctl_move_oid(struct sysctl_oid *oidp, struct sysctl_oid_list *parent)
{
    mtx_lock(&sysctl_tree_mtx);
    if (oidp->oid_parent)
        SLIST_REMOVE(oidp->oid_parent, oidp, sysctl_oid, oid_link);
    oidp->oid_parent = parent;
    SLIST_INSERT_HEAD(parent, oidp, oid_link);
    mtx_unlock(&sysctl_tree_mtx);
    return 0;
}

/* ---- value handlers (FreeBSD semantics: copy out the old value, then copy in a new one if given) ---------------- */

static int handle_scalar(void *ptr, intmax_t constval, size_t width, struct sysctl_req *req)
{
    uint64_t tmp = 0;
    void *src = ptr;
    if (src == NULL) {
        tmp = (uint64_t)constval;
        src = &tmp;
    }
    int err = SYSCTL_OUT(req, src, width);
    if (err || req->newptr == NULL)
        return err;
    if (ptr == NULL)
        return EPERM;
    return SYSCTL_IN(req, ptr, width);
}

int sysctl_handle_int(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(int), req); }
int sysctl_handle_bool(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(uint8_t), req); }
int sysctl_handle_8(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(uint8_t), req); }
int sysctl_handle_16(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(uint16_t), req); }
int sysctl_handle_32(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(uint32_t), req); }
int sysctl_handle_64(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(uint64_t), req); }
int sysctl_handle_long(SYSCTL_HANDLER_ARGS) { return handle_scalar(arg1, arg2, sizeof(long), req); }

int sysctl_handle_string(SYSCTL_HANDLER_ARGS)
{
    char *s = arg1;
    if (s == NULL)
        return EINVAL;
    int err = SYSCTL_OUT(req, s, strlen(s) + 1);
    if (err || req->newptr == NULL)
        return err;
    size_t max = arg2 > 0 ? (size_t)arg2 : 0;
    if (max == 0 || req->newlen >= max)
        return EINVAL;
    err = SYSCTL_IN(req, s, req->newlen);
    if (err == 0)
        s[req->newlen] = '\0';
    return err;
}

int sysctl_handle_opaque(SYSCTL_HANDLER_ARGS)
{
    int err = SYSCTL_OUT(req, arg1, (size_t)arg2);
    if (err || req->newptr == NULL)
        return err;
    return SYSCTL_IN(req, arg1, (size_t)arg2);
}

/* ---- reading and writing a leaf from inside the kernel (nmx_sysctl_find/read/write) ------------------------------- */

struct rw_ctx {
    char *buf;              /* NULL: the handler's output is counted and dropped (a write) */
    size_t max;
    const void *val;        /* a write's new value */
};

static int rw_out(struct sysctl_req *req, const void *p, size_t l)
{
    struct rw_ctx *c = req->oldptr;
    if (c->buf != NULL) {
        if (req->oldidx + l > c->max)
            return ENOMEM;
        memcpy(c->buf + req->oldidx, p, l);
    }
    req->oldidx += l;
    return 0;
}

static int rw_in(struct sysctl_req *req, void *p, size_t l)
{
    struct rw_ctx *c = req->oldptr;
    if (c->val == NULL)
        return EPERM;       /* a read carries no new value */
    if (req->newidx + l > req->newlen)
        return EINVAL;
    memcpy(p, (const char *)c->val + req->newidx, l);
    req->newidx += l;
    return 0;
}

static struct sysctl_oid *find_child(struct sysctl_oid_list *list, const char *name, size_t n)
{
    struct sysctl_oid *o;
    SLIST_FOREACH(o, list, oid_link)
        if (strncmp(o->oid_name, name, n) == 0 && o->oid_name[n] == '\0')
            return o;
    return NULL;
}

struct sysctl_oid *nmx_sysctl_find(struct sysctl_oid_list *list, const char *path)
{
    if (list == NULL || path == NULL)
        return NULL;
    /* the lock covers the walk only; the handlers run unlocked because a sysfs show/store may sleep */
    mtx_lock(&sysctl_tree_mtx);
    struct sysctl_oid *o = NULL;
    const char *seg = path;
    for (;;) {
        const char *slash = strchr(seg, '/');
        size_t n = slash ? (size_t)(slash - seg) : strlen(seg);
        o = n ? find_child(list, seg, n) : NULL;
        if (o == NULL || slash == NULL)
            break;
        if ((o->oid_kind & CTLTYPE) != CTLTYPE_NODE) {
            o = NULL;
            break;
        }
        list = SYSCTL_CHILDREN(o);
        seg = slash + 1;
    }
    mtx_unlock(&sysctl_tree_mtx);
    if (o == NULL || o->oid_handler == NULL || (o->oid_kind & CTLTYPE) == CTLTYPE_NODE)
        return NULL;
    return o;
}

/* runs the leaf's handler: c->buf receives its output (bounded by rw_out), c->val is the new value if any */
static int run(struct sysctl_oid *o, struct rw_ctx *c, size_t newlen, size_t *outlen)
{
    struct sysctl_req req = { 0 };
    req.oldptr = c;
    req.oldlen = c->max;
    req.oldfunc = rw_out;
    req.newfunc = rw_in;
    req.newptr = c->val;
    req.newlen = newlen;
    int err = o->oid_handler(o, o->oid_arg1, o->oid_arg2, &req);
    *outlen = req.oldidx;
    return err;
}

int nmx_sysctl_read(struct sysctl_oid *o, char *buf, size_t max, size_t *len)
{
    *len = 0;
    if (o == NULL || buf == NULL || max == 0)
        return EINVAL;
    struct rw_ctx c = { .buf = buf, .max = max - 1, .val = NULL };     /* room kept for the NUL */
    size_t n;
    int err = run(o, &c, 0, &n);
    if (err)
        return err;
    /* string handlers copy out their NUL too; the length reported is the text's */
    while (n > 0 && buf[n - 1] == '\0')
        n--;
    buf[n] = '\0';
    *len = n;
    return 0;
}

int nmx_sysctl_write(struct sysctl_oid *o, const void *val, size_t len)
{
    if (o == NULL || val == NULL || len == 0)
        return EINVAL;
    struct rw_ctx c = { .buf = NULL, .max = 0, .val = val };
    size_t ignored;
    return run(o, &c, len, &ignored);
}
