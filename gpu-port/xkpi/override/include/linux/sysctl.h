/*
 * Linux sysctl tables. macOS has no /proc/sys, so a table registers as nothing and every value keeps its built-in
 * default; register_sysctl() still succeeds so a driver's init does not fail for want of a tuning knob. The only user
 * today is xe's observation_paranoid (default 1: only root may open OA streams), which is the safe default.
 */
#ifndef _NMX_LINUX_SYSCTL_H_
#define _NMX_LINUX_SYSCTL_H_

#include <linux/types.h>

struct ctl_table;
typedef int proc_handler(const struct ctl_table *, int, void *, size_t *, loff_t *);

struct ctl_table {
	const char *procname;
	void *data;
	int maxlen;
	umode_t mode;
	proc_handler *proc_handler;
	void *extra1;
	void *extra2;
};

struct ctl_table_header {
	const struct ctl_table *table;
};

static const int nmx_sysctl_vals[] = { 0, 1 };
#define SYSCTL_ZERO ((void *)&nmx_sysctl_vals[0])
#define SYSCTL_ONE ((void *)&nmx_sysctl_vals[1])

static inline int proc_dointvec_minmax(const struct ctl_table *t, int w, void *b, size_t *l, loff_t *p)
{
	return -EOPNOTSUPP;
}

static inline int proc_dointvec(const struct ctl_table *t, int w, void *b, size_t *l, loff_t *p)
{
	return -EOPNOTSUPP;
}

/* one shared header: nothing is registered, and unregistering it is a no-op */
static struct ctl_table_header nmx_sysctl_none;

#define register_sysctl(path, table) (&nmx_sysctl_none)
#define register_sysctl_sz(path, table, n) (&nmx_sysctl_none)
static inline void unregister_sysctl_table(struct ctl_table_header *h) { }

#endif
