// test_nvrm_power_policy.c: when the notebook power budget (TGP mode) is sent to the GPU firmware.
//
//   cc -std=c11 -Wall -Wextra -I../kexts/NVRM -o /tmp/test_nvrm_power_policy test_nvrm_power_policy.c && /tmp/test_nvrm_power_policy
//
// Negative controls that must FIRE: a policy that sent the budget on battery, never took it off when the charger went,
// flapped on an unknown reading, or kept state across a resume fails here.

#include "nvrm_power_policy.h"
#include <stdio.h>

static int g_fail;
#define CHECK(cond, msg) do { if (!(cond)) { printf("  FAIL: %s\n", msg); g_fail++; } } while (0)

int main(void)
{
    CHECK(nvrm_tgp_want(1, 1, -1) == 1, "on mains with nothing sent yet, TGP mode goes on");
    CHECK(nvrm_tgp_want(1, 1, 1) == -1, "already on: nothing is sent again");
    CHECK(nvrm_tgp_want(1, 0, -1) == 0, "on battery with nothing sent yet, the first decision is off");
    CHECK(nvrm_tgp_want(1, 0, 1) == 0, "the charger went away: TGP mode is taken off");
    CHECK(nvrm_tgp_want(1, 1, 0) == 1, "the charger came back: TGP mode goes on again");
    CHECK(nvrm_tgp_want(0, 1, 1) == 0, "the live rollback (enable 0) takes it off even on mains");
    CHECK(nvrm_tgp_want(0, 1, -1) == 0, "with enable 0 it is never turned on");
    CHECK(nvrm_tgp_want(1, -1, 1) == -1, "an unknown power source keeps what was applied (no flapping)");
    CHECK(nvrm_tgp_want(1, -1, -1) == -1, "an unknown power source with nothing sent sends nothing (needs a reading)");
    CHECK(!nvrm_tgp_gave_up(NVRM_TGP_MAX_FAILS - 1) && nvrm_tgp_gave_up(NVRM_TGP_MAX_FAILS), "five refusals in a row stop the manager");
    CHECK(nvrm_tgp_state_after_resume() == -1, "after a resume the firmware has forgotten: start from nothing sent");
    CHECK(nvrm_tgp_want(1, 1, nvrm_tgp_state_after_resume()) == 1, "after a resume on mains it is sent again");

    printf(g_fail ? "FAILED %d\n" : "ok\n", g_fail);
    return g_fail != 0;
}
