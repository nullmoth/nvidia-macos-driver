/* hwrt_probe: builds a bottom and top level structure through the driver under test, queries a ray set through a compute
 * shader, and compares every ray with a CPU reference. Run it twice on the same scene, once with NVK_HWRT_QUERY=1 and once
 * without, through the bounded GPU runner; the two runs must give the same answers (docs/HWRT.md, P4). With --dump <dir> it also
 * writes the bottom-level structure's bytes as blas_<case>.bin for hwrt_layout_check.py.
 *
 *   glslangValidator -V --target-env vulkan1.2 hwrt_probe.comp -o hwrt_probe.spv
 *   cc -O1 -o hwrt_probe hwrt_probe.c -lm            (x86_64 on the target)
 *   NVK_HWRT_QUERY=1 ./hwrt_probe <vulkan loader dylib> hwrt_probe.spv <case> [--dump <dir>]
 *
 * <case>: A (one triangle), nNN (NN triangles in a strip), big (one triangle 100 times the others plus seven small).
 * Exit 0 only when every ray matches. Written to compile against the Vulkan headers; it has not been run on a card. */
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>

#define CK(x) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { fprintf(stderr, "FAIL %s = %d (line %d)\n", #x, r_, __LINE__); exit(2); } } while (0)

static PFN_vkGetInstanceProcAddr gipa;
static PFN_vkGetDeviceProcAddr gdpa;
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue queue; static uint32_t qfam;
static VkPhysicalDeviceMemoryProperties memprops;
#define I(n) PFN_##n n = (PFN_##n)gipa(inst, #n)
#define D(n) PFN_##n n = (PFN_##n)gdpa(dev, #n)

struct buf { VkBuffer b; VkDeviceMemory m; VkDeviceAddress addr; void *map; VkDeviceSize size; };

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want)
{
   for (uint32_t i = 0; i < memprops.memoryTypeCount; i++)
      if ((bits & (1u << i)) && (memprops.memoryTypes[i].propertyFlags & want) == want) return i;
   fprintf(stderr, "FAIL no memory type\n"); exit(2);
}

static struct buf mkbuf(VkDeviceSize size, VkBufferUsageFlags usage)
{
   D(vkCreateBuffer); D(vkGetBufferMemoryRequirements); D(vkAllocateMemory); D(vkBindBufferMemory); D(vkMapMemory); D(vkGetBufferDeviceAddress);
   struct buf r = { .size = size };
   VkBufferCreateInfo bi = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size, usage | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT, VK_SHARING_MODE_EXCLUSIVE };
   CK(vkCreateBuffer(dev, &bi, NULL, &r.b));
   VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, r.b, &mr);
   VkMemoryAllocateFlagsInfo fi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT };
   VkMemoryAllocateInfo ai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &fi, mr.size,
                               memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
   CK(vkAllocateMemory(dev, &ai, NULL, &r.m)); CK(vkBindBufferMemory(dev, r.b, r.m, 0));
   CK(vkMapMemory(dev, r.m, 0, VK_WHOLE_SIZE, 0, &r.map));
   VkBufferDeviceAddressInfo da = { VK_STRUCTURE_TYPE_BUFFER_DEVICE_ADDRESS_INFO, NULL, r.b };
   r.addr = vkGetBufferDeviceAddress(dev, &da);
   return r;
}

/* ---- the scene and the CPU reference ------------------------------------------------------------------------------- */
static float tris[64 * 9]; static int ntri;

static void scene(const char *c)
{
   if (!strcmp(c, "A")) { float t[9] = { 0, 0, 0, 1, 0, 0, 0, 1, 0 }; memcpy(tris, t, sizeof t); ntri = 1; }
   else if (!strcmp(c, "big")) {
      ntri = 8;
      for (int i = 0; i < 8; i++) { float s = i == 0 ? 100.f : 1.f, x = i * 3.f; float t[9] = { x, 0, 0, x + s, 0, 0, x, s, 0 }; memcpy(tris + 9 * i, t, sizeof t); }
   } else if (c[0] == 'n') {
      ntri = atoi(c + 1); if (ntri < 1 || ntri > 64) { fprintf(stderr, "case n1..n64\n"); exit(2); }
      for (int i = 0; i < ntri; i++) { float x = 2.f * i; float t[9] = { x, 0, 0, x + 1, 0, 0, x, 1, 0 }; memcpy(tris + 9 * i, t, sizeof t); }
   } else { fprintf(stderr, "unknown case %s\n", c); exit(2); }
}

