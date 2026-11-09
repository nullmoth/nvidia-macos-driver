// nvmtl_claims.h - what the Metal device claims to apps, decided from what the Vulkan device can back.
//
// Each claim is tied to the Vulkan capability that implements it, so a GPU without the capability does not advertise
// the Metal feature (an app that trusts the claim and uses the feature would get a pipeline the driver cannot build).
//
//   ray tracing        VK_KHR_ray_query + VK_KHR_acceleration_structure (+ deferred host operations)
//   barycentrics       VK_KHR_fragment_shader_barycentric: the translator emits BaryCoordKHR / BaryCoordNoPerspKHR
//   Metal 3 family     always: mesh and object shaders are lowered to compute plus vertex work in the plugin
//                      (NVMTLLibraryLoad.m, xlate_lower_mesh2), so they do not depend on VK_EXT_mesh_shader
//
// NVIDIA: every card the driver supports (Turing and newer) has both extensions in NVK, so its claims are unchanged.
// tools/test_claims.c checks that, and checks that a GCN-class device without the barycentric extension does not
// claim it; built with -DNVMTL_CLAIMS_UNGATED (the old, unconditional claims) that second part must fail.
#ifndef NVMTL_CLAIMS_H
#define NVMTL_CLAIMS_H

struct nvmtl_caps {
    int barycentric;    // VK_KHR_fragment_shader_barycentric advertised with fragmentShaderBarycentric
    int raytracing;     // the ray-query set the device enables (nvmtl_vk_rt_available)
};

static inline int nvmtl_claim_barycentrics(const struct nvmtl_caps *c)
{
#ifdef NVMTL_CLAIMS_UNGATED
    (void)c;
    return 1;
#else
    return c->barycentric != 0;
#endif
}

static inline int nvmtl_claim_metal3(const struct nvmtl_caps *c)
{
    (void)c;
    return 1;   // mesh shaders are emulated through compute: backed on every device
}

static inline int nvmtl_claim_raytracing(const struct nvmtl_caps *c)
{
    return c->raytracing != 0;
}

#endif
