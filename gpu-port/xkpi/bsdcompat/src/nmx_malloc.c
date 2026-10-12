/*
 * FreeBSD malloc(9) and UMA zones on xnu.
 *
 * Small blocks (< one page): IOMalloc of the size plus room for a header and the alignment; the header in front of
 * the returned address records the raw allocation and the size, so free() needs no size. The returned address is
 * aligned to 16 bytes, or to the block's own size when that is a power of two up to a page: that is Linux kmalloc's
 * guarantee, and drivers rely on it (a 64-byte kmalloc is 64-byte aligned, a page-sized one page-aligned).
 * Large blocks (>= one page): IOMallocAligned on page boundaries, recorded in a table by address, so they stay
 * exactly page-aligned with no header in front.
 * Each malloc type counts the bytes it holds (ks_inuse); a type still holding memory at unload is reported.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <vm/uma.h>
#include <vm/vm.h>
#include <vm/vm_extern.h>
#include <kern/locks.h>

MALLOC_DEFINE(M_TEMP, "temp", "misc temporary data buffers");
MALLOC_DEFINE(M_DEVBUF, "devbuf", "device driver memory");

#define NMX_MAGIC 0x4e4d58414c4c4f43ULL   /* "NMXALLOC" */
struct nmx_hdr {
    uint64_t magic;
    void *raw;
    size_t rawsize;
    size_t size;
    struct malloc_type *type;
    uint64_t pad;               /* keeps the header a multiple of 16 bytes */
};
_Static_assert(sizeof(struct nmx_hdr) % 16 == 0, "header must keep 16-byte alignment");

#define BIG_BUCKETS 512
struct big_rec {
    void *addr;
    size_t size;
    struct malloc_type *type;
    int contig;                 /* from contigmalloc: pages of the page pool, not IOMallocAligned */
    struct big_rec *next;
};
static struct big_rec *big_table[BIG_BUCKETS];
static lck_spin_t *big_lock;

static unsigned big_bucket(const void *p)
{
    return (unsigned)(((uintptr_t)p >> PAGE_SHIFT) & (BIG_BUCKETS - 1));
}

static size_t natural_align(size_t size)
{
    if (size < 16 || size >= PAGE_SIZE || (size & (size - 1)) != 0)
        return 16;
    return size;
}

int nmx_malloc_init(void)
{
    big_lock = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    return big_lock ? 0 : ENOMEM;
}

static void account(struct malloc_type *type, int64_t delta)
{
    if (type)
        __atomic_fetch_add(&type->ks_inuse, (uint64_t)delta, __ATOMIC_RELAXED);
}

void *malloc(size_t size, struct malloc_type *type, int flags)
{
    if (size == 0)
        size = 1;
    if (size >= PAGE_SIZE) {
        size_t rounded = round_page(size);
        struct big_rec *rec = IOMalloc(sizeof(*rec));
        if (rec == NULL)
            return NULL;
        void *p = IOMallocAligned(rounded, PAGE_SIZE);
        if (p == NULL) {
            IOFree(rec, sizeof(*rec));
            return NULL;
        }
        if (flags & M_ZERO)
            bzero(p, rounded);
        rec->addr = p;
        rec->size = rounded;
        rec->type = type;
        rec->contig = 0;
        unsigned b = big_bucket(p);
        lck_spin_lock(big_lock);
        rec->next = big_table[b];
        big_table[b] = rec;
        lck_spin_unlock(big_lock);
        account(type, (int64_t)rounded);
        return p;
    }
    size_t align = natural_align(size);
    size_t rawsize = size + sizeof(struct nmx_hdr) + align;
    void *raw = IOMalloc(rawsize);
    if (raw == NULL)
        return NULL;
    uintptr_t user = ((uintptr_t)raw + sizeof(struct nmx_hdr) + align - 1) & ~(uintptr_t)(align - 1);
    struct nmx_hdr *h = (struct nmx_hdr *)(user - sizeof(struct nmx_hdr));
    h->magic = NMX_MAGIC;
    h->raw = raw;
    h->rawsize = rawsize;
    h->size = size;
    h->type = type;
    if (flags & M_ZERO)
        bzero((void *)user, size);
    account(type, (int64_t)size);
    return (void *)user;
}

static struct big_rec *big_take(void *addr)
{
    unsigned b = big_bucket(addr);
    lck_spin_lock(big_lock);
    for (struct big_rec **pp = &big_table[b]; *pp; pp = &(*pp)->next) {
        if ((*pp)->addr == addr) {
            struct big_rec *r = *pp;
            *pp = r->next;
            lck_spin_unlock(big_lock);
            return r;
        }
    }
    lck_spin_unlock(big_lock);
    return NULL;
}

