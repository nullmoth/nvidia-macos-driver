/* nvmtl_loss.h - what the log says when the Vulkan device is lost. Pure text, so tools/test_loss.c proves it on the host.
   A lost device used to leave one line ("fence wait -> -4"), and WindowServer's later abort said nothing about why. */
#ifndef NVMTL_LOSS_H
#define NVMTL_LOSS_H
#include <stdio.h>
#include <string.h>

static inline const char *nvmtl_vk_result_name(int r)
{
    switch (r) {
    case 0: return "VK_SUCCESS";
    case 2: return "VK_TIMEOUT";
    case -1: return "VK_ERROR_OUT_OF_HOST_MEMORY";
    case -2: return "VK_ERROR_OUT_OF_DEVICE_MEMORY";
    case -3: return "VK_ERROR_INITIALIZATION_FAILED";
    case -4: return "VK_ERROR_DEVICE_LOST";
    case -5: return "VK_ERROR_MEMORY_MAP_FAILED";
    default: return "an unknown VkResult";
    }
}
/* One sentence naming what ended the device: where it was noticed, the result, how long the wait ran, which caller was
   submitting, how long the device had been up, and what that combination means. `where` is "submit" or "fence". */
static inline void nvmtl_loss_cause(char *out, size_t cap, const char *where, int vkresult, long waited_ms, const char *who,
                                    long up_seconds, const char *process)
{
    const int submit = where && !strcmp(where, "submit");
    const char *meaning =
        vkresult == -4 && submit ? "the GPU channel was already dead when work was queued (an earlier fault or reset; see the fault-context lines above)" :
        vkresult == -4 ? "the GPU faulted or was reset while this work ran (see the fault-context lines above)" :
        vkresult == 2 ? "the fence neither signalled nor reported an error for the whole give-up time: a stalled channel" :
        (vkresult == -1 || vkresult == -2) ? "the driver ran out of memory while submitting, not a GPU fault" :
        "an unexpected result from the Vulkan driver";
    snprintf(out, cap, "device lost in %s: %s -> %s (%d) after %ld ms, submitted for %s, device up %ld s: %s",
             process ? process : "?", submit ? "queue submit" : "fence wait", nvmtl_vk_result_name(vkresult),
             vkresult, waited_ms, who ? who : "?", up_seconds, meaning);
}
#endif
