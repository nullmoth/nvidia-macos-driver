/*
 * Device nodes for the port, LinuxKPI side: FreeBSD's make_dev(9) and per-open file records on top of the xnu
 * devfs glue (xnu/nmx_devfs.cpp), and the mmap ioctl.
 *
 * Every cdev made here is an xnu clone node. Each open of it gets its own minor ("open id") from the devfs glue,
 * a FreeBSD struct file, and the cdevsw's d_fdopen, exactly as FreeBSD's devfs would call it, so LinuxKPI's
 * linux_dev_fdopen builds its linux_file and installs linuxfileops. Later calls on that open (ioctl, read, poll,
 * close) go through the file's operations.
 *
 * mmap: the driver's own mmap and fault handlers run as they do on Linux, against a vm_area_struct the port owns
 * (struct nmx_umap). The fault handler's page inserts (vmf_insert_pfn_prot and friends, linux_page.c fork) land in
 * the record's page list instead of a page table; once every page is known the whole range is mapped into the
 * calling process at once (nmx_xnu_map_user). TTM buffers are pinned for the life of the mapping (the hook
 * drm_gem_ttm_mmap installs), because a user mapping made this way cannot be re-faulted if the buffer moves.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/conf.h>
#include <sys/file.h>
#include <sys/uio.h>
#include <sys/proc.h>
#include <sys/stdarg.h>

#include <linux/kernel.h>
#include <linux/mm.h>
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/rwsem.h>
#include <linux/sched.h>

#include "nmx_xnu.h"
#include "nmx_drm_uapi.h"

/* below, "file" is FreeBSD's struct file again (LinuxKPI renames it to linux_file); linux_compat.c does the same */
#undef file

MALLOC_DEFINE(M_NMXCDEV, "nmxcdev", "device nodes and their opens");

/* ---- FreeBSD make_dev(9) -------------------------------------------------------------------------------------------- */

static struct mtx devmtx;
MTX_SYSINIT(nmx_devmtx, &devmtx, "cdev list", MTX_DEF);

void dev_lock(void)
{
    mtx_lock(&devmtx);
}

void dev_unlock(void)
{
    mtx_unlock(&devmtx);
}

void make_dev_args_init_impl(struct make_dev_args *args, size_t sz)
{
    bzero(args, sz);
    args->mda_size = sz;
}

static int make_dev_va(struct make_dev_args *args, struct cdev **cdevp, const char *fmt, va_list ap)
{
    *cdevp = NULL;
    if (args->mda_size != sizeof(*args) || args->mda_devsw == NULL)
        return EINVAL;
    struct cdev *c = malloc(sizeof(*c), M_NMXCDEV, ((args->mda_flags & MAKEDEV_NOWAIT) ? M_NOWAIT : M_WAITOK) | M_ZERO);
    if (c == NULL)
        return ENOMEM;
    vsnprintf(c->si_name, sizeof(c->si_name), fmt, ap);
    c->si_devsw = args->mda_devsw;
    c->si_drv1 = args->mda_si_drv1;
    c->si_drv2 = args->mda_si_drv2;
    c->si_unit = args->mda_unit;
    c->si_uid = args->mda_uid;
    c->si_gid = args->mda_gid;
    c->si_mode = (mode_t)args->mda_mode;
    c->nmx_refs = 1;                    /* the creation reference, dropped by destroy_dev */

    dev_lock();
    LIST_INSERT_HEAD(&c->si_devsw->d_devs, c, si_list);
    dev_unlock();

    c->nmx_node = nmx_xnu_devfs_make(c->si_name, c->si_uid, c->si_gid, c->si_mode, c);
    if (c->nmx_node == NULL) {
        printf("NullMoth xkpi: could not create /dev/%s\n", c->si_name);
        dev_lock();
        LIST_REMOVE(c, si_list);
        dev_unlock();
        free(c, M_NMXCDEV);
        return ENXIO;
    }
    *cdevp = c;
    return 0;
}

int make_dev_s(struct make_dev_args *args, struct cdev **cdevp, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int err = make_dev_va(args, cdevp, fmt, ap);
    va_end(ap);
    return err;
}

