#!/usr/bin/env python3
# patch_mesa_no_tls.py <mesa tree> <dir holding nm_tls.h>
# Gives every compiler thread-local variable in the tree a per-thread-key variant, selected by NM_NO_COMPILER_TLS
# (see nm_tls.h). Thirteen lines in eight files: Mesa's API layer and utilities, and three files of our Vulkan
# driver patches. Without the switch the tree compiles exactly as before.
# Anchors are exact strings with asserted counts, read from the pristine copy (.notls-orig) when one exists; a tree
# that does not match is left untouched.
import os, shutil, sys
T, here = sys.argv[1], sys.argv[2]
def tls(decl, type_, name):
    return ("#ifdef NM_NO_COMPILER_TLS\nNM_TLS_KEY(%s);\n#define %s NM_TLS_REF(%s, %s)\n#else\n%s\n#endif\n" % (name, name, type_, name, decl.rstrip("\n")))
# Each patched file includes nm_tls.h by a path relative to itself, so no include directory has to be added and the
# header never reaches the build system's own configure-time test programs.
def inc(rel): return '#include "%snm_tls.h"\n' % rel
E = {
 "src/mesa/glapi/glapi/glapi.h": [(
  "_GLAPI_EXPORT extern __THREAD_INITIAL_EXEC struct _glapi_table * _mesa_glapi_tls_Dispatch;\n_GLAPI_EXPORT extern __THREAD_INITIAL_EXEC void * _mesa_glapi_tls_Context;\n",
  "#ifdef NM_NO_COMPILER_TLS\n/* per-thread slots behind two functions; the two names stay usable as lvalues */\n_GLAPI_EXPORT struct _glapi_table **nm_glapi_dispatch_slot(void);\n_GLAPI_EXPORT void **nm_glapi_context_slot(void);\n#define _mesa_glapi_tls_Dispatch (*nm_glapi_dispatch_slot())\n#define _mesa_glapi_tls_Context (*nm_glapi_context_slot())\n#else\n_GLAPI_EXPORT extern __THREAD_INITIAL_EXEC struct _glapi_table * _mesa_glapi_tls_Dispatch;\n_GLAPI_EXPORT extern __THREAD_INITIAL_EXEC void * _mesa_glapi_tls_Context;\n#endif\n")],
 "src/mesa/glapi/shared-glapi/core.c": [(
  "__THREAD_INITIAL_EXEC struct _glapi_table *_mesa_glapi_tls_Dispatch\n   = (struct _glapi_table *)table_noop_array;\n__THREAD_INITIAL_EXEC void *_mesa_glapi_tls_Context;\n",
  "#ifdef NM_NO_COMPILER_TLS\n#include \"../../../util/nm_tls.h\"\n/* One block per thread holding both slots. A thread's first use finds it zeroed, so the dispatch slot is given\n * its initial value (the no-op table) then, as the initialiser of the compiler's variable does. */\nstruct nm_glapi_slots { struct _glapi_table *dispatch; void *context; int set; };\nNM_TLS_KEY(nm_glapi);\nstatic struct nm_glapi_slots *nm_glapi_thread(void)\n{\n   struct nm_glapi_slots *s = &NM_TLS_REF(struct nm_glapi_slots, nm_glapi);\n   if (!s->set) { s->dispatch = (struct _glapi_table *)table_noop_array; s->set = 1; }\n   return s;\n}\nstruct _glapi_table **nm_glapi_dispatch_slot(void) { return &nm_glapi_thread()->dispatch; }\nvoid **nm_glapi_context_slot(void) { return &nm_glapi_thread()->context; }\n#else\n__THREAD_INITIAL_EXEC struct _glapi_table *_mesa_glapi_tls_Dispatch\n   = (struct _glapi_table *)table_noop_array;\n__THREAD_INITIAL_EXEC void *_mesa_glapi_tls_Context;\n#endif\n")],
 "src/util/u_call_once.c": [(
  "static thread_local struct util_call_once_context_t call_once_context;\n",
  inc("") + tls("static thread_local struct util_call_once_context_t call_once_context;", "struct util_call_once_context_t", "call_once_context"))],
 "src/util/u_debug.c": [(
  "   static thread_local char output[4096];\n   static thread_local char rest[256];\n",
  "#ifdef NM_NO_COMPILER_TLS\n   /* a debugging helper that formats a flag word: two threads formatting at once could interleave their text.\n    * Arrays are kept (the code takes their size), so these are shared rather than per thread at this floor. */\n   static char output[4096];\n   static char rest[256];\n#else\n   static thread_local char output[4096];\n   static thread_local char rest[256];\n#endif\n")],
 "src/util/u_qsort.cpp": [(
  "thread_local int (*tl_qsort_r_compar)(const void *, const void *, void *);\nthread_local void *tl_qsort_r_arg;\n",
  "#ifdef NM_NO_COMPILER_TLS\n" + inc("") + "typedef int (*nm_qsort_compar_t)(const void *, const void *, void *);\nNM_TLS_KEY(tl_qsort_r_compar);\nNM_TLS_KEY(tl_qsort_r_arg);\n#define tl_qsort_r_compar NM_TLS_REF(nm_qsort_compar_t, tl_qsort_r_compar)\n#define tl_qsort_r_arg NM_TLS_REF(void *, tl_qsort_r_arg)\n#else\nthread_local int (*tl_qsort_r_compar)(const void *, const void *, void *);\nthread_local void *tl_qsort_r_arg;\n#endif\n")],
 "src/nouveau/cubin/macos/nv_libelf_mini.c": [(
  "static _Thread_local int nv_elf_err;\n", inc("../../../util/") + tls("static _Thread_local int nv_elf_err;", "int", "nv_elf_err"))],
 "src/nouveau/vulkan/nvk_nir_lower_descriptors.c": [(
  "static _Thread_local struct { bool hit; uint32_t set, binding; } nvk_unbound;\n",
  inc("../../util/") + "#ifdef NM_NO_COMPILER_TLS\nstruct nm_nvk_unbound { bool hit; uint32_t set, binding; };\nNM_TLS_KEY(nvk_unbound);\n#define nvk_unbound NM_TLS_REF(struct nm_nvk_unbound, nvk_unbound)\n#else\nstatic _Thread_local struct { bool hit; uint32_t set, binding; } nvk_unbound;\n#endif\n")],
 "src/nouveau/vulkan/nvkmd/nvrm/nvkmd_nvrm_mem.c": [(
  "static __thread struct nvmtl_nvk_share nvrm_import_staged;\nstatic __thread bool nvrm_import_is_staged;\n",
  inc("../../../../util/") + tls("static __thread struct nvmtl_nvk_share nvrm_import_staged;", "struct nvmtl_nvk_share", "nvrm_import_staged") +
  tls("static __thread bool nvrm_import_is_staged;", "bool", "nvrm_import_is_staged"))],
}
new = {}
for f, edits in E.items():
    p = os.path.join(T, f)
    if not os.path.exists(p): print(f"skipped (not in this tree): {f}"); continue
    s = open(p + ".notls-orig" if os.path.exists(p + ".notls-orig") else p).read()
    for old, rep in edits:
        if s.count(old) != 1: sys.exit(f"STOP: {f}: anchor found {s.count(old)} times, expected 1; nothing was changed")
        s = s.replace(old, rep)
    new[p] = s
for p, s in new.items():
    if not os.path.exists(p + ".notls-orig"): shutil.copy2(p, p + ".notls-orig")
    tmp = p + ".tmp"; open(tmp, "w").write(s); os.replace(tmp, p)
shutil.copy2(os.path.join(here, "nm_tls.h"), os.path.join(T, "src/util/nm_tls.h"))
print(f"patched {len(new)} files; nm_tls.h placed in src/util")
