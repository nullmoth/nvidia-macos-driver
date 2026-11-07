#!/usr/bin/env python3
"""DLSS kernels for every RTX card on NVK: an install-time kernel cache behind VK_NVX_binary_import.

Applied to the NVK tree after nvk-macos.patch (and the speed267 NAK patches), like those: anchored, asserted edits.
Usage: patch_dlss_cache.py <mesa tree>

Why: DLSS hands NVK fatbins through vkCreateCuModuleNVX. NVK loads only SASS of the device's exact SM, and Mesa has no
PTX compiler, so on RTX 50 (sm_120) nearly every Super Resolution / Frame Generation module has
nothing to load: they ship sm_89 SASS and PTX only. The DLSS 4 transformer kernels also import __assertfail, which NVK
cannot link, so even their matching SASS fails to parse. The installer compiles each fatbin's PTX for the user's SM with
NVIDIA's own nvJitLink (the user's download) and a trap __assertfail, into /Library/NullMoth/dlss/kernels, named by the
BLAKE3 of the PTX object exactly as it sits in the fatbin (the fatbins DLSS passes at run time are byte-identical to those
in the downloaded files). NVK falls back to that file only when the fatbin itself gives nothing loadable.

The cache is system-wide, so a file is used only if it is a regular file owned by root and not writable by group or
others, opened without following links and checked on the open descriptor."""
import os
import sys

root = sys.argv[1]


def edit(rel, old, new, count=1):
    p = os.path.join(root, rel)
    s = open(p).read()
    if new in s:
        return  # already applied
    n = s.count(old)
    assert n == count, f"{rel}: anchor found {n}x, want {count}: {old[:80]!r}"
    s = s.replace(old, new)
    open(p + ".tmp", "w").write(s)
    os.replace(p + ".tmp", p)


# 1. the raw PTX object of a fatbin (the cache key is computed over these exact bytes)
edit("src/nouveau/cubin/nv_fatbin.h",
     "bool nv_is_sm_compatible(uint32_t device_sm, uint32_t requested_sm, bool backwards_compat);",
     "bool nv_is_sm_compatible(uint32_t device_sm, uint32_t requested_sm, bool backwards_compat);\n\n"
     "/* The PTX object a device of this SM would JIT: the highest PTX target not above it (PTX is forward\n"
     " * compatible), else the lowest one. Returns the object's payload exactly as stored (possibly compressed). */\n"
     "bool nv_fatbin_get_ptx(struct nv_fatbin *fatbin, uint32_t sm, const void **out_data, size_t *out_size);")
edit("src/nouveau/cubin/nv_fatbin.c",
     "bool\nnv_is_sm_compatible(",
     """bool
nv_fatbin_get_ptx(struct nv_fatbin *fatbin, uint32_t sm, const void **out_data, size_t *out_size)
{
   uint32_t best = 0, lowest = UINT32_MAX;
   const void *best_data = NULL, *low_data = NULL;
   size_t best_size = 0, low_size = 0;
   const uint8_t *ptr = fatbin->data;
   const uint8_t *end = fatbin->data + fatbin->size;
   /* Unlike the SASS walk, every header and payload is bounds-checked: this result is hashed and looked up. */
   while ((size_t)(end - ptr) >= sizeof(struct nv_fatbin_object_header_t)) {
      const struct nv_fatbin_object_header_t *object = (const struct nv_fatbin_object_header_t *)ptr;
      if (object->header_size < sizeof(*object) || object->header_size > (size_t)(end - ptr))
         break;
      const uint8_t *payload = ptr + object->header_size;
      if (object->size > (size_t)(end - payload))
         break;
      if (object->kind == NV_FATBIN_OBJECT_KIND_PTX) {
         if (object->sm <= sm && object->sm >= best) {
            best = object->sm; best_data = payload; best_size = object->size;
         }
         if (object->sm < lowest) {
            lowest = object->sm; low_data = payload; low_size = object->size;
         }
      }
      ptr = payload + object->size;
   }
   if (best_data) { *out_data = best_data; *out_size = best_size; return true; }
   if (low_data) { *out_data = low_data; *out_size = low_size; return true; }
   return false;
}

bool
nv_is_sm_compatible(""")