struct cdev *make_dev(struct nmx_bsd_cdevsw *devsw, int unit, uid_t uid, gid_t gid, int perms, const char *fmt, ...)
{
    struct make_dev_args args;
    struct cdev *c;
    make_dev_args_init(&args);
    args.mda_devsw = devsw;
    args.mda_unit = unit;
    args.mda_uid = uid;
    args.mda_gid = gid;
    args.mda_mode = perms;
    va_list ap;
    va_start(ap, fmt);
    int err = make_dev_va(&args, &c, fmt, ap);
    va_end(ap);
    return err ? NULL : c;
}

void dev_ref(struct cdev *c)
{
    __atomic_fetch_add(&c->nmx_refs, 1, __ATOMIC_ACQ_REL);
}

void dev_rel(struct cdev *c)
{
    if (__atomic_sub_fetch(&c->nmx_refs, 1, __ATOMIC_ACQ_REL) == 0)
        free(c, M_NMXCDEV);
}

/*
 * The node disappears at once (no new opens); opens already made keep the cdev alive through their references and
 * finish against it, as FreeBSD's destroy_dev allows them to.
 */
void destroy_dev(struct cdev *c)
{
    if (c == NULL)
        return;
    void *node = c->nmx_node;
    c->nmx_node = NULL;
    __atomic_store_n(&c->nmx_gone, 1, __ATOMIC_RELEASE);
    if (node != NULL)
        nmx_xnu_devfs_remove(node);
    dev_lock();
    LIST_REMOVE(c, si_list);
    dev_unlock();
    dev_rel(c);
}

void destroy_dev_sched(struct cdev *c)
{
    destroy_dev(c);
}

void destroy_dev_drain(struct nmx_bsd_cdevsw *csw)
{
}

int dev2unit(struct cdev *c)
{
    return c->si_unit;
}

const char *devtoname(struct cdev *c)
{
    return c->si_name;
}

int dev2udev(struct cdev *c)
{
    return c->si_unit;
}

/* ---- opens ------------------------------------------------------------------------------------------------------------ */

struct nmx_umap;

struct nmx_open {
    TAILQ_ENTRY(nmx_open) link;
    int id;
    struct file *fp;
    struct cdev *cdev;
    void *priv;                         /* devfs_set_cdevpriv */
    d_priv_dtor_t *priv_dtor;
    struct mtx maps_mtx;
    TAILQ_HEAD(, nmx_umap) maps;        /* user mappings made through this open */
};

#define OPEN_BUCKETS 64
static TAILQ_HEAD(, nmx_open) opens[OPEN_BUCKETS];
static struct mtx opens_mtx;
MTX_SYSINIT(nmx_opens, &opens_mtx, "cdev opens", MTX_DEF);

static struct nmx_open *open_find(int id)
{
    struct nmx_open *o;
    mtx_lock(&opens_mtx);
    TAILQ_FOREACH(o, &opens[(unsigned)id % OPEN_BUCKETS], link)
        if (o->id == id)
            break;
    mtx_unlock(&opens_mtx);
    return o;
}

/* the open an entry point is running for, so devfs_get_cdevpriv finds its data */
static struct nmx_open *enter(struct nmx_open *o)
{
    struct thread *td = curthread;
    struct nmx_open *prev = td->td_nmx_open;
    td->td_nmx_open = o;
    return prev;
}

static void leave(struct nmx_open *prev)
{
    curthread->td_nmx_open = prev;
}

int devfs_set_cdevpriv(void *priv, d_priv_dtor_t *dtr)
{
    struct nmx_open *o = curthread->td_nmx_open;
    if (o == NULL)
        return ENOENT;
    if (o->priv != NULL)
        return EBUSY;
    o->priv = priv;
    o->priv_dtor = dtr;
    return 0;
}

int devfs_get_cdevpriv(void **datap)
{
    struct nmx_open *o = curthread->td_nmx_open;
    *datap = o ? o->priv : NULL;
    return *datap ? 0 : ENOENT;
}

