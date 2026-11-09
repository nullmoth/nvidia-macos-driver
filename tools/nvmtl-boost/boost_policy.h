/* boost_policy.h - when nvmtl-boost holds the system-wide GPU boost request.
   nvmtl-boost keeps one resource-manager client open that asks for "boost to maximum, until cleared" while a setting says
   so, and clears the request when the setting turns it off, so the boost can be switched at run time without a restart.
   This is the decision and the settings parser only (pure C), so test_boost_policy.c proves them without a GPU.
   The setting is one line, gpuboost=0 or gpuboost=1, in a root-owned file. A missing file, a missing line or an
   unreadable value means "leave things as they are": the daemon holds the request only for an explicit 1, and drops
   it only for an explicit 0 (a garbled file must neither start nor stop a boost). The idea of a run-time boost switch
   comes from the NvGfx project (watchthelight-labs/NvGfx); the code is ours. */
#ifndef NVMTL_BOOST_POLICY_H
#define NVMTL_BOOST_POLICY_H
#include <stddef.h>
#include <string.h>

enum boost_setting { BOOST_UNSET = -1, BOOST_OFF = 0, BOOST_ON = 1 };
enum boost_action { BOOST_KEEP = 0, BOOST_HOLD = 1, BOOST_RELEASE = 2 };

/* the last gpuboost=<0|1> line of a settings text (later lines win); anything else on a line is ignored */
static inline enum boost_setting boost_parse(const char *text, size_t len)
{
    enum boost_setting v = BOOST_UNSET;
    size_t i = 0;
    while (i < len) {
        size_t e = i;
        while (e < len && text[e] != '\n') e++;
        size_t s = i;
        while (s < e && (text[s] == ' ' || text[s] == '\t')) s++;
        if (e - s >= 9 && !memcmp(text + s, "gpuboost=", 9)) {
            size_t t = s + 9, u = e;
            while (u > t && (text[u - 1] == ' ' || text[u - 1] == '\t' || text[u - 1] == '\r')) u--;
            if (u - t == 1 && text[t] == '1') v = BOOST_ON;
            else if (u - t == 1 && text[t] == '0') v = BOOST_OFF;
        }
        i = e + 1;
    }
    return v;
}

/* what to do given the setting and whether the request is held now */
static inline enum boost_action boost_decide(enum boost_setting want, int held)
{
    if (want == BOOST_ON) return held ? BOOST_KEEP : BOOST_HOLD;
    if (want == BOOST_OFF) return held ? BOOST_RELEASE : BOOST_KEEP;
    return BOOST_KEEP;
}

/* a file the daemon may trust: a regular file owned by root and not writable by group or others */
static inline int boost_file_trusted(unsigned owner_uid, unsigned mode_bits)
{
    return owner_uid == 0 && (mode_bits & 0022) == 0;
}
#endif
