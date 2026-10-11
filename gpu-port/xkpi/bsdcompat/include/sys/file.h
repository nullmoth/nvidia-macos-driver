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
 * File descriptors: xnu gives kexts no KPI to create one, so exporting a GPU object as an fd (dma-buf, sync_file)
 * is reported as unsupported (EOPNOTSUPP) and looking one up finds nothing (EBADF). The render path does not need them.
 */
int fget(struct thread *td, int fd, cap_rights_t *rightsp, struct file **fpp);
int fget_unlocked(struct thread *td, int fd, cap_rights_t *needrightsp, struct file **fpp);
void fdrop(struct file *fp, struct thread *td);
int falloc(struct thread *td, struct file **resultfp, int *resultfd, int flags);
void finit(struct file *fp, u_int flag, short type, void *data, struct fileops *ops);
void fdclose(struct thread *td, struct file *fp, int idx);
#define fhold(fp) __atomic_fetch_add(&(fp)->f_count, 1, __ATOMIC_SEQ_CST)
#endif
