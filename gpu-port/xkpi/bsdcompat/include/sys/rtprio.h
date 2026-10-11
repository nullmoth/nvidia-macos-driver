/* FreeBSD <sys/rtprio.h> */
#pragma once
#define RTP_PRIO_REALTIME 2
#define RTP_PRIO_NORMAL 3
#define RTP_PRIO_IDLE 4
#define RTP_PRIO_FIFO 2
#define RTP_PRIO_MIN 0
#define RTP_PRIO_MAX 31
struct rtprio { unsigned short type; unsigned short prio; };
struct thread;
/* raises an xnu thread to a fixed (time-constraint free) high priority for FIFO, as LinuxKPI's sched_set_fifo asks */
int rtp_to_pri(struct rtprio *rtp, struct thread *td);
