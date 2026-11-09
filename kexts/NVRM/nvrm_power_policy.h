// nvrm_power_policy.h: when a notebook GPU is told to run at its platform power budget (TGP mode).
//
// A notebook GPU boots at the VBIOS default power limit. The platform's real budget is lifted by sending the GPU
// firmware the TGP-mode control; it should be on only while the machine is on mains power, and a charger that goes away
// must take it off again. This is the decision only (pure C, no kernel headers) so a host test can prove it:
//
//   enable      sysctl debug.nvrm_tgp_enable: 1 = follow the power source, 0 = hold the VBIOS default
//   ac          last power-source reading: 1 mains, 0 battery, -1 unknown
//   state       what the firmware was last told: 1 on, 0 off, -1 nothing yet
//
// The answer is the value to send, or -1 to send nothing. An unknown power source keeps what was applied rather than
// flapping, but the first decision needs a reading.

#ifndef NVRM_POWER_POLICY_H
#define NVRM_POWER_POLICY_H

#define NVRM_TGP_MAX_FAILS 5

static inline int nvrm_tgp_want(int enable, int ac, int state)
{
    int want = (ac < 0) ? state : ((enable && ac == 1) ? 1 : 0);
    if (want < 0 || want == state) return -1;
    return want;
}

// After this many refused controls in a row the manager stops trying until the next resume.
static inline int nvrm_tgp_gave_up(unsigned fails) { return fails >= NVRM_TGP_MAX_FAILS; }

// A suspended GPU cannot take a control, and the firmware forgets TGP mode across a power cycle: the manager is held
// while paused and starts again from "nothing sent" when the GPU is back.
static inline int nvrm_tgp_state_after_resume(void) { return -1; }

#endif
