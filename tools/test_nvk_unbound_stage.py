#!/usr/bin/env python3
"""A shader that reads a binding its pipeline layout leaves empty must fail pipeline creation, whichever stage it is.

NVK notes such a read in a per-thread record while it lowers a stage's descriptors, and each lowering clears the record
first. Mesa lowers every stage of a pipeline before it compiles any (nvk_compile_shaders), while our check sat in
nvk_compile_shader: by the time it ran, only the last stage's record was left. A vertex shader's read was forgotten as
soon as the fragment shader was lowered, and the pipeline was built. nvk/nvk-unbound-stage.patch takes the record after
each stage. The flow is modelled here in C with the record's real rules (clear at the start of a lowering, first note
wins, take clears); the old order must build the bad pipeline and the new one must refuse it."""
import pathlib, shutil, subprocess, sys, tempfile

R = pathlib.Path(__file__).resolve().parents[1]
patch = (R / "nvk/nvk-unbound-stage.patch").read_text()
bld = (R / "build/build_nvk.sh").read_text()
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail.strip()[-200:]})" if detail and not ok else ""))
    fails += not ok


MODEL = r'''
#include <stdbool.h>
#include <stdio.h>
static struct { bool hit; unsigned set, binding; } rec;                       /* nvk_unbound */
static void note(unsigned s, unsigned b) { if (!rec.hit) { rec.hit = true; rec.set = s; rec.binding = b; } }
static bool take(unsigned *s, unsigned *b) { bool h = rec.hit; if (h) { *s = rec.set; *b = rec.binding; } rec.hit = false; return h; }
static void lower(bool reads_empty, unsigned binding) { rec.hit = false; if (reads_empty) note(0, binding); }   /* nvk_nir_lower_descriptors */
/* old: lower every stage, then the check runs once per stage at compile time (the first take sees what is left) */
static int old_flow(const bool *bad, int n) {
    for (int i = 0; i < n; i++) lower(bad[i], 10 + i);
    for (int i = 0; i < n; i++) { unsigned s, b; if (take(&s, &b)) return 1; }
    return 0;
}
/* new: take the record after each stage is lowered; -1 none, else the first stage that read an empty binding */
static int new_flow(const bool *bad, int n) {
    int first = -1;
    for (int i = 0; i < n; i++) { lower(bad[i], 10 + i); unsigned s, b; if (take(&s, &b) && first < 0) first = i; }
    return first;
}
int main(void) {
    const bool cases[4][2] = { { false, false }, { true, false }, { false, true }, { true, true } };
    for (int c = 0; c < 4; c++) printf("%d %d %d %d\n", cases[c][0], cases[c][1], old_flow(cases[c], 2), new_flow(cases[c], 2));
    return 0;
}
'''
cc = shutil.which("cc") or shutil.which("clang")
if not cc:
    print("SKIP: needs a C compiler"); sys.exit(2)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d / "m.c").write_text(MODEL)
    subprocess.run([cc, "-Wall", "-Werror", "-o", str(d / "m"), str(d / "m.c")], check=True)
    rows = {(int(a), int(b)): (int(o), int(n)) for a, b, o, n in (l.split() for l in subprocess.run([str(d / "m")], capture_output=True, text=True).stdout.splitlines())}
arm("a clean vertex and fragment shader build either way", rows[(0, 0)] == (0, -1), str(rows[(0, 0)]))
arm("control: the old order builds a pipeline whose vertex shader reads an empty binding", rows[(1, 0)][0] == 0, str(rows[(1, 0)]))
arm("taking the record after each stage refuses it and names the vertex stage", rows[(1, 0)][1] == 0, str(rows[(1, 0)]))
arm("a fragment shader's read is refused by both orders", rows[(0, 1)] == (1, 1), str(rows[(0, 1)]))
arm("with both stages bad the first one is the one named", rows[(1, 1)][1] == 0, str(rows[(1, 1)]))

added = "\n".join(l[1:] for l in patch.splitlines() if l.startswith("+") and not l.startswith("+++"))
arm("the patch takes the record inside the lowering loop, before the cross-stage pass",
    "nvk_nir_lower_descriptors_take_unbound(&ub_set, &ub_binding) && !unbound_hit" in added
    and patch.index("take_unbound(&ub_set") < patch.index("nir_opt_varyings_bulk"))
arm("on a hit it frees every stage's NIR, clears the outputs and returns an error naming the stage",
    "for (uint32_t j = 0; j < shader_count; j++)\n         ralloc_free(infos[j].nir);" in added
    and "memset(shaders_out, 0" in added and "_mesa_shader_stage_to_string(unbound_stage)" in added and "VK_ERROR_UNKNOWN" in added)
chain = next((l for l in bld.splitlines() if l.strip().startswith('CHAIN="nvk-macos') and "nvk-card-select" in l), "")
arm("the patch comes after every patch that changes the shader compile path",
    "nvk-warnings nvk-unbound-stage" in chain, chain[-80:])
arm("the 2.8 chain is unchanged", "nvk-unbound-stage" not in next(l for l in bld.splitlines() if 'CHAIN="nvk-macos @FIXES nvk-620-kernel nvk-recycle nvk-slab nvk-slabdef" ;;' in l))
print("FAILED" if fails else "PASS"); sys.exit(1 if fails else 0)