static size_t big_size(const void *addr)
{
    size_t size = 0;
    unsigned b = big_bucket(addr);
    lck_spin_lock(big_lock);
    for (struct big_rec *r = big_table[b]; r; r = r->next)
        if (r->addr == addr) { size = r->size; break; }
    lck_spin_unlock(big_lock);
    return size;
}

void free(void *addr, struct malloc_type *type)
{
    if (addr == NULL)
        return;
    if (((uintptr_t)addr & PAGE_MASK) == 0) {
        struct big_rec *r = big_take(addr);
        if (r) {
            account(r->type, -(int64_t)r->size);
            if (r->contig)
                kmem_free(r->addr, r->size);
            else
                IOFreeAligned(r->addr, r->size);
            IOFree(r, sizeof(*r));
            return;
        }
        /* a page-aligned small block is possible (a 2 KB block can land on a page boundary): fall through */
    }
    struct nmx_hdr *h = (struct nmx_hdr *)((uintptr_t)addr - sizeof(struct nmx_hdr));
    if (h->magic != NMX_MAGIC)
        panic("NullMoth xkpi: free(%p) of memory malloc() did not return (type %s)", addr,
              type && type->ks_shortdesc ? type->ks_shortdesc : "?");
    h->magic = 0;
    account(h->type, -(int64_t)h->size);
    IOFree(h->raw, h->rawsize);
}

/*
 * contigmalloc blocks are recorded here too. FreeBSD's free() accepts them, and LinuxKPI relies on that: kmalloc of
 * more than a page is contigmalloc, kfree is free(). Without the record, freeing one panicked ("memory malloc() did
 * not return"): ivpu's firmware buffer vmap freed its page list that way at boot, on every start with the NPU on.
 */
int nmx_malloc_note_contig(void *addr, size_t size, struct malloc_type *type)
{
    struct big_rec *rec = IOMalloc(sizeof(*rec));
    if (rec == NULL)
        return ENOMEM;
    rec->addr = addr;
    rec->size = round_page(size);
    rec->type = type;
    rec->contig = 1;
    unsigned b = big_bucket(addr);
    lck_spin_lock(big_lock);
    rec->next = big_table[b];
    big_table[b] = rec;
    lck_spin_unlock(big_lock);
    account(type, (int64_t)rec->size);
    return 0;
}

/* contigfree: drops the record if there is one; the caller returns the pages */
void nmx_malloc_forget_contig(void *addr)
{
    struct big_rec *r = big_take(addr);
    if (r) {
        account(r->type, -(int64_t)r->size);
        IOFree(r, sizeof(*r));
    }
}

void zfree(void *addr, struct malloc_type *type)
{
    if (addr == NULL)
        return;
    bzero(addr, malloc_usable_size(addr));
    free(addr, type);
}

size_t malloc_usable_size(const void *addr)
{
    if (addr == NULL)
        return 0;
    if (((uintptr_t)addr & PAGE_MASK) == 0) {
        size_t s = big_size(addr);
        if (s)
            return s;
    }
    const struct nmx_hdr *h = (const struct nmx_hdr *)((uintptr_t)addr - sizeof(struct nmx_hdr));
    return h->magic == NMX_MAGIC ? h->size : 0;
}

size_t malloc_size(size_t size)
{
    return size >= PAGE_SIZE ? round_page(size) : size;
}

void *realloc(void *addr, size_t size, struct malloc_type *type, int flags)
{
    if (addr == NULL)
        return malloc(size, type, flags);
    size_t old = malloc_usable_size(addr);
    if (size <= old && size >= old / 2 && old < PAGE_SIZE)
        return addr;
    void *n = malloc(size, type, flags & ~M_ZERO);
    if (n == NULL)
        return NULL;
    memcpy(n, addr, old < size ? old : size);
    if ((flags & M_ZERO) && size > old)
        bzero((char *)n + old, size - old);
    free(addr, type);
    return n;
}

void *reallocf(void *addr, size_t size, struct malloc_type *type, int flags)
{
    void *n = realloc(addr, size, type, flags);
    if (n == NULL)
        free(addr, type);
    return n;
}

void *malloc_aligned(size_t size, size_t align, struct malloc_type *type, int flags)
{
    if (align <= 16 || (size >= PAGE_SIZE && align <= PAGE_SIZE))
        return malloc(size, type, flags);
    if (align <= PAGE_SIZE)
        return malloc(size < align ? align : roundup2(size, align), type, flags);
    panic("NullMoth xkpi: malloc_aligned(%zu, %zu): alignment above a page is not supported", size, align);
}

