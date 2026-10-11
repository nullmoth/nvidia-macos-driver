/* SPDX-License-Identifier: Apache-2.0 */
/*
 * Linux kernel AIO types, which XRT's PCIe layer declares for its Alveo DMA path. macOS has no kernel AIO; the types
 * exist so the layer compiles, and the AIO set-up reports failure (the NPU never uses that path).
 */
#ifndef NMX_COMPAT_LINUX_AIO_ABI_H
#define NMX_COMPAT_LINUX_AIO_ABI_H
#include <stdint.h>
typedef unsigned long aio_context_t;
struct io_event {
	uint64_t data;
	uint64_t obj;
	int64_t res;
	int64_t res2;
};
struct iocb {
	uint64_t aio_data;
	uint32_t aio_key;
	int32_t aio_rw_flags;
	uint16_t aio_lio_opcode;
	int16_t aio_reqprio;
	uint32_t aio_fildes;
	uint64_t aio_buf;
	uint64_t aio_nbytes;
	int64_t aio_offset;
	uint64_t aio_reserved2;
	uint32_t aio_flags;
	uint32_t aio_resfd;
};
#define IOCB_CMD_PREAD 0
#define IOCB_CMD_PWRITE 1
#define IOCB_CMD_PREADV 7
#define IOCB_CMD_PWRITEV 8
#endif
