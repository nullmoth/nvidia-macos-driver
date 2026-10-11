/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The GPU scheduler interface of Linux 7.x that AMD's XDNA (7.3) is written against, on the 6.14 scheduler in this
 * port: init through an argument struct, a client id on job init, and the newer timeout verdicts. Included by
 * aie2_pci.h after drm/gpu_scheduler.h (fork_npu.py).
 */
#ifndef _NMX_XDNA_SCHED_H_
#define _NMX_XDNA_SCHED_H_
#include <drm/gpu_scheduler.h>

struct drm_sched_init_args {
	const struct drm_sched_backend_ops *ops;
	struct workqueue_struct *submit_wq;
	struct workqueue_struct *timeout_wq;
	u32 num_rqs;
	u32 credit_limit;
	unsigned int hang_limit;
	long timeout;
	atomic_t *score;
	const char *name;
	struct device *dev;
};

static inline int nmx_drm_sched_init(struct drm_gpu_scheduler *sched, const struct drm_sched_init_args *a)
{
	return drm_sched_init(sched, a->ops, a->submit_wq, a->num_rqs, a->credit_limit, a->hang_limit, a->timeout,
			      a->timeout_wq, a->score, a->name, a->dev);
}

/* 7.x passes the DRM client id for fdinfo accounting; 6.14 has no such field */
#define nmx_drm_sched_job_init(job, entity, credits, owner, client_id) \
	drm_sched_job_init((job), (entity), (credits), (owner))

/*
 * 7.x timeout verdicts. RESET (the driver reset the context) and NO_HANG (the job was not stuck; keep it) both mean
 * "the driver handled the timeout" to the 6.14 scheduler, which is its NOMINAL.
 */
#define DRM_GPU_SCHED_STAT_RESET DRM_GPU_SCHED_STAT_NOMINAL
#define DRM_GPU_SCHED_STAT_NO_HANG DRM_GPU_SCHED_STAT_NOMINAL
#endif