void devfs_clear_cdevpriv(void)
{
    struct nmx_open *o = curthread->td_nmx_open;
    if (o == NULL || o->priv == NULL)
        return;
    if (o->priv_dtor)
        o->priv_dtor(o->priv);
    o->priv = NULL;
    o->priv_dtor = NULL;
}

static void umap_release_all(struct nmx_open *o);

int nmx_cdev_open(void *cookie, int open_id, int fflags)
{
    struct cdev *c = cookie;
    if (__atomic_load_n(&c->nmx_gone, __ATOMIC_ACQUIRE))
        return ENXIO;
    struct nmx_open *o = malloc(sizeof(*o), M_NMXCDEV, M_WAITOK | M_ZERO);
    struct file *fp = malloc(sizeof(*fp), M_NMXCDEV, M_WAITOK | M_ZERO);
    if (o == NULL || fp == NULL) {
        free(o, M_NMXCDEV);
        free(fp, M_NMXCDEV);
        return ENOMEM;
    }
    dev_ref(c);
    fp->f_count = 1;
    fp->f_flag = (u_int)fflags & (FREAD | FWRITE | O_NONBLOCK | O_APPEND);
    fp->f_type = DTYPE_DEV;
    o->id = open_id;
    o->fp = fp;
    o->cdev = c;
    mtx_init(&o->maps_mtx, "nmx umaps", NULL, MTX_DEF);
    TAILQ_INIT(&o->maps);

    struct nmx_open *prev = enter(o);
    struct nmx_bsd_cdevsw *sw = c->si_devsw;
    int err = 0;
    if (sw->d_fdopen != NULL)
        err = sw->d_fdopen(c, fflags, curthread, fp);
    else if (sw->d_open != NULL)
        err = sw->d_open(c, fflags, 0x2000 /* S_IFCHR */, curthread);
    if (err) {
        devfs_clear_cdevpriv();
        leave(prev);
        mtx_destroy(&o->maps_mtx);
        free(fp, M_NMXCDEV);
        free(o, M_NMXCDEV);
        dev_rel(c);
        return err;
    }
    leave(prev);

    mtx_lock(&opens_mtx);
    if (opens[(unsigned)open_id % OPEN_BUCKETS].tqh_last == NULL)
        TAILQ_INIT(&opens[(unsigned)open_id % OPEN_BUCKETS]);
    TAILQ_INSERT_HEAD(&opens[(unsigned)open_id % OPEN_BUCKETS], o, link);
    mtx_unlock(&opens_mtx);
    return 0;
}

/* FreeBSD fdrop: the last reference closes the file through its operations */
void fdrop(struct file *fp, struct thread *td)
{
    if (fp == NULL)
        return;
    if (__atomic_sub_fetch(&fp->f_count, 1, __ATOMIC_ACQ_REL) != 0)
        return;
    if (fp->f_ops != NULL && fp->f_ops->fo_close != NULL)
        fp->f_ops->fo_close(fp, td);
    free(fp, M_NMXCDEV);
}

int nmx_cdev_close(int open_id, int fflags)
{
    struct nmx_open *o;
    mtx_lock(&opens_mtx);
    TAILQ_FOREACH(o, &opens[(unsigned)open_id % OPEN_BUCKETS], link)
        if (o->id == open_id)
            break;
    if (o != NULL)
        TAILQ_REMOVE(&opens[(unsigned)open_id % OPEN_BUCKETS], o, link);
    mtx_unlock(&opens_mtx);
    if (o == NULL)
        return EBADF;

    struct nmx_open *prev = enter(o);
    umap_release_all(o);
    int err = 0;
    struct nmx_bsd_cdevsw *sw = o->cdev->si_devsw;
    if (o->fp->f_ops == NULL && sw->d_close != NULL)
        err = sw->d_close(o->cdev, fflags, 0x2000, curthread);
    fdrop(o->fp, curthread);
    devfs_clear_cdevpriv();
    leave(prev);

    dev_rel(o->cdev);
    mtx_destroy(&o->maps_mtx);
    free(o, M_NMXCDEV);
    return err;
}

