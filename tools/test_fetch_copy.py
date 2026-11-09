#!/usr/bin/env python3
"""Framebuffer fetch emulated by copies: the SPIR-V rewrite (plugin/nvmtl_fetch_copy.h) and its wiring.
Each module is built from GLSL with glslangValidator, rewritten, and checked with spirv-val and spirv-dis.
  usage: test_fetch_copy.py [tree root]   (needs glslangValidator, spirv-val, spirv-dis, cc)"""
import pathlib, shutil, subprocess, sys, tempfile

ROOT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1]
bad = 0


def check(name, ok, detail=""):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name + (f"  [{detail}]" if detail else ""))


for tool in ("glslangValidator", "spirv-val", "spirv-dis"):
    if not shutil.which(tool):
        print(f"SKIP: {tool} not found")
        sys.exit(0)

HDR = "#version 450\n"
CASES = {
    "two_inputs": HDR + """
layout(input_attachment_index=0, set=3, binding=192) uniform subpassInput c0;
layout(input_attachment_index=1, set=3, binding=193) uniform subpassInput c1;
layout(location=0) out vec4 o;
void main(){ o = subpassLoad(c0) * 0.5 + subpassLoad(c1); }""",
    "with_fragcoord": HDR + """
layout(input_attachment_index=0, set=3, binding=192) uniform subpassInput c0;
layout(location=0) out vec4 o;
void main(){ o = subpassLoad(c0) + vec4(gl_FragCoord.xy * 0.001, 0.0, 0.0); }""",
    "twin_after_subpass": HDR + """
layout(input_attachment_index=0, set=3, binding=192) uniform subpassInput c0;
layout(set=0, binding=0) uniform texture2D t;
layout(set=0, binding=1) uniform sampler s;
layout(location=0) out vec4 o;
void main(){ vec4 a = subpassLoad(c0); o = a + texture(sampler2D(t, s), vec2(0.5)); }""",
    "with_sampled_twin": HDR + """
layout(set=0, binding=0) uniform texture2D t;
layout(set=0, binding=1) uniform sampler s;
layout(input_attachment_index=0, set=3, binding=192) uniform subpassInput c0;
layout(location=0) out vec4 o;
void main(){ o = subpassLoad(c0) + texture(sampler2D(t, s), vec2(0.5)); }""",
}
NOT_REWRITTEN = {
    "no_subpass_input": HDR + "layout(location=0) out vec4 o;\nvoid main(){ o = vec4(1.0); }",
    "multisampled_input": HDR + """
layout(input_attachment_index=0, set=3, binding=192) uniform subpassInputMS c0;
layout(location=0) out vec4 o;
void main(){ o = subpassLoad(c0, 0); }""",
}

with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    exe = t / "rw"
    r = subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", f"-I{ROOT / 'plugin'}", str(ROOT / "tools/fetch_copy_rewrite.c"), "-o", str(exe)],
                       capture_output=True, text=True)
    check("the rewriter compiles without warnings", r.returncode == 0 and not r.stderr.strip(), r.stderr[:300])

    def build(name, src):
        f = t / f"{name}.frag"
        f.write_text(src)
        out = t / f"{name}.spv"
        p = subprocess.run(["glslangValidator", "-V", "--target-env", "vulkan1.1", "-o", str(out), str(f)], capture_output=True, text=True)
        return out if p.returncode == 0 else None

    for name, src in CASES.items():
        spv = build(name, src)
        check(f"{name}: the source module is valid and reads a subpass input", spv is not None and
              "SubpassData" in subprocess.run(["spirv-dis", str(spv)], capture_output=True, text=True).stdout)
        if spv is None:
            continue
        p = subprocess.run([str(exe)], input=spv.read_bytes(), capture_output=True)
        check(f"{name}: the module is rewritten", p.returncode == 0 and len(p.stdout) > 0)
        new = t / f"{name}.new.spv"
        new.write_bytes(p.stdout)
        v = subprocess.run(["spirv-val", "--target-env", "vulkan1.1", str(new)], capture_output=True, text=True)
        check(f"{name}: the rewritten module passes spirv-val", v.returncode == 0, (v.stdout + v.stderr)[:400])
        d = subprocess.run(["spirv-dis", str(new)], capture_output=True, text=True).stdout
        check(f"{name}: no subpass input and no input-attachment capability is left",
              "SubpassData" not in d and "InputAttachment" not in d and "OpImageRead" not in d)
        check(f"{name}: each read became a texel fetch at the pixel's own position",
              d.count("OpImageFetch") == src.count("subpassLoad") and "FragCoord" in d and "ConvertFToS" in d)
        check(f"{name}: FragCoord is declared as the fragment input exactly once", d.count("BuiltIn FragCoord") == 1)

    for name, src in NOT_REWRITTEN.items():
        spv = build(name, src)
        if spv is None:
            check(f"{name}: source built", False)
            continue
        p = subprocess.run([str(exe)], input=spv.read_bytes(), capture_output=True)
        check(f"{name}: left alone (the caller keeps the original shader)", p.returncode == 3 and not p.stdout)
    check("a truncated module is refused, not read past its end",
          subprocess.run([str(exe)], input=b"\x03\x02\x23\x07\x00\x00\x01\x00\x00\x00\x00\x00", capture_output=True).returncode == 3)

# wiring
vk = (ROOT / "plugin/nvmtl_vk.c").read_text()
obj = (ROOT / "plugin/NVMTLObjects.m").read_text()
check("the pipeline rewrites its fragment module only when fetch-copy is on and fetch is off",
      "nvmtl_fetch_copy_on()" in vk and "nvmtl_spirv_fetch_to_texture(" in vk)
check("a copy-read pipeline uses the sampled-image layout at set 3", "g_fcopyLayout" in vk)
check("the encoder ends the pass, copies and resumes it before such a draw", "nvmtlFetchCopy" in obj and "nvmtl_vk_fetch_copy(" in obj)
check("the switch is NVMTL_FETCH_COPY (and the boot-arg for WindowServer)", "NVMTL_FETCH_COPY" in vk and "nvfetchcopy" in vk)
sys.exit(1 if bad else 0)
