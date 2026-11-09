/* test_rusd_format: the report from a synthetic shared-data block. cc -I<ogkm inc> -I. test_rusd_format.c */
#include <string.h>
#include <stdio.h>
#include "rusd_format.h"
static int fails;
static void arm(const char *n, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", n); fails += !ok; }
int main(void)
{
    static NV00DE_SHARED_DATA d; static char buf[4096];
    memset(&d, 0, sizeof d);
    FILE *f = fmemopen(buf, sizeof buf, "w"); rusd_format(&d, f); fclose(f);
    arm("an empty block prints n/a for every field, no stale numbers", strstr(buf, "temperature_c n/a") && strstr(buf, "power_w n/a") && strstr(buf, "fan_rpm n/a") && !strstr(buf, "power_w 0"));
    d.temperatures[RUSD_TEMPERATURE_TYPE_GPU].lastModifiedTimestamp = 100; d.temperatures[RUSD_TEMPERATURE_TYPE_GPU].temperature = 52 * 256 + 128;
    d.instPowerUsage.lastModifiedTimestamp = 100; d.instPowerUsage.info.instGpuPower = 83500;
    d.powerLimitGpu.lastModifiedTimestamp = 100; d.powerLimitGpu.info.requestedmW = 80000; d.powerLimitGpu.info.enforcedmW = 80000;
    d.clkPublicDomainInfos.lastModifiedTimestamp = 100; d.clkPublicDomainInfos.info[0].actualClkKHz = 1815000; d.clkPublicDomainInfos.info[1].actualClkKHz = 7001000;
    d.perfCurrentPstate.lastModifiedTimestamp = 100; d.perfCurrentPstate.currentPstate = 8;
    d.perfDevUtil.lastModifiedTimestamp = 100; d.perfDevUtil.info.gpuPercentBusy = 37;
    d.clkThrottleInfo.lastModifiedTimestamp = 100; d.clkThrottleInfo.reasonMask = RUSD_CLK_THROTTLE_REASON_SW_POWER_CAP;
    d.fanCoolerStatus.lastModifiedTimestamp = 100; d.fanCoolerStatus.info.rpmCurr[0] = 2100;
    memset(buf, 0, sizeof buf);
    f = fmemopen(buf, sizeof buf, "w"); rusd_format(&d, f); fclose(f);
    arm("temperature is read as 24.8 fixed point (52.5 C)", strstr(buf, "temperature_c 52.5") != 0);
    arm("power in mW is printed in W", strstr(buf, "power_w 83.500") != 0);
    arm("requested and enforced limits are both shown", strstr(buf, "requested 80.0 enforced 80.0") != 0);
    arm("graphics clock kHz is printed in MHz", strstr(buf, "clock_mhz_graphics 1815") != 0);
    arm("pstate, utilisation, throttle mask and fan are printed", strstr(buf, "pstate P8") && strstr(buf, "util_gpu_pct 37") && strstr(buf, "throttle_reasons 0x4") && strstr(buf, "fan_rpm 2100"));
    d.instPowerUsage.lastModifiedTimestamp = RUSD_TIMESTAMP_WRITE_IN_PROGRESS;
    memset(buf, 0, sizeof buf); f = fmemopen(buf, sizeof buf, "w"); rusd_format(&d, f); fclose(f);
    arm("a field being written is n/a, not a torn number", strstr(buf, "power_w n/a") != 0);
    printf("%d failed\n", fails); return fails != 0;
}
