/* FreeBSD KTR tracing: not built on macOS. */
#pragma once
/* drm-kmod's i915 trace points call the variadic form */
#define CTR(m, ...) do { } while (0)
#define CTR0(m, f) do { } while (0)
#define CTR1(m, f, a) do { } while (0)
#define CTR2(m, f, a, b) do { } while (0)
#define CTR3(m, f, a, b, c) do { } while (0)
#define CTR4(m, f, a, b, c, d) do { } while (0)
#define CTR5(m, f, a, b, c, d, e) do { } while (0)
#define CTR6(m, f, a, b, c, d, e, g) do { } while (0)
#define KTR_GEN 0
#define KTR_DEV 0
#define KTR_DRM 0
#define KTR_DRM_REG 0
#define KTR_LOCK 0
