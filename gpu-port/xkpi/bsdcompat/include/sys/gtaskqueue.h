/*
 * FreeBSD grouptaskqueues, as LinuxKPI's tasklets use them: on macOS a grouptask is a task on the system
 * taskqueue, run by its thread; the per-CPU placement FreeBSD offers is a scheduling preference, not a guarantee
 * tasklets depend on.
 */
#pragma once
#include <sys/taskqueue.h>
typedef void gtask_fn_t(void *context);
struct gtask {
    struct task gt_task;
    gtask_fn_t *gt_fn;
    void *gt_arg;
};
struct grouptask {
    struct gtask gtask;
    void *gt_taskqueue;
    int gt_cpu;
    char gt_name[32];
};
struct taskqgroup;
extern struct taskqgroup *qgroup_softirq;
void nmx_grouptask_init(struct grouptask *gt, int prio, gtask_fn_t *fn, void *arg);
void nmx_grouptask_enqueue(struct grouptask *gt);
#define GROUPTASK_INIT(gt, prio, fn, arg) nmx_grouptask_init((gt), (prio), (gtask_fn_t *)(fn), (arg))
#define GROUPTASK_ENQUEUE(gt) nmx_grouptask_enqueue(gt)
int taskqgroup_attach_cpu(struct taskqgroup *qgroup, struct grouptask *grptask, void *uniq, int cpu, void *dev, void *irq, const char *name);
void taskqgroup_attach(struct taskqgroup *qgroup, struct grouptask *grptask, void *uniq, void *dev, void *irq, const char *name);
void taskqgroup_detach(struct taskqgroup *qgroup, struct grouptask *gtask);
void taskqgroup_drain_all(struct taskqgroup *qgroup);
