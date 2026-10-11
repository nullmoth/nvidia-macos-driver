/*
 * FreeBSD character devices (make_dev(9), struct cdev, struct cdevsw) on xnu.
 *
 * xnu's own <sys/conf.h> (its struct cdevsw, cdevsw_add) is included first, by the port prelude. FreeBSD's
 * struct cdevsw has another layout, so in LinuxKPI code the name refers to the port's FreeBSD-shaped structure
 * (struct nmx_bsd_cdevsw); only the devfs glue (xnu/nmx_devfs.cpp) ever uses xnu's.
 *
 * Each FreeBSD cdev becomes an xnu devfs clone node: every open gets its own minor number, which is how the port
 * keeps per-open state (a Linux struct file) although an xnu cdevsw entry point sees only a dev_t.
 */
#ifndef NMXKPI_SYS_CONF_H
#define NMXKPI_SYS_CONF_H
#include_next <sys/conf.h>
#include <sys/types.h>
#include <sys/queue.h>

struct thread;
struct file;
struct uio;
struct knote;
struct vm_object;
struct cdev;

#define cdevsw nmx_bsd_cdevsw
/* xnu names its entry-point types with macros of the same names; LinuxKPI means FreeBSD's */
#undef d_open_t
#undef d_close_t
#undef d_read_t
#undef d_write_t
#undef d_ioctl_t
#undef d_stop_t
#undef d_reset_t
#undef d_select_t
#undef d_mmap_t
#undef d_strategy_t
#undef d_poll_t

typedef int d_open_t(struct cdev *dev, int oflags, int devtype, struct thread *td);
typedef int d_fdopen_t(struct cdev *dev, int oflags, struct thread *td, struct file *fp);
typedef int d_close_t(struct cdev *dev, int fflag, int devtype, struct thread *td);
typedef int d_ioctl_t(struct cdev *dev, u_long cmd, caddr_t data, int fflag, struct thread *td);
typedef int d_read_t(struct cdev *dev, struct uio *uio, int ioflag);
typedef int d_write_t(struct cdev *dev, struct uio *uio, int ioflag);
typedef int d_poll_t(struct cdev *dev, int events, struct thread *td);
typedef int d_kqfilter_t(struct cdev *dev, struct knote *kn);
typedef int d_mmap_t(struct cdev *dev, vm_ooffset_t offset, vm_paddr_t *paddr, int nprot, vm_memattr_t *memattr);
typedef int d_mmap_single_t(struct cdev *cdev, vm_ooffset_t *offset, vm_size_t size, struct vm_object **object, int nprot);
typedef void d_purge_t(struct cdev *dev);

struct nmx_bsd_cdevsw {
    int d_version;
    u_int d_flags;
    const char *d_name;
    d_open_t *d_open;
    d_fdopen_t *d_fdopen;
    d_close_t *d_close;
    d_read_t *d_read;
    d_write_t *d_write;
    d_ioctl_t *d_ioctl;
    d_poll_t *d_poll;
    d_mmap_t *d_mmap;
    d_kqfilter_t *d_kqfilter;
    d_purge_t *d_purge;
    d_mmap_single_t *d_mmap_single;
    LIST_HEAD(, cdev) d_devs;       /* the cdevs made on this cdevsw, under dev_lock */
};

#define D_VERSION_00 0x20011966
#define D_VERSION_04 0x5c48c353
#define D_VERSION D_VERSION_04
#define D_TRACKCLOSE 0x00080000
#define D_MMAP_ANON 0x00100000
#define D_NEEDGIANT 0x00400000
#define D_NEEDMINOR 0x00800000

struct cdev {
    void *si_drv1;
    void *si_drv2;
    struct nmx_bsd_cdevsw *si_devsw;
    char si_name[64];
    int si_unit;
    uid_t si_uid;
    gid_t si_gid;
    mode_t si_mode;
    void *nmx_node;                 /* the xnu devfs node (opaque; owned by the devfs glue) */
    int nmx_base_minor;             /* minor of the clone node; opens get their own minors */
    u_int nmx_refs;                 /* creation reference + one per open (dev_ref/dev_rel) */
    int nmx_gone;                   /* destroy_dev ran: no new opens */
    TAILQ_ENTRY(cdev) nmx_link;
    LIST_ENTRY(cdev) si_list;       /* in si_devsw->d_devs */
};

struct make_dev_args {
    size_t mda_size;
    int mda_flags;
    struct nmx_bsd_cdevsw *mda_devsw;
    struct ucred *mda_cr;
    uid_t mda_uid;
    gid_t mda_gid;
    int mda_mode;
    int mda_unit;
    void *mda_si_drv1;
    void *mda_si_drv2;
};
#define MAKEDEV_REF 0x0001
#define MAKEDEV_NOWAIT 0x0004
#define MAKEDEV_WAITOK 0x0008
#define MAKEDEV_ETERNAL 0x0010
#define MAKEDEV_CHECKNAME 0x0020
#define MAKEDEV_ETERNAL_KLD MAKEDEV_ETERNAL
#ifndef UID_ROOT
#define UID_ROOT 0
#endif
#ifndef GID_WHEEL
#define GID_WHEEL 0
#endif
#ifndef GID_OPERATOR
#define GID_OPERATOR 5
#endif

void make_dev_args_init_impl(struct make_dev_args *args, size_t sz);
#define make_dev_args_init(a) make_dev_args_init_impl((a), sizeof(struct make_dev_args))
int make_dev_s(struct make_dev_args *args, struct cdev **cdev, const char *fmt, ...) __printflike(3, 4);
struct cdev *make_dev(struct nmx_bsd_cdevsw *devsw, int unit, uid_t uid, gid_t gid, int perms, const char *fmt, ...) __printflike(6, 7);
void destroy_dev(struct cdev *dev);
void destroy_dev_sched(struct cdev *dev);
void destroy_dev_drain(struct nmx_bsd_cdevsw *csw);
void dev_ref(struct cdev *dev);
void dev_rel(struct cdev *dev);
int dev2unit(struct cdev *dev);
/* the lock over every cdevsw's d_devs list (FreeBSD's devmtx) */
void dev_lock(void);
void dev_unlock(void);
/* the most one read or write call moves (FreeBSD devfs_iosize_max_clamp=1: INT_MAX) */
#define DEVFS_IOSIZE_MAX 0x7fffffff
#define dev_refthread(dev, refp) ((dev)->si_devsw)
#define dev_relthread(dev, ref) do { } while (0)

/* per-open private data: set while a cdev entry point runs for a particular open (see nmx_linux_cdev.c) */
typedef void d_priv_dtor_t(void *data);
int devfs_set_cdevpriv(void *priv, d_priv_dtor_t *dtr);
int devfs_get_cdevpriv(void **datap);
void devfs_clear_cdevpriv(void);
#endif