# 2. a module may own the bytes it parsed (a cache file read at create time)
edit("src/nouveau/vulkan/nvk_cubin.h",
     "struct nvk_cubin_module {\n   struct vk_object_base base;\n\n   struct nv_cubin_module *info;\n};",
     "struct nvk_cubin_module {\n   struct vk_object_base base;\n\n   struct nv_cubin_module *info;\n"
     "   /* Bytes read from the DLSS kernel cache; the parsed module points into them, so they live as long as it. */\n"
     "   void *owned_data;\n};")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """   if (module->info) {
      nv_cubin_module_fini(module->info);
      vk_free2(&dev->vk.alloc, pAllocator, module->info);
   }
   vk_object_free(&dev->vk, pAllocator, module);""",
     """   if (module->info) {
      nv_cubin_module_fini(module->info);
      vk_free2(&dev->vk.alloc, pAllocator, module->info);
   }
   free(module->owned_data);
   vk_object_free(&dev->vk, pAllocator, module);""")

# 3. the fallback
edit("src/nouveau/vulkan/nvk_cubin.c",
     '#include "util/u_process.h"',
     '#include "util/u_process.h"\n#include "util/mesa-blake3.h"\n\n#include <fcntl.h>\n#include <sys/stat.h>\n#include <unistd.h>')
edit("src/nouveau/vulkan/nvk_cubin.c",
     "/* nvk_cubin_module */\n",
     r'''/* nvk_cubin_module */

#define NVK_DLSS_KERNEL_DIR "/Library/NullMoth/dlss/kernels"
#define NVK_DLSS_KERNEL_MAX (64u << 20)

/* Reads <dir>/<blake3 of the PTX object>.sm<N>.cubin. NULL when absent or not trustworthy (see the file comment). */
static void *
nvk_dlss_cache_read(const void *ptx, size_t ptx_size, uint32_t sm, size_t *out_size)
{
   blake3_hash h;
   char hex[BLAKE3_HEX_LEN], path[256];
   _mesa_blake3_compute(ptx, ptx_size, h);
   _mesa_blake3_format(hex, h);
   snprintf(path, sizeof(path), NVK_DLSS_KERNEL_DIR "/%s.sm%u.cubin", hex, sm);

   int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
   if (fd < 0)
      return NULL;
   struct stat st;
   if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != 0 || (st.st_mode & (S_IWGRP | S_IWOTH)) ||
       st.st_size <= 0 || (uint64_t)st.st_size > NVK_DLSS_KERNEL_MAX) {
      mesa_logw("NVK: DLSS kernel %s refused (owner, mode or size)", path);
      close(fd);
      return NULL;
   }
   void *buf = malloc((size_t)st.st_size);
   size_t got = 0;
   while (buf && got < (size_t)st.st_size) {
      ssize_t n = read(fd, (uint8_t *)buf + got, (size_t)st.st_size - got);
      if (n <= 0) { free(buf); buf = NULL; break; }
      got += (size_t)n;
   }
   close(fd);
   if (buf) *out_size = got;
   return buf;
}
''')

edit("src/nouveau/vulkan/nvk_cubin.c",
     """      if (!nv_fatbin_get_bytecode(&fatbin, pdev->info.sm,
                                  backwards_compat, &data, &size))
         return vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                         "Fatbin does not have compatible bytecode");
   } else {""",
     """      if (!nv_fatbin_get_bytecode(&fatbin, pdev->info.sm,
                                  backwards_compat, &data, &size)) {
         /* No SASS for this SM (e.g. DLSS on RTX 50, which ships sm_89 SASS + PTX): the install-time cache. */
         const void *ptx; size_t ptx_size;
         if (nv_fatbin_get_ptx(&fatbin, pdev->info.sm, &ptx, &ptx_size))
            module->owned_data = nvk_dlss_cache_read(ptx, ptx_size, pdev->info.sm, &size);
         if (!module->owned_data)
            return vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                             "Fatbin does not have compatible bytecode");
         data = module->owned_data;
      }
   } else {""")

edit("src/nouveau/vulkan/nvk_cubin.c",
     """   if (!nv_cubin_module_init(module->info, data, size))
      return VK_ERROR_INITIALIZATION_FAILED;""",
     """   if (!nv_cubin_module_init(module->info, data, size)) {
      /* SASS that NVK cannot link (the DLSS 4 transformer kernels import __assertfail): the install-time cache holds
       * the same PTX compiled with a trap __assertfail. Only once, and only for a fatbin. */
      const void *ptx; size_t ptx_size;
      struct nv_fatbin fb;
      if (module->owned_data || !nv_fatbin_init(&fb, pCreateInfo->pData, pCreateInfo->dataSize) ||
          !nv_fatbin_get_ptx(&fb, pdev->info.sm, &ptx, &ptx_size))
         return VK_ERROR_INITIALIZATION_FAILED;
      module->owned_data = nvk_dlss_cache_read(ptx, ptx_size, pdev->info.sm, &size);
      if (!module->owned_data)
         return VK_ERROR_INITIALIZATION_FAILED;
      nv_cubin_module_fini(module->info);
      memset(module->info, 0, sizeof(*module->info));
      if (!nv_cubin_module_init(module->info, module->owned_data, size))
         return VK_ERROR_INITIALIZATION_FAILED;
   }""")

