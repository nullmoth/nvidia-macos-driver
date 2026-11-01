#!/usr/bin/env python3
"""patch_prepass_pick.py <nak/api.rs> - build compute shaders with and without NAK's prepass scheduler, keep the one with
fewer static cycles at no occupancy or spill cost. Applies over patch that added NVMTL_NAK_NO_PREPASS. Atomic write."""
import os, pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
assert "let build = |prepass: bool|" not in s, "already patched"
start = s.index("    let mut s = nak_shader_from_nir(nak, nir, &sm, has_task_shader);\n")
end = s.index("    s.gather_info();\n", start) + len("    s.gather_info();\n")
body = s[start:end]
inner = body.replace("    let mut s = nak_shader_from_nir(nak, nir, &sm, has_task_shader);\n", "")
old_gate = """    // speed267: NVMTL_NAK_NO_PREPASS=1 keeps the NIR order (loads first) instead of the pressure-first prepass
    // schedule, which re-interleaves each load with its consumers (convolve_vertical: 1 tap of loads in flight).
    if std::env::var("NVMTL_NAK_NO_PREPASS").map_or(true, |v| v != "1") {
        pass!(s, opt_instr_sched_prepass);
    }
"""
assert inner.count(old_gate) == 1, "gate anchor"
inner = inner.replace(old_gate, """    if prepass {
        pass!(s, opt_instr_sched_prepass);
    }
""")
inner = "\n".join(("    " + l if l.strip() else l) for l in inner.split("\n"))
new = """    // speed267: NAK's prepass scheduler targets the lowest register count first, and on compute kernels that re-interleaves
    // each load with its consumers: convolve_vertical's 4-tap unrolled block issued 1 tap of loads at a time (NVIDIA's
    // compiler keeps 6 in flight). Without it the NIR order (loads first) stands. Across 183 GB7 kernels skipping it
    // gave fewer static cycles on 131, more on 30, 0 more spills, and lost occupancy on 2 (MPS's blur, 40 -> 48 GPRs),
    // so a compute shader is built both ways and keeps the order without the prepass only when it costs no occupancy
    // and no spills and has fewer static cycles. NVMTL_NAK_PREPASS_PICK=0 = always the prepass (upstream behaviour);
    // NVMTL_NAK_NO_PREPASS=1 = never.
    let build = |prepass: bool| {
        let mut s = nak_shader_from_nir(nak, nir, &sm, has_task_shader);
""" + inner.rstrip() + """
        s
    };
    let never = std::env::var("NVMTL_NAK_NO_PREPASS").map_or(false, |v| v == "1");
    let pick = std::env::var("NVMTL_NAK_PREPASS_PICK").map_or(true, |v| v != "0");
    let s = if never {
        build(false)
    } else {
        let a = build(true);
        if pick && nir.info.stage() == MESA_SHADER_COMPUTE {
            let b = build(false);
            let spills = |x: &ShaderInfo| x.num_spills_to_mem + x.num_fills_from_mem + x.num_spills_to_reg;
            if b.info.max_warps_per_sm >= a.info.max_warps_per_sm
                && spills(&b.info) <= spills(&a.info)
                && b.info.num_static_cycles < a.info.num_static_cycles
            {
                b
            } else {
                a
            }
        } else {
            a
        }
    };
"""
s = s[:start] + new + s[end:]
t = p.with_name(p.name + ".tmp"); t.write_text(s); os.replace(t, p); print("patched", p)
