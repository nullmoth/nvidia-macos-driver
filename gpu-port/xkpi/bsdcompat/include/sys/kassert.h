/* FreeBSD <sys/kassert.h> on xnu. KASSERT/MPASS check only in INVARIANTS builds, as on FreeBSD; CTASSERT is static. */
#pragma once
#include <kern/debug.h>
#undef KASSERT
#ifdef INVARIANTS
#define NMX_KASSERT(exp, msg) do { if (__builtin_expect(!(exp), 0)) { printf("KASSERT %s:%d: ", __FILE__, __LINE__); printf msg; printf("\n"); panic("KASSERT: %s", #exp); } } while (0)
#else
#define NMX_KASSERT(exp, msg) do { } while (0)
#endif
#define KASSERT(exp, msg) NMX_KASSERT(exp, msg)
#define MPASS(ex) KASSERT((ex), ("Assertion %s failed at %s:%d", #ex, __FILE__, __LINE__))
#define MPASS2(ex, what) KASSERT((ex), ("Assertion %s failed at %s:%d", what, __FILE__, __LINE__))
#define CTASSERT(x) _Static_assert(x, "compile-time assertion failed")
#define VNASSERT(exp, vp, msg) KASSERT(exp, msg)