# 4. an undefined global function in a cubin is an import, not a kernel (NVIDIA's DLSS 4 transformer SASS imports
#    __assertfail; stock Mesa refused the whole module). Where the install-time cache can replace such a module (any SM
#    a DLSS PTX target reaches) it carries a trap __assertfail; on Turing, whose only code is this SASS, the import stays
#    unbound and an assert that fires faults instead of trapping. Asserts are not expected to fire in shipped DLSS.
edit("src/nouveau/cubin/nv_cubin.c",
     """         if (ELF64_ST_TYPE(symbol->st_info) != STT_FUNC ||
             ELF64_ST_BIND(symbol->st_info) != STB_GLOBAL ||
             ELF64_ST_VISIBILITY(symbol->st_other) != STV_DEFAULT)
            continue;
""",
     """         if (ELF64_ST_TYPE(symbol->st_info) != STT_FUNC ||
             ELF64_ST_BIND(symbol->st_info) != STB_GLOBAL ||
             ELF64_ST_VISIBILITY(symbol->st_other) != STV_DEFAULT)
            continue;

         /* An undefined global function (e.g. __assertfail from device assert()) is an import, not a kernel. */
         if (symbol->st_shndx == SHN_UNDEF)
            continue;
""")

# 5. Blackwell (sm_100 and newer) code reads its launch dimensions and memory windows from new bank-0 offsets, measured
#    on an RTX 5060 with a marker probe: blockDim x/y/z at 0x360/0x364/0x368, gridDim at 0x370/0x374/0x378. NVK wrote
#    only the pre-Blackwell 0x00/0x0C, so every kernel saw a 0-sized block and grid (DLSS's tiled kernels then never
#    finish). The shared/local memory windows (0x2F0/0x2F8) and 0xfffdc0 at 0x37C follow the same layout; the window
#    values are the ones NVK programs into the hardware (nvk_cmd_dispatch.c).
edit("src/nouveau/vulkan/nvk_cubin.c",
     "   root->grid_dim[2] = pLaunchInfo->gridDimZ;\n",
     """   root->grid_dim[2] = pLaunchInfo->gridDimZ;
   if (function->module->info->sm >= 100 && root_size >= 0x380) {
      uint32_t *w = (uint32_t *)root;
      const uint64_t smem_window = 1ull << 32;   /* sm_100+ is newer than Hopper: NVK's 4 GiB-aligned window */
      const uint64_t lmem_window = 0xffull << 24;
      memcpy(&w[0x2f0 / 4], &smem_window, 8);
      memcpy(&w[0x2f8 / 4], &lmem_window, 8);
      w[0x360 / 4] = pLaunchInfo->blockDimX; w[0x364 / 4] = pLaunchInfo->blockDimY; w[0x368 / 4] = pLaunchInfo->blockDimZ;
      w[0x370 / 4] = pLaunchInfo->gridDimX;  w[0x374 / 4] = pLaunchInfo->gridDimY;  w[0x378 / 4] = pLaunchInfo->gridDimZ;
      w[0x37c / 4] = 0xfffdc0;
   }
""")

# 6. expose the extension without the experimental switch (the plugin's DLSS path is its only user)
edit("src/nouveau/vulkan/nvk_physical_device.c",
     """      .NVX_binary_import = info->cls_eng3d >= MAXWELL_A &&
         instance->experimental_flags & NVK_EXPERIMENTAL_DLSS,""",
     """      .NVX_binary_import = info->cls_eng3d >= MAXWELL_A,   /* DLSS through the plugin; kernels: nvk_cubin.c */""")
# 7. 64-bit image handles: NVIDIA-compiled kernels take a combined image+sampler handle as image index | sampler index << 32
#    (measured with a JIT-compiled tex.base probe on an RTX 5060: the 32-bit packing, sampler in bits 20..31, read zeros
#    for any sampler index but 0; the high-word packing read every texel right). Storage and sampled-only handles are
#    the image index either way.
edit("src/nouveau/vulkan/nvk_image_view.c",
     """   /* NVK does not currently support 64-bit texture addressing,
    * so this is the same as vkGetImageViewHandleNVX. */

   return nvk_GetImageViewHandleNVX(_device, pInfo);""",
     """   const uint32_t h = nvk_GetImageViewHandleNVX(_device, pInfo);
   if (pInfo->descriptorType != VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER)
      return h;
   return (uint64_t)(h & 0xfffff) | ((uint64_t)(h >> 20) << 32);""")

