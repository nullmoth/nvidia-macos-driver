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

/* One state drives all of it. When the Game Mode service is installed (1401 turned it on: the root-owned folder
 * /Library/NullMoth/gamemode exists), the two switches below default to on; an explicit value in the environment, 0 or 1,
 * always wins. So the CPU side, the game's own GPU priority and the lowering of other clients all follow one setting. */
static inline const char *nvmtl_game_switch(const char *env_value, int service_installed)
{
    if (env_value && *env_value) return env_value;
    return service_installed ? "1" : 0;
}

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


/* ---- the other clients while a game is active ------------------------------------------------------------------------------
 * The plugin runs inside every Metal client, so each client can lower its OWN channel group while a game is in the foreground.
 * The policy helper (the Mac app's nullmoth-gamemode-policy.py) publishes "a game is active" as the state of the notify(3) key
 * com.nullmoth.gamemode.active (1 active, 0 not) and in the root-owned file /Library/NullMoth/gamemode-active. This decides what
 * a client that is not the game does with it. It is lowered only when it is a plain application: never a system process (the
 * window server above all), never anything under /System or /usr, and only with NVMTL_GAME_LOWER_BACKGROUND=1.
 *
 *   NVMTL_GAME_BACKGROUND_TIMESLICE_US=<n>   the timeslice a lowered client gets (default 1000)
 *
 * The knobs are read when the client's queue context is made, so a client already running keeps what it has until it makes a
 * new device; a client started while the game is active is lowered from the start. */
#define NVMTL_BG_DEFAULT_TIMESLICE_US 1000UL

struct nvmtl_bg_priority { int apply; int interleave_low; unsigned long timeslice_us; };

static inline int nvmtl_bg_path_protected(const char *exe_path)
{
    if (!exe_path || !*exe_path) return 1;                 /* unknown: do not touch */
    static const char *const prefix[] = { "/System/", "/usr/", "/sbin/", "/bin/", "/Library/Apple/", "/private/var/db/", 0 };
    for (int i = 0; prefix[i]; i++) if (!strncmp(exe_path, prefix[i], strlen(prefix[i]))) return 1;
    const char *base = strrchr(exe_path, '/');
    base = base ? base + 1 : exe_path;
    static const char *const name[] = { "WindowServer", "loginwindow", "Dock", "SystemUIServer", "ControlCenter", "coreaudiod",
                                        "launchd", "kernel_task", "universalaccessd", "NVMTLWindowServerProbe", 0 };
    for (int i = 0; name[i]; i++) if (!strcmp(base, name[i])) return 1;
    return 0;
}

static inline struct nvmtl_bg_priority nvmtl_bg_priority_decide(const char *sw, int process_is_game, int game_active,
                                                                const char *exe_path, const char *timeslice_env)
{
    struct nvmtl_bg_priority r = { 0, 0, 0 };
    if (!sw || strcmp(sw, "1") || process_is_game || !game_active || nvmtl_bg_path_protected(exe_path)) return r;
    r.apply = 1;
    r.interleave_low = 1;
    r.timeslice_us = NVMTL_BG_DEFAULT_TIMESLICE_US;
    if (timeslice_env && *timeslice_env) {
        char *end = 0;
        unsigned long v = strtoul(timeslice_env, &end, 10);
        if (end && !*end && v > 0 && v <= 1000000UL) r.timeslice_us = v;
    }
    return r;
}

static inline void nvmtl_bg_priority_apply(const struct nvmtl_bg_priority *p)
{
    if (!p->apply) return;
    if (p->interleave_low) setenv("NVRM_TSG_Q0_HIGH", "0", 0);   /* 0 = interleave LOW in the NVK knob */
    if (p->timeslice_us) {
        char b[24];
        snprintf(b, sizeof b, "%lu", p->timeslice_us);
        setenv("NVRM_TSG_TIMESLICE_US", b, 0);
    }
}

/* ---- live re-prioritisation of a client that is already running -----------------------------------------------------------
 * The knobs above are read when a queue's context is made. For a client that is already running, the NVK driver exports
 * nvmtl_nvk_tsg_reprioritise_v1(level, timeslice_us), which re-applies the interleave level and timeslice on this process's
 * existing channel groups and, with level -1, puts them back to what they were. This decides when to call it. The plugin
 * calls it again each time the notify(3) key com.nullmoth.gamemode.active changes.
 *
 *   a game process (raise switch on)       active  -> HIGH with the game timeslice;  inactive -> restore
 *   another client (lower switch on)       active  -> LOW with the background timeslice; inactive -> restore
 *   a system process / unknown path        never touched
 *   a user who set NVRM_TSG_Q0_HIGH or NVRM_TSG_TIMESLICE_US themselves   never touched (their value wins)
 * `applied` is the caller's memory of whether it has changed the groups, so a repeated notification does nothing. */
enum { NVMTL_LIVE_NONE = 0, NVMTL_LIVE_SET = 1, NVMTL_LIVE_RESTORE = 2 };
struct nvmtl_live_action { int kind; int level; unsigned long timeslice_us; };

static inline struct nvmtl_live_action nvmtl_live_decide(const char *raise_sw, const char *lower_sw, int process_is_game, int game_active,
                                                         const char *exe_path, int user_pinned, int applied,
                                                         const char *game_ts_env, const char *bg_ts_env)
{
    struct nvmtl_live_action a = { NVMTL_LIVE_NONE, 0, 0 };
    if (user_pinned) return a;
    if (process_is_game) {
        struct nvmtl_game_priority g = nvmtl_game_priority_decide(raise_sw, 1, game_ts_env);
        if (!g.apply) return a;
        if (game_active && !applied) { a.kind = NVMTL_LIVE_SET; a.level = 2; a.timeslice_us = g.timeslice_us; }
        else if (!game_active && applied) { a.kind = NVMTL_LIVE_RESTORE; a.level = -1; }
        return a;
    }
    struct nvmtl_bg_priority b = nvmtl_bg_priority_decide(lower_sw, 0, 1, exe_path, bg_ts_env);   /* what it WOULD do while a game is active */
    if (!b.apply) return a;
    if (game_active && !applied) { a.kind = NVMTL_LIVE_SET; a.level = 0; a.timeslice_us = b.timeslice_us; }
    else if (!game_active && applied) { a.kind = NVMTL_LIVE_RESTORE; a.level = -1; }
    return a;
}

#endif
