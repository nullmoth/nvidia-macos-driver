/*
 * The port's user-mapping records (override/src/nmx_linux_cdev.c): every vm_area_struct a driver sees belongs to
 * one. A fault handler's page inserts are collected into it; drm_gem_ttm_mmap installs a pin hook on it.
 */
#ifndef NMX_VMA_H
#define NMX_VMA_H
#include <linux/mm.h>
vm_fault_t nmx_umap_insert(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn, pgprot_t prot);
void nmx_vma_set_pin(struct vm_area_struct *vma, int (*pin)(void *arg, bool on), void *arg);
#endif
