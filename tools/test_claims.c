// test_claims.c - the Metal claims nvmtl_claims.h makes from a device's Vulkan capabilities.
//
// Build twice (tools/test_claims.sh): as is, every arm must pass; with -DNVMTL_CLAIMS_UNGATED (the claims as they
// were, made on every device) the GCN arm must fail, which shows the gate is what keeps that device honest.
#include <stdio.h>
#include "../plugin/nvmtl_claims.h"

static int bad;
static void arm(const char *name, int ok)
{
    printf("%s  %s\n", ok ? "PASS" : "FAIL", name);
    bad += !ok;
}

int main(void)
{
    // NVK on every supported NVIDIA card (Turing and newer): the barycentric extension and the ray-query set
    const struct nvmtl_caps nvk = { 1, 1 };
    // RADV on GCN 1-4 and the Vega APUs (drm-shim caps, 11-03): no barycentric extension, no ray query
    const struct nvmtl_caps gcn = { 0, 0 };
    // RADV on RDNA 2-4: both
    const struct nvmtl_caps rdna = { 1, 1 };

    arm("NVIDIA (NVK) claims exactly what it claimed before: barycentrics, Metal 3, ray tracing",
        nvmtl_claim_barycentrics(&nvk) && nvmtl_claim_metal3(&nvk) && nvmtl_claim_raytracing(&nvk));
    arm("RDNA 2-4 claim barycentrics, Metal 3 and ray tracing",
        nvmtl_claim_barycentrics(&rdna) && nvmtl_claim_metal3(&rdna) && nvmtl_claim_raytracing(&rdna));
    arm("GCN and Vega APUs claim Metal 3 (mesh is compute-emulated) but not barycentrics or ray tracing",
        !nvmtl_claim_barycentrics(&gcn) && nvmtl_claim_metal3(&gcn) && !nvmtl_claim_raytracing(&gcn));
    printf("%d of 3 arms pass\n", 3 - bad);
    return bad ? 1 : 0;
}
