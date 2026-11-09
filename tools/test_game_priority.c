/* test_game_priority.c - the GPU side of Game Mode. cc -I plugin tools/test_game_priority.c -o /tmp/t && /tmp/t */
#include <stdio.h>
#include "nvmtl_game_priority.h"
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
int main(void)
{
    struct nvmtl_game_priority p;
    p = nvmtl_game_priority_decide(0, 1, 0);
    arm("without the switch nothing is applied", !p.apply && !p.interleave_high && !p.timeslice_us);
    p = nvmtl_game_priority_decide("0", 1, 0);
    arm("a switch that is not 1 applies nothing", !p.apply);
    p = nvmtl_game_priority_decide("1", 0, 0);
    arm("a process that is not a game is never raised", !p.apply);
    p = nvmtl_game_priority_decide("1", 1, 0);
    arm("a game with the switch on gets interleave HIGH and the default timeslice", p.apply && p.interleave_high && p.timeslice_us == 4000);
    p = nvmtl_game_priority_decide("1", 1, "8000");
    arm("a timeslice given by the user is used", p.timeslice_us == 8000);
    p = nvmtl_game_priority_decide("1", 1, "0");
    arm("a timeslice of 0 leaves the hardware default alone", p.apply && p.timeslice_us == 0);
    p = nvmtl_game_priority_decide("1", 1, "abc");
    arm("a timeslice that is not a number falls back to the default", p.timeslice_us == 4000);
    p = nvmtl_game_priority_decide("1", 1, "99999999");
    arm("a timeslice above one second is refused", p.timeslice_us == 4000);
    unsetenv("NVRM_TSG_Q0_HIGH"); unsetenv("NVRM_TSG_TIMESLICE_US");
    p = nvmtl_game_priority_decide("1", 1, 0);
    nvmtl_game_priority_apply(&p);
    arm("apply sets both knobs in the environment", getenv("NVRM_TSG_Q0_HIGH") && !strcmp(getenv("NVRM_TSG_Q0_HIGH"), "1") && !strcmp(getenv("NVRM_TSG_TIMESLICE_US"), "4000"));
    setenv("NVRM_TSG_TIMESLICE_US", "1234", 1);
    nvmtl_game_priority_apply(&p);
    arm("a value the user already set is not overwritten", !strcmp(getenv("NVRM_TSG_TIMESLICE_US"), "1234"));
    unsetenv("NVRM_TSG_Q0_HIGH"); unsetenv("NVRM_TSG_TIMESLICE_US");
    p = nvmtl_game_priority_decide(0, 1, 0);
    nvmtl_game_priority_apply(&p);
    arm("with the switch off the environment is untouched", !getenv("NVRM_TSG_Q0_HIGH") && !getenv("NVRM_TSG_TIMESLICE_US"));
    /* the other clients while a game is active */
    {
        struct nvmtl_bg_priority b;
        b = nvmtl_bg_priority_decide(0, 0, 1, "/Applications/App.app/Contents/MacOS/App", 0);
        arm("without the switch no client is lowered", !b.apply);
        b = nvmtl_bg_priority_decide("1", 0, 0, "/Applications/App.app/Contents/MacOS/App", 0);
        arm("with no game active no client is lowered", !b.apply);
        b = nvmtl_bg_priority_decide("1", 1, 1, "/Applications/Game.app/Contents/MacOS/Game", 0);
        arm("the game itself is never lowered", !b.apply);
        b = nvmtl_bg_priority_decide("1", 0, 1, "/Applications/App.app/Contents/MacOS/App", 0);
        arm("an ordinary application is lowered to interleave LOW with the default timeslice", b.apply && b.interleave_low && b.timeslice_us == 1000);
        b = nvmtl_bg_priority_decide("1", 0, 1, "/Applications/App.app/Contents/MacOS/App", "500");
        arm("a timeslice given by the user is used", b.timeslice_us == 500);
        b = nvmtl_bg_priority_decide("1", 0, 1, "/Applications/App.app/Contents/MacOS/App", "0");
        arm("a timeslice of 0 is refused for a lowered client", b.timeslice_us == 1000);
        const char *protectedPaths[] = { "/System/Library/CoreServices/WindowServer", "/usr/libexec/something", "/sbin/launchd", "/Applications/X.app/Contents/MacOS/Dock",
                                         "/Applications/X.app/Contents/MacOS/WindowServer", "", 0 };
        int all = 1;
        for (int i = 0; protectedPaths[i] || i == 5; i++) { b = nvmtl_bg_priority_decide("1", 0, 1, i == 5 ? 0 : protectedPaths[i], 0); if (b.apply) all = 0; if (i == 5) break; }
        arm("system processes, the window server and an unknown path are never lowered", all);
        unsetenv("NVRM_TSG_Q0_HIGH"); unsetenv("NVRM_TSG_TIMESLICE_US");
        b = nvmtl_bg_priority_decide("1", 0, 1, "/Applications/App.app/Contents/MacOS/App", 0);
        nvmtl_bg_priority_apply(&b);
        arm("apply sets interleave LOW (0) and the timeslice, and keeps a value the user set", !strcmp(getenv("NVRM_TSG_Q0_HIGH"), "0") && !strcmp(getenv("NVRM_TSG_TIMESLICE_US"), "1000"));
        setenv("NVRM_TSG_TIMESLICE_US", "77", 1);
        nvmtl_bg_priority_apply(&b);
        arm("a user's own timeslice is kept", !strcmp(getenv("NVRM_TSG_TIMESLICE_US"), "77"));
        unsetenv("NVRM_TSG_Q0_HIGH"); unsetenv("NVRM_TSG_TIMESLICE_US");
    }
    /* live re-prioritisation of clients that are already running */
    {
        const char *app = "/Applications/App.app/Contents/MacOS/App";
        struct nvmtl_live_action a;
        a = nvmtl_live_decide("1", "1", 1, 1, "/Applications/G.app/Contents/MacOS/G", 0, 0, 0, 0);
        arm("live: a game becoming active is raised to HIGH with the game timeslice", a.kind == NVMTL_LIVE_SET && a.level == 2 && a.timeslice_us == 4000);
        a = nvmtl_live_decide("1", "1", 1, 0, "/Applications/G.app/Contents/MacOS/G", 0, 1, 0, 0);
        arm("live: the game going inactive restores", a.kind == NVMTL_LIVE_RESTORE && a.level == -1);
        a = nvmtl_live_decide("1", "1", 1, 1, "/Applications/G.app/Contents/MacOS/G", 0, 1, 0, 0);
        arm("live: a repeated active notification does nothing", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "1", 1, 0, "/Applications/G.app/Contents/MacOS/G", 0, 0, 0, 0);
        arm("live: inactive with nothing applied does nothing", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "1", 0, 1, app, 0, 0, 0, 0);
        arm("live: another client becoming lowered goes to LOW with the background timeslice", a.kind == NVMTL_LIVE_SET && a.level == 0 && a.timeslice_us == 1000);
        a = nvmtl_live_decide("1", "1", 0, 0, app, 0, 1, 0, 0);
        arm("live: another client restores when the game ends", a.kind == NVMTL_LIVE_RESTORE && a.level == -1);
        a = nvmtl_live_decide("1", "1", 0, 1, "/System/Library/CoreServices/WindowServer", 0, 0, 0, 0);
        arm("live: the window server is never lowered", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "1", 0, 1, "", 0, 0, 0, 0);
        arm("live: an unknown path is never lowered", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "1", 0, 1, app, 1, 0, 0, 0);
        arm("live: a user's own NVRM_TSG setting means no live change", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide(0, 0, 0, 1, app, 0, 0, 0, 0);
        arm("live: with both switches off nothing is done", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("0", "1", 1, 1, "/Applications/G.app/Contents/MacOS/G", 0, 0, 0, 0);
        arm("live: raise switch off leaves a game alone (lower switch is for others)", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "0", 0, 1, app, 0, 0, 0, 0);
        arm("live: lower switch off leaves other clients alone", a.kind == NVMTL_LIVE_NONE);
        a = nvmtl_live_decide("1", "1", 0, 1, app, 0, 0, 0, "250");
        arm("live: a background timeslice from the user is used", a.timeslice_us == 250);
        a = nvmtl_live_decide("1", "1", 1, 1, "/Applications/G.app/Contents/MacOS/G", 0, 0, "9000", 0);
        arm("live: a game timeslice from the user is used", a.timeslice_us == 9000);
        /* a full cycle with the caller's memory, as the plugin keeps it */
        int applied = 0, calls = 0, restores = 0;
        int seq[] = { 1, 1, 0, 0, 1, 0 };
        for (int i = 0; i < 6; i++) {
            a = nvmtl_live_decide("1", "1", 0, seq[i], app, 0, applied, 0, 0);
            if (a.kind == NVMTL_LIVE_SET) { applied = 1; calls++; }
            if (a.kind == NVMTL_LIVE_RESTORE) { applied = 0; restores++; }
        }
        arm("live: 1,1,0,0,1,0 gives two lowerings and two restores, ending restored", calls == 2 && restores == 2 && applied == 0);
    }
    /* one state */
    arm("with the service installed the switch defaults to on", nvmtl_game_switch(0, 1) && !strcmp(nvmtl_game_switch(0, 1), "1") && !strcmp(nvmtl_game_switch("", 1), "1"));
    arm("without the service and without a setting the switch is off", nvmtl_game_switch(0, 0) == 0);
    arm("an explicit 0 wins over the service and an explicit 1 works without it", !strcmp(nvmtl_game_switch("0", 1), "0") && !strcmp(nvmtl_game_switch("1", 0), "1"));
    p = nvmtl_game_priority_decide(nvmtl_game_switch("0", 1), 1, 0);
    arm("an explicit 0 therefore applies nothing even for a game", !p.apply);
    return fails ? 1 : 0;
}
