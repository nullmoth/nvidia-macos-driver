/* FreeBSD <sys/smp.h> on xnu */
#pragma once
#include <sys/cpuset.h>
extern int mp_ncpus;
extern int mp_ncores;
extern int mp_maxid;
extern int smp_started;
extern cpuset_t all_cpus;
#define CPU_FOREACH(i) for ((i) = 0; (i) <= mp_maxid; (i)++)
#define CPU_ABSENT(i) ((i) > mp_maxid)
void smp_rendezvous(void (*setup)(void *), void (*action)(void *), void (*teardown)(void *), void *arg);
void smp_rendezvous_cpus(cpuset_t map, void (*setup)(void *), void (*action)(void *), void (*teardown)(void *), void *arg);
#define smp_no_rendezvous_barrier NULL
