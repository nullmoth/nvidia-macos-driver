/* FreeBSD <sys/interrupt.h>: interrupt threads are xnu's; LinuxKPI only needs the filter return codes. */
#pragma once
#define FILTER_STRAY 0x01
#define FILTER_HANDLED 0x02
#define FILTER_SCHEDULE_THREAD 0x04
#define INTR_TYPE_TTY 1
#define INTR_TYPE_BIO 2
#define INTR_TYPE_NET 4
#define INTR_TYPE_CAM 8
#define INTR_TYPE_MISC 16
#define INTR_TYPE_CLK 32
#define INTR_TYPE_AV 64
#define INTR_EXCL 256
#define INTR_MPSAFE 512
#define INTR_ENTROPY 1024
#define INTR_MD1 4096
typedef int driver_filter_t(void *);
/* interrupt CPU affinity: xnu places device interrupts itself; the request is accepted and reported as done */
int intr_setaffinity(int irq, int mode, const void *mask);
/* waits until no handler of interrupt number 'irq' is running (LinuxKPI synchronize_irq) */
void _intr_drain(int irq);
typedef void driver_intr_t(void *);
