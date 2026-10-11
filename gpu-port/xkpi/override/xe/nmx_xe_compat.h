/*
 * Linux kernel interfaces xe 6.14 uses that FreeBSD LinuxKPI (or this port's layer) does not provide, force-included
 * into every xe source by cc-xe.sh. Each is either the Linux behaviour or, where macOS has no counterpart, the
 * documented answer for "this facility is absent".
 */
#ifndef _NMX_XE_COMPAT_H_
#define _NMX_XE_COMPAT_H_

#include <linux/types.h>
#include <linux/errno.h>
#include <linux/bitops.h>
#include <linux/kernel.h>
#include <linux/ktime.h>
#include <linux/rwsem.h>
#include <linux/mutex.h>
#include <linux/capability.h>
#include <linux/io.h>
#include <linux/iosys-map.h>

/* declared at file scope so the prototypes below name the same struct as the rest of the driver */
struct drm_device;
struct pci_dev;

/* errno values Linux has and FreeBSD does not; numbers are Linux's, never returned to userland as-is */
#ifndef EREMCHG
#define EREMCHG 78
#endif
#ifndef ENOPKG
#define ENOPKG 65
#endif

/* fault injection: never configured, so nothing ever fails on purpose */
struct fault_attr { int unused; };
#define DECLARE_FAULT_ATTR(name) struct fault_attr name = { 0 }
static inline bool should_fail(struct fault_attr *attr, ssize_t size) { return false; }
#ifndef ALLOW_ERROR_INJECTION
#define ALLOW_ERROR_INJECTION(fname, type) struct nmx_allow_error_injection_unused
#endif

/* performance monitoring needs CAP_PERFMON on Linux; here only the superuser opens OA streams */
static inline bool perfmon_capable(void) { return capable(CAP_SYS_ADMIN); }

#ifndef lockdep_assert_held_write
#define lockdep_assert_held_write(l) do { (void)(l); } while (0)
#endif
#ifndef lockdep_assert_held_read
#define lockdep_assert_held_read(l) do { (void)(l); } while (0)
#endif

static inline int down_read_interruptible(struct rw_semaphore *sem)
{
	down_read(sem);
	return 0;
}

/* set the 8 bits at 'start' (a multiple of 8) to 'value' */
static inline void bitmap_set_value8(unsigned long *map, unsigned long value, unsigned long start)
{
	size_t index = start / BITS_PER_LONG;
	unsigned long offset = start % BITS_PER_LONG;

	map[index] &= ~(0xFFUL << offset);
	map[index] |= (value & 0xFF) << offset;
}

/* iterate the runs [b, e) of set (or clear) bits */
#define for_each_set_bitrange(b, e, addr, size)				\
	for ((b) = 0;							\
	     (b) = find_next_bit((addr), (size), (b)),			\
	     (e) = find_next_zero_bit((addr), (size), (b) + 1),	\
	     (b) < (size);						\
	     (b) = (e) + 1)
#define for_each_clear_bitrange(b, e, addr, size)			\
	for ((b) = 0;							\
	     (b) = find_next_zero_bit((addr), (size), (b)),		\
	     (e) = find_next_bit((addr), (size), (b) + 1),		\
	     (b) < (size);						\
	     (b) = (e) + 1)

#ifndef FIELD_MAX
#define FIELD_MAX(_mask) ((__typeof__(_mask))((_mask) >> __builtin_ctzll(_mask)))
#endif

/*
 * clocks: macOS keeps no TAI offset, so TAI reads as real time (the 37 s leap offset is not known in-kernel).
 * The ids only need to differ from the FreeBSD ones the layer already defines (CLOCK_MONOTONIC is 4 there, which
 * Linux's CLOCK_MONOTONIC_RAW number collided with in xe_oa's switch).
 */
#ifndef CLOCK_MONOTONIC_RAW
#define CLOCK_MONOTONIC_RAW 104
#endif
#ifndef CLOCK_BOOTTIME
#define CLOCK_BOOTTIME 107
#endif
#ifndef CLOCK_TAI
#define CLOCK_TAI 111
#endif
static inline u64 ktime_get_real_ns(void) { return ktime_to_ns(ktime_get_real()); }
static inline u64 ktime_get_clocktai_ns(void) { return ktime_to_ns(ktime_get_real()); }