char *strdup_flags(const char *string, struct malloc_type *type, int flags)
{
    size_t n = strlen(string) + 1;
    char *c = malloc(n, type, flags);
    if (c)
        memcpy(c, string, n);
    return c;
}

/* ---- UMA zones: fixed-size allocators with constructor/destructor, as LinuxKPI's kmem_cache uses them ---------- */

struct uma_zone {
    const char *name;
    size_t size;
    int align;
    uma_ctor ctor;
    uma_dtor dtor;
    uma_init init;
    uma_fini fini;
    uint32_t flags;
    uma_import import;              /* cache zones: items come from the creator, not from the allocator */
    uma_release release;
    void *arg;
};

MALLOC_DEFINE(M_UMA, "uma", "UMA zone items");

uma_zone_t uma_zcreate(const char *name, size_t size, uma_ctor ctor, uma_dtor dtor, uma_init uminit, uma_fini fini,
                       int align, uint32_t flags)
{
    struct uma_zone *z = malloc(sizeof(*z), M_UMA, M_WAITOK | M_ZERO);
    if (z == NULL)
        return NULL;
    z->name = name;
    z->size = size;
    z->align = align;
    z->ctor = ctor;
    z->dtor = dtor;
    z->init = uminit;
    z->fini = fini;
    z->flags = flags;
    return z;
}

/*
 * A cache zone owns no memory: every item is obtained through import() and handed back through release(). FreeBSD
 * keeps released items in per-CPU buckets; here each allocation imports one item, which gives the same results.
 */
uma_zone_t uma_zcache_create(const char *name, int size, uma_ctor ctor, uma_dtor dtor, uma_init zinit, uma_fini zfini,
                             uma_import zimport, uma_release zrelease, void *arg, int flags)
{
    if (zimport == NULL || zrelease == NULL)
        return NULL;
    struct uma_zone *z = malloc(sizeof(*z), M_UMA, M_WAITOK | M_ZERO);
    if (z == NULL)
        return NULL;
    z->name = name;
    z->size = size < 0 ? 0 : (size_t)size;
    z->ctor = ctor;
    z->dtor = dtor;
    z->init = zinit;
    z->fini = zfini;
    z->import = zimport;
    z->release = zrelease;
    z->arg = arg;
    z->flags = (uint32_t)flags;
    return z;
}

void uma_zdestroy(uma_zone_t z)
{
    free(z, M_UMA);
}

void *uma_zalloc_arg(uma_zone_t z, void *arg, int flags)
{
    if (z->import) {
        void *it = NULL;
        if (z->import(z->arg, &it, 1, -1, flags) != 1 || it == NULL)
            return NULL;
        if (z->ctor && z->ctor(it, (int)z->size, arg, flags) != 0) {
            z->release(z->arg, &it, 1);
            return NULL;
        }
        return it;
    }
    size_t align = (size_t)z->align + 1;
    void *item = (align > 16) ? malloc_aligned(z->size, align, M_UMA, flags) : malloc(z->size, M_UMA, flags);
    if (item == NULL)
        return NULL;
    if ((z->flags & UMA_ZONE_ZINIT) || (flags & M_ZERO))
        bzero(item, z->size);
    if (z->init && z->init(item, (int)z->size, flags) != 0) {
        free(item, M_UMA);
        return NULL;
    }
    if (z->ctor && z->ctor(item, (int)z->size, arg, flags) != 0) {
        if (z->fini)
            z->fini(item, (int)z->size);
        free(item, M_UMA);
        return NULL;
    }
    return item;
}

void uma_zfree_arg(uma_zone_t z, void *item, void *arg)
{
    if (item == NULL)
        return;
    if (z->dtor)
        z->dtor(item, (int)z->size, arg);
    if (z->import) {
        z->release(z->arg, &item, 1);
        return;
    }
    if (z->fini)
        z->fini(item, (int)z->size);
    free(item, M_UMA);
}

void nmx_malloc_fini(void)
{
    if (big_lock == NULL)
        return;
    int leaked = 0;
    for (unsigned b = 0; b < BIG_BUCKETS; b++)
        for (struct big_rec *r = big_table[b]; r; r = r->next)
            leaked++;
    if (leaked)
        printf("NullMoth xkpi: %d large allocation(s) still held at unload\n", leaked);
    lck_spin_free(big_lock, nmx_lck_grp);
    big_lock = NULL;
}
