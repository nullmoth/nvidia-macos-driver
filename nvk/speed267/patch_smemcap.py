#!/usr/bin/env python3
"""patch_smemcap.py <qmd.rs> - NAK_SMEM_TARGET_CAP_KB=<n>: never pick a TARGET shared-memory carve-out above n KB
(still >= the one-workgroup minimum). Measured 11-01 on the RTX 5060: a 64-thread kernel with 4 KB of threadgroup
memory jumps from the 64 KB to the 100 KB carve-out (24 groups x 4 KB > 64 KB), which leaves 28 KB of L1, and ran
1.65x slower (503 vs 306 us) than with 2 KB, where the 64 KB carve-out keeps 64 KB of L1. Exact anchors."""
import os, pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
A = """    let min = gv100_pick_smem_size_kb(smem_size, smem_sizes, 1);
    let target =
        gv100_pick_smem_size_kb(smem_size, smem_sizes, workgroups_per_sm);
"""
assert s.count(A) == 1, "anchor A"
s = s.replace(A, """    let min = gv100_pick_smem_size_kb(smem_size, smem_sizes, 1);
    let mut target =
        gv100_pick_smem_size_kb(smem_size, smem_sizes, workgroups_per_sm);
    // speed267: an optional cap on the target carve-out, so a kernel that reuses data through L1 keeps it
    if let Some(cap) = nak_smem_target_cap_kb() {
        if target > cap {
            target = smem_sizes
                .iter()
                .copied()
                .filter(|&v| v >= min && v <= cap)
                .max()
                .unwrap_or(min.max(target.min(cap)));
        }
    }
""")
B = """/// Returns the (MIN_, TARGET_, MAX_) SM_CONFIG_SHARED_MEM_SIZE values."""
assert s.count(B) == 1, "anchor B"
s = s.replace(B, """fn nak_smem_target_cap_kb() -> Option<u16> {
    static CAP: std::sync::OnceLock<Option<u16>> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("NAK_SMEM_TARGET_CAP_KB")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&v| v > 0)
    })
}

""" + B)
t = p.with_name(p.name + ".tmp"); t.write_text(s); os.replace(t, p); print("patched", p)
