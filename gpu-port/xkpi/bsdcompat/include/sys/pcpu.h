/* FreeBSD <sys/pcpu.h> on xnu: the current CPU and thread. */
#pragma once
#include <sys/types.h>
#include <kern/thread.h>
int nmx_curcpu(void);
#define curcpu nmx_curcpu()
#define PCPU_GET(member) nmx_pcpu_##member()
#define nmx_pcpu_cpuid() nmx_curcpu()
struct thread;
struct thread *nmx_curthread(void);
#define curthread nmx_curthread()
#define DPCPU_DEFINE(t, n) t nmx_dpcpu_##n[256]
#define DPCPU_DEFINE_STATIC(t, n) static t nmx_dpcpu_##n[256]
#define DPCPU_DECLARE(t, n) extern t nmx_dpcpu_##n[256]
#define DPCPU_PTR(n) (&nmx_dpcpu_##n[curcpu])
#define DPCPU_GET(n) (nmx_dpcpu_##n[curcpu])
#define DPCPU_SET(n, v) (nmx_dpcpu_##n[curcpu] = (v))
#define DPCPU_ID_PTR(i, n) (&nmx_dpcpu_##n[(i)])
#define DPCPU_ID_GET(i, n) (nmx_dpcpu_##n[(i)])
#define DPCPU_ID_SET(i, n, v) (nmx_dpcpu_##n[(i)] = (v))