static int nmx_mmap(struct nmx_open *o, struct nmx_drm_mmap *req);
int nmx_drm_node_ioctl(struct linux_file *filp, unsigned long cmd, void *data);   /* nmx_drm_os.c: -1 = not one of its ioctls */
static int nmx_munmap(struct nmx_open *o, struct nmx_drm_munmap *req);

int nmx_cdev_ioctl(int open_id, unsigned long cmd, void *data, int fflag)
{
    struct nmx_open *o = open_find(open_id);
    if (o == NULL)
        return EBADF;
    struct nmx_open *prev = enter(o);
    int err;
    if (cmd == NMX_DRM_IOCTL_MMAP) {
        err = nmx_mmap(o, data);
    } else if (cmd == NMX_DRM_IOCTL_MUNMAP) {
        err = nmx_munmap(o, data);
    } else if (o->fp->f_ops != NULL && (err = nmx_drm_node_ioctl(o->fp->f_data, cmd, data)) != -1) {
        /* answered by the port (DRM_IOCTL_GET_PCIINFO) */
    } else if (o->fp->f_ops != NULL) {
        err = o->fp->f_ops->fo_ioctl(o->fp, cmd, data, NULL, curthread);
    } else {
        struct nmx_bsd_cdevsw *sw = o->cdev->si_devsw;
        err = sw->d_ioctl ? sw->d_ioctl(o->cdev, cmd, data, fflag, curthread) : ENOTTY;
    }
    leave(prev);
    return err;
}

/* reads go straight to the caller's buffer: Linux read handlers copy_to_user() into it themselves */
int nmx_cdev_read(int open_id, uint64_t uaddr, size_t len, size_t *done, int nonblock)
{
    *done = 0;
    struct nmx_open *o = open_find(open_id);
    if (o == NULL)
        return EBADF;
    if (o->fp->f_ops == NULL || o->fp->f_ops->fo_read == NULL)
        return ENODEV;
    struct iovec iov = { .iov_base = (void *)(uintptr_t)uaddr, .iov_len = len };
    struct uio uio = {
        .uio_iov = &iov, .uio_iovcnt = 1, .uio_offset = o->fp->f_offset, .uio_resid = (ssize_t)len,
        .uio_segflg = UIO_USERSPACE, .uio_rw = UIO_READ, .uio_td = curthread,
    };
    if (nonblock)
        o->fp->f_flag |= O_NONBLOCK;
    else
        o->fp->f_flag &= ~(u_int)O_NONBLOCK;
    struct nmx_open *prev = enter(o);
    int err = o->fp->f_ops->fo_read(o->fp, &uio, NULL, 0, curthread);
    leave(prev);
    *done = len - (size_t)uio.uio_resid;
    return err;
}

int nmx_cdev_poll(int open_id, int events)
{
    struct nmx_open *o = open_find(open_id);
    if (o == NULL || o->fp->f_ops == NULL || o->fp->f_ops->fo_poll == NULL)
        return 0;
    struct nmx_open *prev = enter(o);
    int r = o->fp->f_ops->fo_poll(o->fp, events, NULL, curthread);
    leave(prev);
    return r;
}

/* ---- mmap --------------------------------------------------------------------------------------------------------------- */

struct nmx_umap {
    struct vm_area_struct vma;          /* the driver sees only this; the record is found from it */
    struct vm_object obj;               /* what the fault handler locks (VM_OBJECT_WLOCK(vma->vm_obj)) */
    TAILQ_ENTRY(nmx_umap) link;
    uint64_t *pages;                    /* physical address per page index, 0 until inserted */
    size_t npages;
    int cache;                          /* NMX_CACHE_* of the first insert */
    int have_cache;
    int mixed_cache;                    /* a later insert asked for another cache mode: refused */
    int (*pin)(void *arg, bool on);     /* drm_gem_ttm_mmap's hook (nmx_vma_set_pin) */
    void *pin_arg;
    int pinned;
    int vm_opened;                      /* the driver's mmap succeeded: vm_ops->close is owed */
    void *xmap;                         /* nmx_xnu_map_user handle */
    uint64_t uaddr;
    uint64_t usize;
};