/* nearest hit by Moller-Trumbore; flags handled: cull back (0x10) and cull front (0x20), front = the ray arrives against the normal */
static int ref_hit(const float *o, const float *d, float tmin, float tmax, uint32_t flags, float *tout, uint32_t *prim, int *front)
{
   int hit = 0;
   for (int i = 0; i < ntri; i++) {
      const float *v = tris + 9 * i;
      float e1[3], e2[3], p[3], s[3], q[3];
      for (int k = 0; k < 3; k++) { e1[k] = v[3 + k] - v[k]; e2[k] = v[6 + k] - v[k]; s[k] = o[k] - v[k]; }
      p[0] = d[1] * e2[2] - d[2] * e2[1]; p[1] = d[2] * e2[0] - d[0] * e2[2]; p[2] = d[0] * e2[1] - d[1] * e2[0];
      float det = e1[0] * p[0] + e1[1] * p[1] + e1[2] * p[2];
      if (det == 0.f) continue;
      float inv = 1.f / det, u = (s[0] * p[0] + s[1] * p[1] + s[2] * p[2]) * inv;
      if (u < 0 || u > 1) continue;
      q[0] = s[1] * e1[2] - s[2] * e1[1]; q[1] = s[2] * e1[0] - s[0] * e1[2]; q[2] = s[0] * e1[1] - s[1] * e1[0];
      float w = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) * inv;
      if (w < 0 || u + w > 1) continue;
      float t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) * inv;
      if (t < tmin || t > tmax) continue;
      int f = det > 0.f;
      if (f && (flags & 0x20)) continue;
      if (!f && (flags & 0x10)) continue;
      if (!hit || t < *tout) { hit = 1; *tout = t; *prim = (uint32_t)i; *front = f; }
   }
   return hit;
}

int main(int argc, char **argv)
{
   if (argc < 4) { fprintf(stderr, "usage: hwrt_probe <vulkan loader> <spv> <case> [--dump <dir>]\n"); return 2; }
   const char *dumpdir = argc >= 6 && !strcmp(argv[4], "--dump") ? argv[5] : NULL;
   scene(argv[3]);
   void *h = dlopen(argv[1], RTLD_NOW); if (!h) { fprintf(stderr, "FAIL dlopen %s: %s\n", argv[1], dlerror()); return 2; }
   gipa = (PFN_vkGetInstanceProcAddr)dlsym(h, "vkGetInstanceProcAddr");
   PFN_vkCreateInstance cre = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
   VkApplicationInfo ap = { VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "hwrt_probe", 1, "none", 0, VK_API_VERSION_1_3 };
   VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &ap };
   CK(cre(&ici, NULL, &inst));
   I(vkEnumeratePhysicalDevices); I(vkGetPhysicalDeviceMemoryProperties); I(vkGetPhysicalDeviceQueueFamilyProperties); I(vkCreateDevice);
   uint32_t n = 1; VkResult er = vkEnumeratePhysicalDevices(inst, &n, &pd);
   if ((er != VK_SUCCESS && er != VK_INCOMPLETE) || n == 0) { fprintf(stderr, "FAIL no physical device (%d)\n", er); return 2; }
   vkGetPhysicalDeviceMemoryProperties(pd, &memprops);
   uint32_t nq = 8; VkQueueFamilyProperties qp[8]; vkGetPhysicalDeviceQueueFamilyProperties(pd, &nq, qp);
   for (qfam = 0; qfam < nq && !(qp[qfam].queueFlags & VK_QUEUE_COMPUTE_BIT); qfam++) {}
   float prio = 1.f;
   VkDeviceQueueCreateInfo qi = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, qfam, 1, &prio };
   VkPhysicalDeviceAccelerationStructureFeaturesKHR asf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ACCELERATION_STRUCTURE_FEATURES_KHR, NULL, VK_TRUE };
   VkPhysicalDeviceRayQueryFeaturesKHR rqf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_RAY_QUERY_FEATURES_KHR, &asf, VK_TRUE };
   VkPhysicalDeviceVulkan12Features f12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, &rqf };
   f12.bufferDeviceAddress = VK_TRUE;
   const char *ext[] = { "VK_KHR_acceleration_structure", "VK_KHR_ray_query", "VK_KHR_deferred_host_operations" };
   VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &f12, 0, 1, &qi, 0, NULL, 3, ext };
   CK(vkCreateDevice(pd, &dci, NULL, &dev));
   gdpa = (PFN_vkGetDeviceProcAddr)gipa(inst, "vkGetDeviceProcAddr");
   D(vkGetDeviceQueue); D(vkCreateCommandPool); D(vkAllocateCommandBuffers); D(vkBeginCommandBuffer); D(vkEndCommandBuffer); D(vkQueueSubmit);
   D(vkQueueWaitIdle); D(vkCreateAccelerationStructureKHR); D(vkGetAccelerationStructureBuildSizesKHR); D(vkCmdBuildAccelerationStructuresKHR);
   D(vkGetAccelerationStructureDeviceAddressKHR); D(vkCreateShaderModule); D(vkCreateDescriptorSetLayout); D(vkCreatePipelineLayout);
   D(vkCreateComputePipelines); D(vkCreateDescriptorPool); D(vkAllocateDescriptorSets); D(vkUpdateDescriptorSets); D(vkCmdBindPipeline);
   D(vkCmdBindDescriptorSets); D(vkCmdPushConstants); D(vkCmdDispatch);
   vkGetDeviceQueue(dev, qfam, 0, &queue);
   VkCommandPool pool; VkCommandPoolCreateInfo pci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, qfam };
   CK(vkCreateCommandPool(dev, &pci, NULL, &pool));
   VkCommandBuffer cb; VkCommandBufferAllocateInfo cai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1 };
   CK(vkAllocateCommandBuffers(dev, &cai, &cb));

