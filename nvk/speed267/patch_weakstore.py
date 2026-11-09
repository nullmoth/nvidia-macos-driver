#!/usr/bin/env python3
# patch_weakstore.py <from_nir.rs>: plain global stores are weak (STG.E) unless the access is coherent or volatile.
import os, sys
p = sys.argv[1]; s = open(p).read()
a = """            nir_intrinsic_store_global_nv => {
                let data = self.get_src(&srcs[0]);
                let size_B =
                    (srcs[0].bit_size() / 8) * srcs[0].num_components();
                assert!(u32::from(size_B) <= intrin.align());
                let access = MemAccess {
                    mem_type: MemType::from_size(size_B, false),
                    space: MemSpace::Global(MemAddrType::A64),
                    order: MemOrder::Strong(MemScope::GPU),"""
b = """            nir_intrinsic_store_global_nv => {
                let data = self.get_src(&srcs[0]);
                let size_B =
                    (srcs[0].bit_size() / 8) * srcs[0].num_components();
                assert!(u32::from(size_B) <= intrin.align());
                // speed267: a plain store is weak (STG.E), as NVIDIA's own compiler emits it; coherent/volatile stay
                // strong.gpu. Measured 11-01 on GB7's particle with Shared (system-RAM) buffers: strong.gpu stores
                // cost ~70-100 ns per threadgroup (an empty kernel: 317 us vs 5.6 us with the buffer in VRAM).
                // Visibility across dispatches comes from the barriers between them, not from the store's order.
                // NVMTL_NAK_STRONG_STORES=1 restores strong.gpu everywhere.
                static STRONG_STORES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                let strong_stores = *STRONG_STORES.get_or_init(|| {
                    std::env::var_os("NVMTL_NAK_STRONG_STORES").is_some_and(|v| v != "0")
                });
                let store_order = if !strong_stores
                    && (intrin.access() & (ACCESS_COHERENT | ACCESS_VOLATILE)) == 0
                {
                    MemOrder::Weak
                } else {
                    MemOrder::Strong(MemScope::GPU)
                };
                let access = MemAccess {
                    mem_type: MemType::from_size(size_B, false),
                    space: MemSpace::Global(MemAddrType::A64),
                    order: store_order,"""
assert s.count(a) == 1
bak = p + ".pre-weakstore"
if not os.path.exists(bak):
    import shutil; shutil.copy2(p, bak)
s = s.replace(a, b); t = p + ".tmp"; open(t, "w").write(s); os.replace(t, p); print("patched", p)