static struct nmx_umap *umap_of(struct vm_area_struct *vma)
{
    return __containerof(vma, struct nmx_umap, vma);
}

void nmx_vma_set_pin(struct vm_area_struct *vma, int (*pin)(void *arg, bool on), void *arg)
{
    struct nmx_umap *u = umap_of(vma);
    u->pin = pin;
    u->pin_arg = arg;
}

/* the page-table insert of a fault handler (linux_page.c fork calls this) */
vm_fault_t nmx_umap_insert(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn, pgprot_t prot)
{
    struct nmx_umap *u = umap_of(vma);
    if (addr < vma->vm_start || addr >= vma->vm_end)
        return VM_FAULT_SIGBUS;
    size_t idx = (addr - vma->vm_start) >> PAGE_SHIFT;
    int cache = (int)pgprot2cachemode(prot);
    if (!u->have_cache) {
        u->cache = cache;
        u->have_cache = 1;
    } else if (cache != u->cache) {
        u->mixed_cache = 1;
    }
    if (u->pages[idx] == 0)
        vma->vm_pfn_count++;
    u->pages[idx] = (uint64_t)pfn << PAGE_SHIFT;
    return VM_FAULT_NOPAGE;
}

/* runs the driver's fault handler until every page of the mapping is known */
static int umap_populate(struct nmx_umap *u)
{
    struct vm_area_struct *vma = &u->vma;
    bzero(u->pages, sizeof(uint64_t) * u->npages);
    vma->vm_pfn_count = 0;
    u->have_cache = 0;
    u->mixed_cache = 0;
    for (size_t i = 0; i < u->npages; i++) {
        if (u->pages[i] != 0)
            continue;   /* a previous fault prefaulted it */
        struct vm_fault vmf = {
            .flags = (vma->vm_flags & VM_WRITE) ? FAULT_FLAG_WRITE : 0,
            .pgoff = vma->vm_pgoff + i,
            .address = vma->vm_start + (i << PAGE_SHIFT),
            .page = NULL,
            .vma = vma,
        };
        vm_fault_t r = vma->vm_ops->fault(&vmf);
        /* NOPAGE with nothing inserted means "try again" (the buffer was busy); FreeBSD retries the same way */
        for (int tries = 0; r == VM_FAULT_NOPAGE && u->pages[i] == 0 && tries < 1000; tries++) {
            pause("nmxflt", 1);
            r = vma->vm_ops->fault(&vmf);
        }
        if (u->pages[i] == 0)
            return r == VM_FAULT_OOM ? ENOMEM : EFAULT;
    }
    return u->mixed_cache ? EINVAL : 0;
}

static void umap_free(struct nmx_umap *u)
{
    struct vm_area_struct *vma = &u->vma;
    if (u->xmap != NULL)
        nmx_xnu_unmap_user(u->xmap);
    if (u->pinned && u->pin != NULL)
        u->pin(u->pin_arg, false);
    if (u->vm_opened && vma->vm_ops != NULL && vma->vm_ops->close != NULL)
        vma->vm_ops->close(vma);
    if (vma->vm_file != NULL)
        fput(vma->vm_file);
    if (vma->vm_mm != NULL)
        mmput(vma->vm_mm);
    mtx_destroy(&u->obj.lock);
    free(u->pages, M_NMXCDEV);
    free(u, M_NMXCDEV);
}

