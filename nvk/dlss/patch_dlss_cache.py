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
print("patch_dlss_cache: applied")
