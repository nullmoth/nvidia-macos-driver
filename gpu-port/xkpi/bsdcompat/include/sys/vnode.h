/*
 * FreeBSD vnodes as LinuxKPI uses them: a Linux inode is a FreeBSD vnode. These are the port's own objects, one per
 * DRM device node (and per anonymous shmem file), never passed to xnu: xnu's vnode_t is opaque to kexts and its
 * vnode KPIs are only called from C++ glue that does not see this header.
 */
#ifndef NMXKPI_SYS_VNODE_H
#define NMXKPI_SYS_VNODE_H
#ifndef _SYS_VNODE_H_
#define _SYS_VNODE_H_   /* xnu's guard: keep its header out of LinuxKPI code */
#endif
#include <sys/types.h>
#include <sys/_mutex.h>
#include <sys/selinfo.h>
struct cdev;
struct vm_object;
struct vnode {
    volatile u_int v_usecount;
    struct mtx v_interlock;
    struct cdev *v_rdev;            /* Linux i_cdev */
    void *v_data;                   /* Linux i_private */
    struct vm_object *v_object;     /* Linux i_mapping */
    u_int v_type;
};
enum vtype { VNON, VREG, VDIR, VBLK, VCHR, VLNK, VSOCK, VFIFO, VBAD, VMARKER };
int vget(struct vnode *vp, int flags);
void vref(struct vnode *vp);
void vrele(struct vnode *vp);
void vput(struct vnode *vp);
#define VI_LOCK(vp) mtx_lock(&(vp)->v_interlock)
#define VI_UNLOCK(vp) mtx_unlock(&(vp)->v_interlock)
#define LK_SHARED 0x200000
#define LK_EXCLUSIVE 0x080000
#define LK_RETRY 0x000400
#endif
