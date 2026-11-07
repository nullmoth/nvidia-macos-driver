/* lnxlibc.h - glibc ABI translations used by elfhost. */
#pragma once
#include <stddef.h>
#include <time.h>

void *lnx_bind(const char *name);                       /* NULL if the name needs no translation */
void *lnx_obj(const void *key, size_t size, void (*init)(void *));
long lnx_clock_gettime(long linux_clock_id, struct timespec *ts);