#define SUBMIT(body) do { VkCommandBufferBeginInfo bi_ = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL, VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; \
   CK(vkBeginCommandBuffer(cb, &bi_)); body; CK(vkEndCommandBuffer(cb)); \
   VkSubmitInfo si_ = { VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &cb }; CK(vkQueueSubmit(queue, 1, &si_, VK_NULL_HANDLE)); CK(vkQueueWaitIdle(queue)); } while (0)

   /* bottom level */
   struct buf vb = mkbuf(sizeof(float) * 9 * ntri, VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR);
   memcpy(vb.map, tris, sizeof(float) * 9 * ntri);
   VkAccelerationStructureGeometryKHR g = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR };
   g.geometryType = VK_GEOMETRY_TYPE_TRIANGLES_KHR; g.flags = VK_GEOMETRY_OPAQUE_BIT_KHR;
   g.geometry.triangles = (VkAccelerationStructureGeometryTrianglesDataKHR){ VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_TRIANGLES_DATA_KHR, NULL,
      VK_FORMAT_R32G32B32_SFLOAT, { vb.addr }, 12, (uint32_t)(3 * ntri - 1), VK_INDEX_TYPE_NONE_KHR };
   VkAccelerationStructureBuildGeometryInfoKHR bgi = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR, NULL,
      VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR, VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR,
      VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR, VK_NULL_HANDLE, VK_NULL_HANDLE, 1, &g };
   uint32_t cnt = (uint32_t)ntri;
   VkAccelerationStructureBuildSizesInfoKHR sz = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_SIZES_INFO_KHR };
   vkGetAccelerationStructureBuildSizesKHR(dev, VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR, &bgi, &cnt, &sz);
   struct buf bstore = mkbuf(sz.accelerationStructureSize, VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR);
   struct buf bscratch = mkbuf(sz.buildScratchSize + 4096, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
   VkAccelerationStructureKHR blas, tlas;
   VkAccelerationStructureCreateInfoKHR ci = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_CREATE_INFO_KHR, NULL, 0, bstore.b, 0, sz.accelerationStructureSize, VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR };
   CK(vkCreateAccelerationStructureKHR(dev, &ci, NULL, &blas));
   bgi.dstAccelerationStructure = blas; bgi.scratchData.deviceAddress = (bscratch.addr + 255) & ~255ull;
   VkAccelerationStructureBuildRangeInfoKHR range = { cnt, 0, 0, 0 }; const VkAccelerationStructureBuildRangeInfoKHR *pr = &range;
   SUBMIT(vkCmdBuildAccelerationStructuresKHR(cb, 1, &bgi, &pr));
   VkAccelerationStructureDeviceAddressInfoKHR dai = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_DEVICE_ADDRESS_INFO_KHR, NULL, blas };
   VkDeviceAddress blas_addr = vkGetAccelerationStructureDeviceAddressKHR(dev, &dai);
   if (dumpdir) {
      char path[1024]; snprintf(path, sizeof path, "%s/blas_%s.bin", dumpdir, argv[3]);
      FILE *f = fopen(path, "wb"); if (f) { fwrite(bstore.map, 1, sz.accelerationStructureSize, f); fclose(f); printf("dumped %llu bytes to %s\n", (unsigned long long)sz.accelerationStructureSize, path); }
   }

   /* top level: one identity instance */
   struct buf ib = mkbuf(sizeof(VkAccelerationStructureInstanceKHR), VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR);
   VkAccelerationStructureInstanceKHR in = { .transform = { { { 1, 0, 0, 0 }, { 0, 1, 0, 0 }, { 0, 0, 1, 0 } } }, .instanceCustomIndex = 0, .mask = 0xff,
                                             .instanceShaderBindingTableRecordOffset = 0, .flags = 0, .accelerationStructureReference = blas_addr };
   memcpy(ib.map, &in, sizeof in);
   VkAccelerationStructureGeometryKHR tg = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR };
   tg.geometryType = VK_GEOMETRY_TYPE_INSTANCES_KHR;
   tg.geometry.instances = (VkAccelerationStructureGeometryInstancesDataKHR){ VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_INSTANCES_DATA_KHR, NULL, VK_FALSE, { ib.addr } };
   VkAccelerationStructureBuildGeometryInfoKHR tbi = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR, NULL,
      VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR, VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR, VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR,
      VK_NULL_HANDLE, VK_NULL_HANDLE, 1, &tg };
   uint32_t one = 1; VkAccelerationStructureBuildSizesInfoKHR tsz = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_SIZES_INFO_KHR };
   vkGetAccelerationStructureBuildSizesKHR(dev, VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR, &tbi, &one, &tsz);
   struct buf tstore = mkbuf(tsz.accelerationStructureSize, VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR);
   struct buf tscratch = mkbuf(tsz.buildScratchSize + 4096, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
   VkAccelerationStructureCreateInfoKHR tci = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_CREATE_INFO_KHR, NULL, 0, tstore.b, 0, tsz.accelerationStructureSize, VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR };
   CK(vkCreateAccelerationStructureKHR(dev, &tci, NULL, &tlas));
   tbi.dstAccelerationStructure = tlas; tbi.scratchData.deviceAddress = (tscratch.addr + 255) & ~255ull;
   VkAccelerationStructureBuildRangeInfoKHR trange = { 1, 0, 0, 0 }; const VkAccelerationStructureBuildRangeInfoKHR *tpr = &trange;
   SUBMIT(vkCmdBuildAccelerationStructuresKHR(cb, 1, &tbi, &tpr));

   /* rays: a grid straight down onto the strip, plus oblique rays; each ray set is run under four flag settings */
   enum { NR = 256 };
   struct buf rb = mkbuf(NR * 32, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT), ob = mkbuf(NR * 16, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
   float (*rays)[8] = rb.map; uint32_t seed = 7;
   for (int i = 0; i < NR; i++) {
      seed = seed * 1664525u + 1013904223u; float fx = (seed >> 8 & 0xffff) / 65535.f;
      seed = seed * 1664525u + 1013904223u; float fy = (seed >> 8 & 0xffff) / 65535.f;
      float x = fx * (2.f * ntri + 1.f) - 0.5f, y = fy * 1.4f - 0.2f;
      float side = (i & 1) ? -1.f : 1.f;                         /* from above and from below: both faces */
      float ox = x + (i % 4 == 3 ? 0.3f : 0.f);
      float org[3] = { ox, y, 5.f * side }, dir[3] = { x - ox, 0.f, -5.f * side };
      for (int k = 0; k < 3; k++) { rays[i][k] = org[k]; rays[i][4 + k] = dir[k]; }
      rays[i][3] = 0.f; rays[i][7] = 100.f;
   }
   FILE *sf = fopen(argv[2], "rb"); if (!sf) { fprintf(stderr, "FAIL open %s\n", argv[2]); return 2; }
   fseek(sf, 0, SEEK_END); long sl = ftell(sf); fseek(sf, 0, SEEK_SET); uint32_t *code = malloc((size_t)sl); if (fread(code, 1, (size_t)sl, sf) != (size_t)sl) return 2; fclose(sf);
   VkShaderModule sm; VkShaderModuleCreateInfo smi = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, (size_t)sl, code };
   CK(vkCreateShaderModule(dev, &smi, NULL, &sm));
   VkDescriptorSetLayoutBinding bind[3] = {
      { 0, VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR, 1, VK_SHADER_STAGE_COMPUTE_BIT },
      { 1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT },
      { 2, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT } };
   VkDescriptorSetLayout dsl; VkDescriptorSetLayoutCreateInfo dli = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, 3, bind };
   CK(vkCreateDescriptorSetLayout(dev, &dli, NULL, &dsl));
   VkPushConstantRange pcr = { VK_SHADER_STAGE_COMPUTE_BIT, 0, 12 };
   VkPipelineLayout pl; VkPipelineLayoutCreateInfo pli = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, 1, &dsl, 1, &pcr };
   CK(vkCreatePipelineLayout(dev, &pli, NULL, &pl));
   VkPipeline pipe; VkComputePipelineCreateInfo cpi = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, NULL, 0,
      { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm, "main" }, pl };
   CK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpi, NULL, &pipe));
   VkDescriptorPoolSize ps[3] = { { VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR, 1 }, { VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 2 } };
   VkDescriptorPool dp; VkDescriptorPoolCreateInfo dpi = { VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, NULL, 0, 1, 2, ps };
   CK(vkCreateDescriptorPool(dev, &dpi, NULL, &dp));
   VkDescriptorSet ds; VkDescriptorSetAllocateInfo dsi = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, NULL, dp, 1, &dsl };
   CK(vkAllocateDescriptorSets(dev, &dsi, &ds));
   VkWriteDescriptorSetAccelerationStructureKHR wa = { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET_ACCELERATION_STRUCTURE_KHR, NULL, 1, &tlas };
   VkDescriptorBufferInfo rbi = { rb.b, 0, VK_WHOLE_SIZE }, obi = { ob.b, 0, VK_WHOLE_SIZE };
   VkWriteDescriptorSet w[3] = {
      { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, &wa, ds, 0, 0, 1, VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR },
      { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, ds, 1, 0, 1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, NULL, &rbi },
      { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, ds, 2, 0, 1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, NULL, &obi } };
   vkUpdateDescriptorSets(dev, 3, w, 0, NULL);

   const uint32_t flagsets[4] = { 0x01 /* opaque */, 0x01 | 0x10 /* cull back */, 0x01 | 0x20 /* cull front */, 0x02 /* no-opaque, every candidate confirmed */ };
   int bad = 0, hits = 0;
   for (int fs = 0; fs < 4; fs++) {
      uint32_t pc[3] = { flagsets[fs], 0xff, NR };
      memset(ob.map, 0, NR * 16);
      SUBMIT(vkCmdBindPipeline(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe); vkCmdBindDescriptorSets(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1, &ds, 0, NULL);
             vkCmdPushConstants(cb, pl, VK_SHADER_STAGE_COMPUTE_BIT, 0, 12, pc); vkCmdDispatch(cb, NR / 64, 1, 1));
      uint32_t (*o)[4] = ob.map; int badf = 0;
      for (int i = 0; i < NR; i++) {
         float t = 0; uint32_t prim = 0; int front = 0;
         int rh = ref_hit(rays[i], rays[i] + 4, 0.f, 100.f, flagsets[fs] & 0x30, &t, &prim, &front);
         int gh = o[i][0] != 0xffffffffu; float gt; memcpy(&gt, &o[i][0], 4);
         int ok = rh == gh && (!rh || (fabsf(gt - t) <= 4e-6f * fmaxf(1.f, fabsf(t)) && o[i][1] == prim && (int)((o[i][3] >> 8) & 1) == front));
         hits += rh; badf += !ok;
         if (!ok && badf <= 3) printf("  ray %d: reference %s t=%g prim=%u front=%d, device %s t=%g prim=%u front=%u\n", i, rh ? "hit" : "miss", t, prim, front, gh ? "hit" : "miss", gt, o[i][1], (o[i][3] >> 8) & 1);
      }
      printf("%s flags=0x%02x: %d of %d rays differ\n", badf ? "FAIL" : "ok  ", flagsets[fs], badf, NR);
      bad += badf;
   }
   printf("%s case %s (%d triangles), NVK_HWRT_QUERY=%s, %d reference hits over 4 flag sets\n", bad ? "FAIL" : "PASS", argv[3], ntri,
          getenv("NVK_HWRT_QUERY") ? getenv("NVK_HWRT_QUERY") : "unset", hits);
   return bad != 0;
}
