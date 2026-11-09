/* nvmtl_game_priority.h - the GPU side of Game Mode for a game's own process.
 *
 * The Vulkan driver (nvk-macos.patch, "tsgsched") already knows two knobs for the channel group it makes for a graphics
 * queue, both applied with the open kernel modules' controls: NVRM_TSG_Q0_HIGH=1 sets the group's interleave level to HIGH
 * (NVA06C_CTRL_CMD_SET_INTERLEAVE_LEVEL, 0xa06c0107, level 2) and NVRM_TSG_TIMESLICE_US=<n> sets its timeslice
 * (NVA06C_CTRL_CMD_SET_TIMESLICE, 0xa06c0103, microseconds). They are read when the queue's context is made, so they have to
 * be set before the Vulkan device is created. This header decides, for one process, whether to set them and to what.
 *
 *   NVMTL_GAME_GPU_PRIORITY=1   the switch; off, nothing is set
 *   NVMTL_GAME_TIMESLICE_US=<n> the timeslice a game's group gets (default 4000; 0 leaves the hardware default)
 *
 * It only ever RAISES the process it runs in, and only when the process is a game (the games category, or an opt-in to Game
 * Mode in its Info.plist: the same two things macOS's own Game Mode daemon reads). Other clients cannot be lowered from here:
 * a channel group can be changed only by the process that owns its handle. */
#ifndef NVMTL_GAME_PRIORITY_H
#define NVMTL_GAME_PRIORITY_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NVMTL_GAME_DEFAULT_TIMESLICE_US 4000UL

struct nvmtl_game_priority {
    int apply;                  /* 0: set nothing */
    int interleave_high;        /* NVRM_TSG_Q0_HIGH=1 */
    unsigned long timeslice_us; /* NVRM_TSG_TIMESLICE_US, 0 = leave it */
};

static inline struct nvmtl_game_priority nvmtl_game_priority_decide(const char *sw, int process_is_game, const char *timeslice_env)
{
    struct nvmtl_game_priority r = { 0, 0, 0 };
    if (!sw || strcmp(sw, "1") || !process_is_game) return r;
    r.apply = 1;
    r.interleave_high = 1;
    r.timeslice_us = NVMTL_GAME_DEFAULT_TIMESLICE_US;
    if (timeslice_env && *timeslice_env) {
        char *end = 0;
        unsigned long v = strtoul(timeslice_env, &end, 10);
        if (end && !*end && v <= 1000000UL) r.timeslice_us = v;
    }
    return r;
}

/* Sets the two knobs in the environment unless the user already set them (a user's own value always wins). */
static inline void nvmtl_game_priority_apply(const struct nvmtl_game_priority *p)
{
    if (!p->apply) return;
    if (p->interleave_high) setenv("NVRM_TSG_Q0_HIGH", "1", 0);
    if (p->timeslice_us) {
        char b[24];
        snprintf(b, sizeof b, "%lu", p->timeslice_us);
        setenv("NVRM_TSG_TIMESLICE_US", b, 0);
    }
}

#endif
