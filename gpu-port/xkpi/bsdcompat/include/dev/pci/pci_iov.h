/* FreeBSD PCI SR-IOV: not supported on macOS (no virtual functions are created); the method types still compile */
#pragma once
#include <sys/nv.h>
#include <sys/bus.h>
typedef int pci_iov_init_t(device_t dev, uint16_t num_vfs, const nvlist_t *config);
typedef void pci_iov_uninit_t(device_t dev);
typedef int pci_iov_add_vf_t(device_t dev, uint16_t vfnum, const nvlist_t *config);
