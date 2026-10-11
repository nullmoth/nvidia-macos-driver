/* FreeBSD struct file as LinuxKPI's linux_file refers to it: the port's record of one open of a DRM node. */
#ifndef NMXKPI_SYS_FILE_H
#define NMXKPI_SYS_FILE_H
#ifndef _SYS_FILE_H_
#define _SYS_FILE_H_    /* xnu's guard */
#endif
#include <sys/types.h>
#include <sys/fcntl.h>
#include <sys/selinfo.h>
#include <sys/capsicum.h>
#include <sys/refcount.h>
struct vnode;
struct fileops;
struct file {
    volatile u_int f_count;
    u_int f_flag;
    void *f_data;
    off_t f_offset;
    struct vnode *f_vnode;
    struct fileops *f_ops;
    short f_type;
    void *nmx_xnu;                  /* the xnu side of this open (dev_t + per-open state), owned by the cdev glue */
};
#define DTYPE_DEV 3
#define DTYPE_VNODE 1
#define FREAD 0x0001
#define FWRITE 0x0002

/*
 * File operations, FreeBSD's layout minus the slots no port code fills (seek, aio, seals, fallocate, fspacectl,
 * fork, spares). The device-node glue (nmx_linux_cdev.c) calls an open's read, write, ioctl, poll and close
 * through these; the other entries are kept so LinuxKPI's table compiles unchanged and are never called.
 */
struct stat;
struct knote;
struct ucred;
struct thread;
struct kinfo_file;
struct filedesc;
typedef int fo_rdwr_t(struct file *fp, struct uio *uio, struct ucred *active_cred, int flags, struct thread *td);
typedef int fo_truncate_t(struct file *fp, off_t length, struct ucred *active_cred, struct thread *td);
typedef int fo_ioctl_t(struct file *fp, u_long com, void *data, struct ucred *active_cred, struct thread *td);
typedef int fo_poll_t(struct file *fp, int events, struct ucred *active_cred, struct thread *td);
typedef int fo_kqfilter_t(struct file *fp, struct knote *kn);
typedef int fo_stat_t(struct file *fp, struct stat *sb, struct ucred *active_cred);
typedef int fo_close_t(struct file *fp, struct thread *td);
typedef int fo_chmod_t(struct file *fp, mode_t mode, struct ucred *active_cred, struct thread *td);
typedef int fo_chown_t(struct file *fp, uid_t uid, gid_t gid, struct ucred *active_cred, struct thread *td);
typedef int fo_sendfile_t(struct file *fp, int sockfd, struct uio *hdr_uio, struct uio *trl_uio, off_t offset,
                          size_t nbytes, off_t *sent, int flags, struct thread *td);
typedef int fo_fill_kinfo_t(struct file *fp, struct kinfo_file *kif, struct filedesc *fdp);
typedef int fo_mmap_t(struct file *fp, vm_map_t map, vm_offset_t *addr, vm_size_t size, vm_prot_t prot,
                      vm_prot_t cap_maxprot, int flags, vm_ooffset_t foff, struct thread *td);
typedef int fo_cmp_t(struct file *fp, struct file *fp1, struct thread *td);
typedef int fo_flags_t;
struct fileops {
    fo_rdwr_t *fo_read;
    fo_rdwr_t *fo_write;
    fo_truncate_t *fo_truncate;
    fo_ioctl_t *fo_ioctl;
    fo_poll_t *fo_poll;
    fo_kqfilter_t *fo_kqfilter;
    fo_stat_t *fo_stat;
    fo_close_t *fo_close;
    fo_chmod_t *fo_chmod;
    fo_chown_t *fo_chown;
    fo_sendfile_t *fo_sendfile;
    fo_fill_kinfo_t *fo_fill_kinfo;
    fo_mmap_t *fo_mmap;
    fo_cmp_t *fo_cmp;
    fo_flags_t fo_flags;
};
#define DFLAG_PASSABLE 0x01
#define DFLAG_SEEKABLE 0x02
/* the "invalid operation" entries: each answers EOPNOTSUPP (FreeBSD kern_descrip.c behaviour) */
fo_rdwr_t invfo_rdwr;
fo_truncate_t invfo_truncate;
fo_ioctl_t invfo_ioctl;
fo_poll_t invfo_poll;
fo_kqfilter_t invfo_kqfilter;
fo_chmod_t invfo_chmod;
fo_chown_t invfo_chown;
fo_sendfile_t invfo_sendfile;

/* F_SETOWN / SIGIO: asynchronous I/O signals are not delivered by the port (see nmx_misc.c) */
struct sigio;
int fsetown(pid_t pgid, struct sigio **sigiop);
pid_t fgetown(struct sigio **sigiop);
void funsetown(struct sigio **sigiop);
/*
 * File descriptors: xnu gives kexts no KPI to create one, so exporting a GPU object as an fd (dma-buf, sync_file)
 * is reported as unsupported (EOPNOTSUPP) and looking one up finds nothing (EBADF). The render path does not need them.
 */
int fget(struct thread *td, int fd, cap_rights_t *rightsp, struct file **fpp);
int fget_unlocked(struct thread *td, int fd, cap_rights_t *needrightsp, struct file **fpp);
void fdrop(struct file *fp, struct thread *td);
#define _fdrop(fp, td) (fdrop((fp), (td)), 0)
int falloc(struct thread *td, struct file **resultfp, int *resultfd, int flags);
void finit(struct file *fp, u_int flag, short type, void *data, struct fileops *ops);
void fdclose(struct thread *td, struct file *fp, int idx);
#define fhold(fp) __atomic_fetch_add(&(fp)->f_count, 1, __ATOMIC_SEQ_CST)
#endif
