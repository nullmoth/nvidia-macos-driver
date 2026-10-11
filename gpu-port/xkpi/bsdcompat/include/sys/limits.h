/* FreeBSD <sys/limits.h> on xnu */
#pragma once
#include <machine/limits.h>
#ifndef SSIZE_MAX
#define SSIZE_MAX LONG_MAX
#endif
#ifndef OFF_MAX
#define OFF_MAX INT64_MAX
#endif