static int nmx_mmap(struct nmx_open *o, struct nmx_drm_mmap *req)
{
    uint64_t fixed = (req->flags & NMX_DRM_MMAP_FIXED) ? req->addr : 0;
    req->addr = 0;
    if ((req->flags & ~NMX_DRM_MMAP_FIXED) != 0 || (req->offset & PAGE_MASK) != 0 || req->size == 0 ||
        req->size > (1ULL << 40) || (fixed & PAGE_MASK) != 0 || ((req->flags & NMX_DRM_MMAP_FIXED) && fixed == 0))
        return EINVAL;
    struct linux_file *filp = o->fp->f_data;
    if (o->fp->f_ops == NULL || filp == NULL || filp->f_op == NULL || filp->f_op->mmap == NULL)
        return ENODEV;
    uint64_t size = round_page(req->size);
    struct nmx_umap *u = malloc(sizeof(*u), M_NMXCDEV, M_WAITOK | M_ZERO);
    if (u == NULL)
        return ENOMEM;
    u->npages = (size_t)(size >> PAGE_SHIFT);
    u->pages = malloc(sizeof(uint64_t) * u->npages, M_NMXCDEV, M_WAITOK | M_ZERO);
    mtx_init(&u->obj.lock, "nmx umap", NULL, MTX_DEF);
    if (u->pages == NULL) {
        mtx_destroy(&u->obj.lock);
        free(u, M_NMXCDEV);
        return ENOMEM;
    }

    linux_set_current(curthread);
    struct mm_struct *mm = current->mm;
    if (mm == NULL || !mmget_not_zero(mm)) {
        mtx_destroy(&u->obj.lock);
        free(u->pages, M_NMXCDEV);
        free(u, M_NMXCDEV);
        return EINVAL;
    }
    struct vm_area_struct *vma = &u->vma;
    vma->vm_start = 0;
    vma->vm_end = size;
    vma->vm_pgoff = req->offset >> PAGE_SHIFT;
    vma->vm_flags = (req->prot & (VM_READ | VM_WRITE)) | VM_SHARED;
    vma->vm_page_prot = vma->vm_flags & (VM_READ | VM_WRITE);
    vma->vm_file = get_file(filp);
    vma->vm_mm = mm;
    vma->vm_obj = &u->obj;

    struct file *oldfp = curthread->td_fpop;
    curthread->td_fpop = o->fp;
    down_write(&mm->mmap_sem);
    int err = -filp->f_op->mmap(filp, vma);
    if (err == 0) {
        u->vm_opened = vma->vm_ops != NULL;
        if (vma->vm_ops != NULL && vma->vm_ops->fault != NULL) {
            /* pass 1 lets the driver place the buffer; pinned, pass 2 reads where it now stays */
            err = umap_populate(u);
            if (err == 0 && u->pin != NULL) {
                err = -u->pin(u->pin_arg, true);
                if (err == 0) {
                    u->pinned = 1;
                    err = umap_populate(u);
                }
            }
        } else if (vma->vm_pfn_count != (int)u->npages) {
            err = EINVAL;   /* no fault handler and the mmap handler did not map every page */
        }
    }
    up_write(&mm->mmap_sem);
    curthread->td_fpop = oldfp;

    if (err == 0) {
        int writable = (vma->vm_flags & VM_WRITE) != 0;
        err = nmx_xnu_map_user(u->pages, u->npages, u->cache, writable, fixed, &u->uaddr, &u->xmap);
    }
    if (err) {
        umap_free(u);
        return err;
    }
    u->usize = size;
    mtx_lock(&o->maps_mtx);
    TAILQ_INSERT_HEAD(&o->maps, u, link);
    mtx_unlock(&o->maps_mtx);
    req->addr = u->uaddr;
    return 0;
}

static int nmx_munmap(struct nmx_open *o, struct nmx_drm_munmap *req)
{
    struct nmx_umap *u;
    mtx_lock(&o->maps_mtx);
    TAILQ_FOREACH(u, &o->maps, link)
        if (u->uaddr == req->addr && u->usize == round_page(req->size))
            break;
    if (u != NULL)
        TAILQ_REMOVE(&o->maps, u, link);
    mtx_unlock(&o->maps_mtx);
    if (u == NULL)
        return EINVAL;
    linux_set_current(curthread);
    umap_free(u);
    return 0;
}

static void umap_release_all(struct nmx_open *o)
{
    struct nmx_umap *u;
    linux_set_current(curthread);
    for (;;) {
        mtx_lock(&o->maps_mtx);
        u = TAILQ_FIRST(&o->maps);
        if (u != NULL)
            TAILQ_REMOVE(&o->maps, u, link);
        mtx_unlock(&o->maps_mtx);
        if (u == NULL)
            break;
        umap_free(u);
    }
}