#ifndef IORESOURCE_UNSET
#define IORESOURCE_UNSET 0x20000000
#endif
#ifndef IORESOURCE_MEM_64
#define IORESOURCE_MEM_64 0x00100000
#endif
#ifndef IORES_DESC_NONE
#define IORES_DESC_NONE 0
#endif
#ifndef WQ_MEM_RECLAIM
#define WQ_MEM_RECLAIM 0
#endif
#ifndef VM_MAYSHARE
#define VM_MAYSHARE 0x00000080
#endif
#ifndef VM_MAYEXEC
#define VM_MAYEXEC 0x00000040
#endif
#ifndef UUID_STRING_LEN
#define UUID_STRING_LEN 36
#endif
#ifndef ACPI_STATE_S3
#define ACPI_STATE_S3 3
#endif

/* copy out of a buffer that may be in I/O memory (VRAM through the BAR) or plain memory */
static inline void iosys_map_memcpy_from(void *dst, const struct iosys_map *src, size_t src_offset, size_t len)
{
	if (src->is_iomem)
		memcpy_fromio(dst, (const char __iomem *)src->vaddr_iomem + src_offset, len);
	else
		memcpy(dst, (const char *)src->vaddr + src_offset, len);
}

#ifndef MICRO
#define MICRO 1000000UL
#endif

/* SR-IOV: macOS assigns no virtual functions, so a GPU reports none and is never a VF's parent */
static inline int pci_sriov_get_totalvfs(struct pci_dev *pdev) { return 0; }

/* the Resizable BAR size code for a byte count (1 MB = 0) */
static inline int pci_rebar_bytes_to_size(u64 bytes)
{
	int log = 63 - __builtin_clzll(roundup_pow_of_two(bytes));
	return (log > 20 ? log : 20) - 20;
}

/* drm_managed mutex: initialised now, destroyed with the device */
#ifndef drmm_mutex_init
void nmx_drmm_mutex_release(struct drm_device *dev, void *lock);
#define drmm_mutex_init(dev, lock) ({						\
	mutex_init(lock);							\
	drmm_add_action_or_reset(dev, nmx_drmm_mutex_release, lock);		\
})
#endif

/* "1 item" / "2 items" and friends */
static inline const char *str_plural(size_t n) { return n == 1 ? "" : "s"; }
static inline const char *str_up_down(bool v) { return v ? "up" : "down"; }

/* a byte count as text with a binary (STRING_UNITS_2) or decimal unit */
enum string_size_units { STRING_UNITS_10 = 0, STRING_UNITS_2 = 1 };
static inline int string_get_size(u64 size, u64 blk_size, enum string_size_units units, char *buf, int len)
{
	static const char *const u2[] = { "B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB" };
	static const char *const u10[] = { "B", "kB", "MB", "GB", "TB", "PB", "EB" };
	u64 div = units == STRING_UNITS_2 ? 1024 : 1000;
	u64 v = size * blk_size;
	int i = 0;

	while (v >= div && i < 6) {
		v /= div;
		i++;
	}
	return snprintf(buf, len, "%llu %s", (unsigned long long)v, units == STRING_UNITS_2 ? u2[i] : u10[i]);
}


#include <linux/ioport.h>
#include <linux/idr.h>
#include <linux/bitmap.h>
#include <linux/jiffies.h>
#include <linux/mm_types.h>
#include <linux/pci.h>

/* KUnit is never built in: symbols exported for tests are plain, and no test is ever running */
#define EXPORT_SYMBOL_IF_KUNIT(sym) struct nmx_kunit_export_unused
#define kunit_get_current_test() NULL

/* xe's clock table wants u64 (*)(void); LinuxKPI's readers return s64 */
static inline u64 nmx_ktime_get_ns(void) { return ktime_get_ns(); }
static inline u64 nmx_ktime_get_raw_ns(void) { return ktime_get_raw_ns(); }
static inline u64 nmx_ktime_get_real_ns(void) { return ktime_get_real_ns(); }
static inline u64 nmx_ktime_get_boottime_ns(void) { return ktime_get_boottime_ns(); }
static inline u64 nmx_ktime_get_clocktai_ns(void) { return ktime_get_clocktai_ns(); }

static inline unsigned long secs_to_jiffies(unsigned int secs) { return msecs_to_jiffies(secs * 1000u); }

