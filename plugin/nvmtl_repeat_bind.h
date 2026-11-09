/* nvmtl_repeat_bind.h - when a render encoder may skip work it already did.
   Two per-draw savings on the render encoder, each with its own switch so a rendering glitch can be A/B'd against it
   without a new build (the switches are read from the environment, which a release plugin fills from nvrm610.conf):
     NVMTL_NO_PSO_SKIP=1  set the pipeline again even when it is the one already bound inside the begun pass
     NVMTL_NO_SV_MEMO=1   look the sampler variant up on every draw instead of reusing this encoder's last choice
   The decisions are pure so tools/test_repeat_bind.c proves them without Metal. The idea comes from the NvGfx project
   (watchthelight-labs/NvGfx); this implementation is our own. */
#ifndef NVMTL_REPEAT_BIND_H
#define NVMTL_REPEAT_BIND_H
#include <stddef.h>

/* a switch that turns a saving OFF: set, non-empty, and not "0" */
static inline int nvmtl_switch_is_off(const char *value)
{
    return value && *value && !(value[0] == '0' && value[1] == 0);
}

/* The same pipeline again inside a begun pass changes nothing. Not inside an open pass-time pool, where every bind
   belongs to the pool. */
static inline int nvmtl_pso_skip(int skipOn, const void *incoming, const void *bound, int passBegun, int passTimePoolOpen)
{
    return skipOn && incoming && incoming == bound && passBegun && !passTimePoolOpen;
}

/* The encoder's last sampler-variant choice stands while the pipeline and the value-snapshot key are the same objects.
   A refused variant is remembered too (the caller stores a sentinel), so a draw that cannot be specialised does not
   retry the translator on every call. */
static inline int nvmtl_sv_memo_hit(int memoOn, const void *lastPipeline, const void *pipeline,
                                    const void *lastKey, const void *key, int haveChoice)
{
    return memoOn && haveChoice && pipeline && key && lastPipeline == pipeline && lastKey == key;
}

#endif
