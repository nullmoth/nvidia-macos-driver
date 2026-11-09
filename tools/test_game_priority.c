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
    return fails ? 1 : 0;
}