# 8. link a cubin the way NVIDIA's driver does before it runs it. Mesa uploaded each kernel's own .text section alone,
#    applied no relocations and bound only constant bank 0. In the RTX 50 kernel cache (259 cubins, nvJitLink output):
#    - 43 hold code relocations, and only three kinds: 56/57 = low/high 32 bits of an absolute address into a MOV
#      immediate (instruction bits 32..63), 75 = a CALL.ABS target (address bits 8..55 at instruction bits 32..79; bits
#      16..23 carry address bits 2..9 and stay 0, so call targets are placed 256-byte aligned). Measured with nvdisasm on
#      cuda_reduce_sum_kernel (MOV R20/R21 = return address, CALL.ABS __cuda_sm20_div_u16, RET.ABS R20). Unrelocated,
#      every such call jumped to GPU address 0: the DLSS 4 transformer layers (72 relocations each) faulted at VA 0x0.
#    - 46 kernels read constant bank 2 (.nv.constant2.<kernel>: the compiler's branch tables; hiluma_engine_input_*
#      does LDC c[0x2] then BRX) and 4 read bank 3 (.nv.constant3, module __constant__ data). Unbound, BRX jumped
#      through garbage.
#    - Native sm_120 SASS carries module-wide banks too: rr2_post_kernel (ray reconstruction) has .nv.constant4, a
#      table of 64-bit pointers (R_CUDA_64) into its .nv.global.init. Every module bank 1, 3..7 is bound, and
#      R_CUDA_32/64 relocations into placed data are applied.
#    A module that needs none of this keeps the old path.
edit("src/nouveau/vulkan/nvk_cubin.h",
     """   void *owned_data;
};
""",
     """   void *owned_data;

   /* A linked module (relocations or constant banks 2/3): its code, globals and banks in one GPU allocation. */
   struct nvkmd_mem *link_mem;
   uint32_t link_count;
   char **link_name;          /* section names placed in link_mem */
   uint64_t *link_addr;
   uint64_t *link_size;
   uint64_t bank_addr[8];     /* module-wide .nv.constant<N> (N = 1, 3..7), bound as bank N */
   uint32_t bank_size[8];
};
""")
edit("src/nouveau/vulkan/nvk_cubin.h",
     """   uint32_t max_warps_per_sm;
};
""",
     """   uint32_t max_warps_per_sm;

   uint64_t cb2_addr;         /* .nv.constant2.<kernel>, bound as bank 2 */
   uint32_t cb2_size;
};
""")
edit("src/nouveau/vulkan/nvk_shader.h",
     """   /* Address of the start of the shader data section */
   uint64_t data_addr;
""",
     """   /* Address of the start of the shader data section */
   uint64_t data_addr;

   /* Imported CUDA kernels (nvk_cubin.c): constant banks beyond the root, by bank index; size 0 = unbound */
   uint64_t cubin_bank_addr[8];
   uint32_t cubin_bank_size[8];
""")
edit("src/nouveau/vulkan/nvk_cmd_dispatch.c",
     """      uint32_t qmd[NAK_MAX_QMD_DWORDS];
      assert(qmd_size_B <= sizeof(qmd));""",
     """      for (uint32_t b = 1; b < ARRAY_SIZE(shader->cubin_bank_size); b++) {
         if (shader->cubin_bank_size[b] == 0)
            continue;
         assert(qmd_info.num_cbufs < ARRAY_SIZE(qmd_info.cbufs));
         assert(shader->cubin_bank_addr[b] % min_cbuf_alignment == 0);
         qmd_info.cbufs[qmd_info.num_cbufs++] = (struct nak_qmd_cbuf) {
            .index = b,
            .addr = shader->cubin_bank_addr[b],
            .size = align(shader->cubin_bank_size[b], min_cbuf_alignment),
         };
      }

      uint32_t qmd[NAK_MAX_QMD_DWORDS];
      assert(qmd_size_B <= sizeof(qmd));""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """#include <fcntl.h>
""",
     """#include "nvkmd/nvkmd.h"
#include "util/u_math.h"

#include <fcntl.h>
#include <inttypes.h>
""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """static VkResult
nvk_cubin_module_init(struct nvk_device *dev, struct nvk_cubin_module *module,""",
     """/* Raw ELF64 reads, every one bounds-checked: the bytes come from the application or the kernel cache. */
struct nvk_link_shdr {
   uint32_t name, type;
   uint64_t flags, addr, offset, size;
   uint32_t link, info;
};

#define NVK_LINK_PLACE_ALIGN 0x100   /* CALL.ABS targets and constant banks */

static VkResult
nvk_cubin_module_link(struct nvk_device *dev, struct nvk_cubin_module *module,
                      const uint8_t *b, size_t n)
{
   uint64_t shoff; uint16_t shentsize, shnum, shstrndx;
   if (n < 64 || memcmp(b, "\\x7f" "ELF", 4) || b[4] != 2)
      return VK_SUCCESS;   /* nv_cubin_module_init accepted it; nothing here to link */
   memcpy(&shoff, b + 0x28, 8); memcpy(&shentsize, b + 0x3a, 2);
   memcpy(&shnum, b + 0x3c, 2); memcpy(&shstrndx, b + 0x3e, 2);
   if (shentsize != 64 || shnum == 0 || shnum > 4096 || shstrndx >= shnum ||
       shoff > n || (uint64_t)shnum * 64 > n - shoff)
      return vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: bad section table");

   struct nvk_link_shdr *sh = calloc(shnum, sizeof(*sh));
   uint64_t *place = calloc(shnum, sizeof(*place));   /* offset in link_mem + 1; 0 = not placed */
   const char **names = calloc(shnum, sizeof(*names));
   VkResult result = VK_SUCCESS;
   if (!sh || !place || !names) { result = VK_ERROR_OUT_OF_HOST_MEMORY; goto out; }

   for (uint32_t i = 0; i < shnum; i++) {
      const uint8_t *h = b + shoff + (uint64_t)i * 64;
      memcpy(&sh[i].name, h + 0, 4); memcpy(&sh[i].type, h + 4, 4); memcpy(&sh[i].flags, h + 8, 8);
      memcpy(&sh[i].addr, h + 16, 8); memcpy(&sh[i].offset, h + 24, 8); memcpy(&sh[i].size, h + 32, 8);
      memcpy(&sh[i].link, h + 40, 4); memcpy(&sh[i].info, h + 44, 4);
      if (sh[i].type != 8 /* NOBITS */ && (sh[i].offset > n || sh[i].size > n - sh[i].offset)) {
         result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: section %u out of bounds", i);
         goto out;
      }
   }
   const struct nvk_link_shdr *st = &sh[shstrndx];
   if (st->type != 3 /* STRTAB */ || st->size == 0) {
      result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: bad section name table");
      goto out;
   }
   for (uint32_t i = 0; i < shnum; i++) {
      if (sh[i].name >= st->size || !memchr(b + st->offset + sh[i].name, 0, st->size - sh[i].name)) {
         result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: bad section name");
         goto out;
      }
      names[i] = (const char *)b + st->offset + sh[i].name;
   }

#define NVK_LINK_IS_TEXT(i) (sh[i].type == 1 && (sh[i].flags & 0x4 /* SHF_EXECINSTR */))
#define NVK_LINK_BANK(i) (!strncmp(names[i], ".nv.constant", 12) && names[i][12] >= '1' && names[i][12] <= '7' && \\
                          names[i][12] != '2' && names[i][13] == 0 ? names[i][12] - '0' : 0)   /* module-wide bank N */
#define NVK_LINK_IS_DATA(i) (!strncmp(names[i], ".nv.global", 10) || !strncmp(names[i], ".nv.constant2.", 14) || \\
                             NVK_LINK_BANK(i))
   bool need = false;
   for (uint32_t i = 0; i < shnum; i++) {
      if (!strncmp(names[i], ".nv.constant2.", 14) || NVK_LINK_BANK(i))
         need = true;
      if ((sh[i].type == 4 /* RELA */ || sh[i].type == 9 /* REL */) && sh[i].size && sh[i].info < shnum) {
         if (NVK_LINK_IS_TEXT(sh[i].info) || NVK_LINK_IS_DATA(sh[i].info))
            need = true;
         else if (strncmp(names[sh[i].info], ".debug", 6)) {
            result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                               "cubin link: relocations into %s are not supported", names[sh[i].info]);
            goto out;
         }
      }
   }
   if (!need)
      goto out;

   uint64_t total = 0;
   uint32_t count = 0;
   for (uint32_t i = 0; i < shnum; i++) {
      if (!NVK_LINK_IS_TEXT(i) && !NVK_LINK_IS_DATA(i))
         continue;
      total = align64(total, NVK_LINK_PLACE_ALIGN);
      place[i] = total + 1;
      total += align64(MAX2(sh[i].size, 1), NVK_LINK_PLACE_ALIGN);   /* a bank's bound range stays inside */
      count++;
   }

   /* The SM prefetches instructions past the end of the code it runs (NVK's own shader heap overallocates 2048 bytes
    * for it). Without this, a small module whose code ended near a page end faulted (FAULT_PTE one page past the
    * module) as soon as a kernel took its CALL.ABS slow path: cuda_dldn_engine_auto_exposure_copy_kernel calling
    * __cuda_sm20_div_rn_f64_full, DLSS at 3x into 4K. */
   const uint64_t stub_off = align64(total, NVK_LINK_PLACE_ALIGN);   /* __assertfail stand-in, see below */
   total = stub_off + NVK_LINK_PLACE_ALIGN;
   total += 0x1000;
   result = nvkmd_dev_alloc_mapped_mem(dev->nvkmd, &dev->vk.base, total, 0x1000,
                                       NVKMD_MEM_LOCAL, NVKMD_MEM_MAP_WR, &module->link_mem);
   if (result != VK_SUCCESS)
      goto out;
   uint8_t *map = module->link_mem->map;
   const uint64_t base = module->link_mem->va->addr;
   memset(map, 0, total);
   for (uint32_t i = 0; i < shnum; i++)
      if (place[i] && sh[i].type != 8)
         memcpy(map + place[i] - 1, b + sh[i].offset, sh[i].size);

   /* The cubins NVIDIA ships as native SASS (sm_75 .. sm_90) import __assertfail and reach it through a pointer in
    * .nv.constant4 (an R_CUDA_64 against the undefined symbol; every internal call is CALL.REL and the one indirect
    * call is CALL.ABS.NOINC through that pointer). CUDA's runtime would supply the function. Here it is a stub of
    * BPT.TRAP then EXIT: the kernel stops with a clean channel fault instead of a call to address 0. The encodings
    * are the same on sm_75, 80, 86, 89 and 90 (nvdisasm -hex of the shipped DLSS kernels: BPT.TRAP 0x1 =
    * 0x000000040000795c / 0x000fea0000300000, EXIT = 0x000000000000794d / 0x000fea0003800000). */
   const bool have_stub = module->info->sm >= 75 && module->info->sm <= 90;
   if (have_stub) {
      static const uint64_t trap_stub[8] = {
         0x000000040000795cull, 0x000fea0000300000ull,   /* BPT.TRAP 0x1 */
         0x000000000000794dull, 0x000fea0003800000ull,   /* EXIT */
         0x000000000000794dull, 0x000fea0003800000ull,   /* EXIT (never reached) */
         0x000000000000794dull, 0x000fea0003800000ull,
      };
      memcpy(map + stub_off, trap_stub, sizeof(trap_stub));
   }

   for (uint32_t r = 0; r < shnum; r++) {
      /* RELA (explicit addend: Hopper and Blackwell cubins) or REL (implicit addend stored at the target: Turing,
       * Ampere and Ada cubins, where the whole DLSS set only has R_CUDA_64 pointers in .nv.constant4) */
      const bool is_rel = sh[r].type == 9;
      if ((sh[r].type != 4 && !is_rel) || !sh[r].size || sh[r].info >= shnum || !place[sh[r].info])
         continue;
      const uint32_t t = sh[r].info, sy = sh[r].link;
      const bool code = NVK_LINK_IS_TEXT(t);
      if (sy >= shnum || sh[sy].type != 2 /* SYMTAB */) {
         result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: relocation without a symbol table");
         goto out;
      }
      const uint64_t esize = is_rel ? 16 : 24;
      for (uint64_t e = 0; e + esize <= sh[r].size; e += esize) {
         uint64_t r_off, r_info; int64_t r_add = 0;
         memcpy(&r_off, b + sh[r].offset + e, 8); memcpy(&r_info, b + sh[r].offset + e + 8, 8);
         if (!is_rel)
            memcpy(&r_add, b + sh[r].offset + e + 16, 8);
         const uint32_t type = (uint32_t)r_info;
         const uint64_t si = r_info >> 32;
         /* overflow-safe: r_off comes from the file; code relocations patch a whole 16-byte instruction */
         const uint64_t span = code ? 16 : (r_info & 0xffffffff) == 1 ? 4 : 8;
         if (si >= sh[sy].size / 24 || (code && r_off % 16) || r_off > sh[t].size || sh[t].size - r_off < span ||
             sh[t].type == 8 /* NOBITS: nothing to patch */) {
            result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: bad relocation in %s", names[t]);
            goto out;
         }
         uint16_t s_shndx; uint64_t s_value;
         uint32_t s_name;
         memcpy(&s_name, b + sh[sy].offset + si * 24, 4);
         memcpy(&s_shndx, b + sh[sy].offset + si * 24 + 6, 2);
         memcpy(&s_value, b + sh[sy].offset + si * 24 + 8, 8);
         if (is_rel && !code) {   /* the addend is what the compiler left in place */
            uint64_t in_place = 0;
            memcpy(&in_place, map + place[t] - 1 + r_off, type == 1 ? 4 : 8);
            r_add = (int64_t)in_place;
         }
         bool undef_ok = false;
         if (s_shndx == 0 /* SHN_UNDEF */ && have_stub && sh[sy].link < shnum && sh[sh[sy].link].type == 3 &&
             s_name < sh[sh[sy].link].size && sh[sh[sy].link].size - s_name >= sizeof("__assertfail") &&
             !strcmp((const char *)b + sh[sh[sy].link].offset + s_name, "__assertfail")) {
            undef_ok = true;
         }
         if (undef_ok) {
            const uint64_t v_stub = base + stub_off + (uint64_t)r_add;
            uint8_t *dst = map + place[t] - 1 + r_off;
            if (code || (type != 1 && type != 2)) {
               result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                                  "cubin link: __assertfail referenced by relocation type %u in %s", type, names[t]);
               goto out;
            }
            if (type == 1) {
               const uint32_t w = (uint32_t)v_stub;
               memcpy(dst, &w, 4);
            } else {
               memcpy(dst, &v_stub, 8);
            }
            continue;
         }
         if (s_shndx >= shnum || !place[s_shndx] || s_value < sh[s_shndx].addr ||
             s_value - sh[s_shndx].addr > sh[s_shndx].size) {
            result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                               "cubin link: %s refers to unplaced section %u", names[t], s_shndx);
            goto out;
         }
         const uint64_t v = base + place[s_shndx] - 1 + (s_value - sh[s_shndx].addr) + (uint64_t)r_add;
         uint8_t *ins = map + place[t] - 1 + r_off;
         if (!code) {                             /* data: R_CUDA_32 / R_CUDA_64, e.g. bank 4's pointers into globals */
            if (type == 1) {
               const uint32_t w = (uint32_t)v;
               memcpy(ins, &w, 4);
            } else if (type == 2) {
               memcpy(ins, &v, 8);
            } else {
               result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                                  "cubin link: data relocation type %u in %s is not supported", type, names[t]);
               goto out;
            }
         } else if (type == 56 || type == 57) {   /* MOV imm32 = low / high half */
            const uint32_t w = type == 56 ? (uint32_t)v : (uint32_t)(v >> 32);
            memcpy(ins + 4, &w, 4);
         } else if (type == 75) {                 /* CALL.ABS: address bits 8..55 at instruction bits 32..79 */
            if ((v & 0xff) || ins[2] != 0 || (v >> 56)) {
               result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                                  "cubin link: CALL.ABS target 0x%" PRIx64 " not encodable in %s", v, names[t]);
               goto out;
            }
            const uint64_t f = v >> 8;
            for (int k = 0; k < 6; k++)
               ins[4 + k] = (uint8_t)(f >> (8 * k));
         } else {
            result = vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED,
                               "cubin link: relocation type %u in %s is not supported", type, names[t]);
            goto out;
         }
      }
   }

   module->link_name = calloc(count, sizeof(char *));
   module->link_addr = calloc(count, sizeof(uint64_t));
   module->link_size = calloc(count, sizeof(uint64_t));
   if (!module->link_name || !module->link_addr || !module->link_size) {
      result = VK_ERROR_OUT_OF_HOST_MEMORY;
      goto out;
   }
   for (uint32_t i = 0; i < shnum; i++) {
      if (!place[i])
         continue;
      const uint32_t k = module->link_count++;
      module->link_name[k] = strdup(names[i]);
      module->link_addr[k] = base + place[i] - 1;
      module->link_size[k] = sh[i].size;
      if (!module->link_name[k]) { result = VK_ERROR_OUT_OF_HOST_MEMORY; goto out; }
      const int bank = NVK_LINK_BANK(i);
      if (bank) {
         module->bank_addr[bank] = module->link_addr[k];
         module->bank_size[bank] = (uint32_t)sh[i].size;
      }
   }
#undef NVK_LINK_IS_TEXT
#undef NVK_LINK_IS_DATA
#undef NVK_LINK_BANK
out:
   free(sh); free(place); free(names);
   return result;   /* on failure the module destroy frees whatever was made */
}

static bool
nvk_cubin_link_find(const struct nvk_cubin_module *module, const char *prefix, const char *fn,
                    uint64_t *addr, uint64_t *size)
{
   const size_t pl = strlen(prefix);
   for (uint32_t k = 0; k < module->link_count; k++) {
      const char *s = module->link_name[k];
      if (!strncmp(s, prefix, pl) && !strcmp(s + pl, fn)) {
         *addr = module->link_addr[k];
         *size = module->link_size[k];
         return true;
      }
   }
   return false;
}

static VkResult
nvk_cubin_module_init(struct nvk_device *dev, struct nvk_cubin_module *module,""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """                          pdev->info.sm, module->info->sm);

   return VK_SUCCESS;
}
""",
     """                          pdev->info.sm, module->info->sm);

   /* after the cache fallback the parsed bytes are owned_data (and size is theirs), not the fatbin's SASS */
   return nvk_cubin_module_link(dev, module, module->owned_data ? module->owned_data : data, size);
}
""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """   free(module->owned_data);
   vk_object_free(&dev->vk, pAllocator, module);""",
     """   free(module->owned_data);
   for (uint32_t k = 0; k < module->link_count; k++)
      free(module->link_name[k]);
   free(module->link_name); free(module->link_addr); free(module->link_size);
   if (module->link_mem)
      nvkmd_mem_unref(module->link_mem);
   vk_object_free(&dev->vk, pAllocator, module);""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """   const int alignment = 0x80; /* maxwell+ */

   VkResult result = nvk_heap_upload(""",
     """   if (module->link_mem) {
      uint64_t addr, size;
      if (!nvk_cubin_link_find(module, ".text.", pCreateInfo->pName, &addr, &size))
         return vk_errorf(dev, VK_ERROR_INITIALIZATION_FAILED, "cubin link: no .text.%s", pCreateInfo->pName);
      function->upload_addr = addr + function->info->func_offset;
      function->upload_size = 0;   /* owned by the module */
      if (nvk_cubin_link_find(module, ".nv.constant2.", pCreateInfo->pName, &addr, &size)) {
         function->cb2_addr = addr;
         function->cb2_size = (uint32_t)size;
      }
      function->max_warps_per_sm = nak_max_warps_per_sm(function->info->gpr_count, nak);
      return VK_SUCCESS;
   }

   const int alignment = 0x80; /* maxwell+ */

   VkResult result = nvk_heap_upload(""")
edit("src/nouveau/vulkan/nvk_cubin.c",
     """   shader.cbuf_map.cbufs[0].type = NVK_CBUF_TYPE_ROOT_DESC;
""",
     """   shader.cbuf_map.cbufs[0].type = NVK_CBUF_TYPE_ROOT_DESC;
   for (int b = 1; b < 8; b++) {
      shader.cubin_bank_addr[b] = b == 2 ? function->cb2_addr : function->module->bank_addr[b];
      shader.cubin_bank_size[b] = b == 2 ? function->cb2_size : function->module->bank_size[b];
   }
""")

# 9. the stack. NVIDIA-compiled kernels start their stack pointer at the stack top the driver puts in bank 0
#    (c[0x37c] on sm_100+, c[0x28] before) and keep every spill there (R1-relative LDL/STL). CUDA's value, 0xfffdc0,
#    lies in the QMD's HIGH local-memory region, which NVK never sizes (NAK writes SHADER_LOCAL_MEMORY_HIGH_SIZE = 0),
#    so spills read back 0. Measured on hiluma_engine_output (MIN_STACK_SIZE 0x28): its output row is reloaded from
#    [R1+0x14] before each store, every pixel landed on row 0, and the DLSS 4 transformer produced an empty image.
#    The top is now the kernel's own declared stack size, inside the LOW region that ensure_slm backs per lane.
edit("src/nouveau/vulkan/nvk_cubin.c",
     """      w[0x37c / 4] = 0xfffdc0;
   }
""",
     """      w[0x37c / 4] = align(function_info->slm_size, 16);   /* stack top: see edit 9 */
   } else if (root_size >= 0x2c) {
      ((uint32_t *)root)[0x28 / 4] = align(function_info->slm_size, 16);
   }
""")

print("patch_dlss_cache: applied")
