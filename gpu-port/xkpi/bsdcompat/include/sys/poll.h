/* FreeBSD <sys/poll.h>: poll event bits (identical to xnu's and Linux's values). */
#pragma once
#define POLLIN 0x0001
#define POLLPRI 0x0002
#define POLLOUT 0x0004
#define POLLRDNORM 0x0040
#define POLLWRNORM POLLOUT
#define POLLRDBAND 0x0080
#define POLLWRBAND 0x0100
#define POLLERR 0x0008
#define POLLHUP 0x0010
#define POLLNVAL 0x0020
#define POLLSTANDARD (POLLIN | POLLPRI | POLLOUT | POLLRDNORM | POLLRDBAND | POLLWRBAND | POLLERR | POLLHUP | POLLNVAL)
struct pollfd { int fd; short events; short revents; };
