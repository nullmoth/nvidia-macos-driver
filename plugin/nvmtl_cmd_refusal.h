/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#ifndef NVMTL_CMD_REFUSAL_H
#define NVMTL_CMD_REFUSAL_H
/*
 * A refused command buffer: why it was refused.
 *
 * -[MTLCommandQueue commandBuffer] returns nil when the Vulkan side cannot begin a command buffer. WindowServer
 * does not tolerate that: CoreDisplay asserts "Failed to get a MTLCommandBuffer" and the session restarts. The cause
 * is usually an earlier failure that disabled the backend for the rest of the process, so the line that names the
 * refusal says which case it is. The texts are macros so that each cause is logged from its own call site: nvlog
 * throttles by format string, and one shared format would let a flood of one cause hide the first line of another.
 */
typedef enum {
    NVMTL_REFUSE_NONE = 0,
    NVMTL_REFUSE_BACKEND_DOWN,   /* the backend is not running in this process */
    NVMTL_REFUSE_POOL,           /* vkCreateCommandPool failed */
    NVMTL_REFUSE_ALLOC,          /* vkAllocateCommandBuffers failed */
    NVMTL_REFUSE_BEGIN           /* vkBeginCommandBuffer failed */
} nvmtl_refusal;

#define NVMTL_REFUSE_TEXT_BACKEND_DOWN \
    "the Vulkan backend is not running in this process: it failed to start, or a submit, a fence wait or the " \
    "completion check disabled it (look for 'disabling backend' or a start-up failure above)"
#define NVMTL_REFUSE_TEXT_POOL  "vkCreateCommandPool failed"
#define NVMTL_REFUSE_TEXT_ALLOC "vkAllocateCommandBuffers failed"
#define NVMTL_REFUSE_TEXT_BEGIN "vkBeginCommandBuffer failed"

static inline const char *nvmtl_refusal_text(nvmtl_refusal why)
{
    switch (why) {
    case NVMTL_REFUSE_BACKEND_DOWN: return NVMTL_REFUSE_TEXT_BACKEND_DOWN;
    case NVMTL_REFUSE_POOL:         return NVMTL_REFUSE_TEXT_POOL;
    case NVMTL_REFUSE_ALLOC:        return NVMTL_REFUSE_TEXT_ALLOC;
    case NVMTL_REFUSE_BEGIN:        return NVMTL_REFUSE_TEXT_BEGIN;
    default:                        return "no refusal";
    }
}
#endif