/* drm_managed allocation (drm-kmod 6.14 only has the zeroing form); freed with the device either way */
#ifndef drmm_kmalloc
#define drmm_kmalloc(dev, size, gfp) drmm_kzalloc((dev), (size), (gfp))
#endif

/* devm_release_action: LinuxKPI devres fork (fork_linux_devres.py) */
void lkpi_devm_release_action(struct device *dev, void (*action)(void *), void *data);
#define devm_release_action(dev, action, data) lkpi_devm_release_action((dev), (action), (data))

/* worker-thread copies into a process's memory (nmx_mm_copy.c) */
unsigned long nmx_mm_copy_to_user(struct mm_struct *mm, void __user *to, const void *from, unsigned long n);
unsigned long nmx_mm_copy_from_user(struct mm_struct *mm, void *to, const void __user *from, unsigned long n);

/*
 * The component framework binds xe's GSC proxy to Linux's mei_gsc_proxy driver. macOS has no MEI driver, so the
 * component registers and is never bound - exactly what Linux does when that module is not loaded.
 */
struct component_ops {
	int (*bind)(struct device *comp, struct device *master, void *data);
	void (*unbind)(struct device *comp, struct device *master, void *data);
};
static inline int component_add_typed(struct device *dev, const struct component_ops *ops, int subcomponent) { return 0; }
static inline void component_del(struct device *dev, const struct component_ops *ops) { }

/* PCI power and peer-to-peer facilities IOPCIFamily owns or does not offer */
static inline void pci_d3cold_enable(struct pci_dev *pdev) { }
static inline void pci_d3cold_disable(struct pci_dev *pdev) { }
static inline bool pci_pr3_present(struct pci_dev *pdev) { return false; }	/* so D3cold is never chosen */
static inline int pci_p2pdma_distance(struct pci_dev *provider, struct device *client, bool verbose) { return -1; }
static inline int pci_sriov_set_totalvfs(struct pci_dev *pdev, u16 numvfs) { return 0; }
static inline bool pci_dev_msi_enabled(struct pci_dev *pdev) { return pdev->msi_enabled || pdev->msix_enabled; }

#ifndef SET_RUNTIME_PM_OPS
#define SET_RUNTIME_PM_OPS(s, r, i) .runtime_suspend = (s), .runtime_resume = (r), .runtime_idle = (i),
#endif

/* OA streams are file descriptors made from an anonymous inode, which the port cannot create yet: opening one fails */
static inline int anon_inode_getfd(const char *name, const void *fops, void *priv, int flags) { return -ENODEV; }

static inline void vm_flags_mod(struct vm_area_struct *vma, unsigned long set, unsigned long clear)
{
	vma->vm_flags = (vma->vm_flags | set) & ~clear;
}

/* xe allocates every id with an explicit start (idr_alloc(..., 1, ...)), so the base needs no tracking */
#define idr_init_base(idr, base) idr_init(idr)

/* "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" */
static inline bool uuid_is_valid(const char *uuid)
{
	for (int i = 0; i < UUID_STRING_LEN; i++) {
		char c = uuid[i];
		if (i == 8 || i == 13 || i == 18 || i == 23) {
			if (c != '-')
				return false;
		} else if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'))) {
			return false;
		}
	}
	return true;
}

/* dst = src << shift, over nbits bits */
static inline void bitmap_shift_left(unsigned long *dst, const unsigned long *src, unsigned int shift,
				     unsigned int nbits)
{
	unsigned int words = BITS_TO_LONGS(nbits), off = shift / BITS_PER_LONG, rem = shift % BITS_PER_LONG;

	for (int k = (int)words - 1; k >= 0; k--) {
		unsigned long lo = 0, hi = 0;
		if (k - (int)off >= 0)
			hi = src[k - off] << rem;
		if (rem && k - (int)off - 1 >= 0)
			lo = src[k - off - 1] >> (BITS_PER_LONG - rem);
		dst[k] = hi | lo;
	}
	if (nbits % BITS_PER_LONG)
		dst[words - 1] &= (1UL << (nbits % BITS_PER_LONG)) - 1;
}

/* every bit set in either of two bitmaps */
#define for_each_or_bit(bit, addr1, addr2, size)					\
	for ((bit) = 0; (bit) < (size); (bit)++)					\
		if (!(test_bit((bit), (addr1)) || test_bit((bit), (addr2)))) {} else

#endif
