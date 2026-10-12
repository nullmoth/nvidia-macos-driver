use std::ffi::OsString;

pub struct EnvVar {
    pub name: &'static str,
    pub default: &'static str,
    pub effect: &'static str,
}

pub const REGISTRY: &[EnvVar] = &[
    EnvVar { name: "METAL2VULKAN_OWNED_DUMP", default: "unset", effect: "path to dump an owned SPIR-V contract failure (debug)" },
    EnvVar { name: "METAL2VULKAN_RQ_DEBUG", default: "off", effect: "trace ray-query call lowering (debug)" },

    EnvVar {
        name: "METAL2VULKAN_VAL_PAR",
        default: "3",
        effect: "max concurrent spirv-val processes (>=1)",
    },
    EnvVar {
        name: "METAL2VULKAN_RELOOPER_MAX_BLOCKS",
        default: "1024",
        effect: "requested relooper cases per dispatch group (hard maximum: 1024)",
    },
    EnvVar {
        name: "METAL2VULKAN_NEST",
        default: "unset",
        effect: "stages the nest relooper may structure: unset = all, 0 = none, or a comma list of kernel,fragment,vertex",
    },
    EnvVar {
        name: "METAL2VULKAN_REPRO_DIR",
        default: "$TMPDIR/metal2vulkan-repros",
        effect: "base directory for FALLBACK repro bundles",
    },
    EnvVar {
        name: "METAL2VULKAN_RETRY_DUMP",
        default: "unset",
        effect: "path to dump a failing retry or corpus-audit SPIR-V module (debug)",
    },
    EnvVar {
        name: "METAL2VULKAN_PHASE_DUMP",
        default: "unset",
        effect: "path prefix for a per-phase SPIR-V dump of the lowering pipeline (debug)",
    },
    EnvVar {
        name: "METAL2VULKAN_<TOOL>",
        default: "PATH search",
        effect: "absolute per-tool override (for example METAL2VULKAN_LLVM_DIS or \
                 METAL2VULKAN_SPIRV_VAL)",
    },
    EnvVar {
        name: "METAL2VULKAN_DBG_RAWBYTE",
        default: "off",
        effect: "trace raw-byte GEP indexing",
    },
    EnvVar {
        name: "METAL2VULKAN_PASS_CONTRACT",
        default: "off",
        effect: "after every lowering phase, report whether the module satisfies the owned \
                 construction contract, so the phase that first breaks it names itself",
    },
    EnvVar {
        name: "METAL2VULKAN_RETRY_DEBUG",
        default: "off",
        effect: "trace each retry tier's emit/validate to stderr",
    },
    EnvVar {
        name: "METAL2VULKAN_TIER_CENSUS",
        default: "off",
        effect: "print the adopted retry tier for each translation",
    },
    EnvVar {
        name: "METAL2VULKAN_PARAM_POINTEE_DBG",
        default: "off",
        effect: "trace parameter-pointee sidecar mismatches",
    },
    EnvVar {
        name: "METAL2VULKAN_POINTEE_DBG",
        default: "off",
        effect: "trace typed-IR pointee-carrier mismatches",
    },
    EnvVar {
        name: "METAL2VULKAN_WHOLE_PART",
        default: "off",
        effect:
            "UNSAFE diagnostic: probe whole-composite local-pointer carriers; may emit invalid \
                 SPIR-V and must not be used as a product feature",
    },
    EnvVar {
        name: "METAL2VULKAN_REINTERP_REAL",
        default: "off",
        effect: "UNSAFE diagnostic: probe same-width float/integer pointer retyping; known \
                 nonconformant and never a product feature",
    },
    EnvVar {
        name: "METAL2VULKAN_STRADDLE_ADMIT",
        default: "off",
        effect:
            "UNSAFE diagnostic: bypass one structured-CFG straddle check; may emit invalid SPIR-V",
    },
    EnvVar {
        name: "METAL2VULKAN_PTR_NETWORK_WHY",
        default: "off",
        effect: "trace pointer phi/select networks with non-uniform pointee types (byte-neutral)",
    },
    EnvVar {
        name: "METAL2VULKAN_STORAGE_DBG",
        default: "off",
        effect: "trace storage-class inference",
    },
    EnvVar {
        name: "METAL2VULKAN_TEX_DBG",
        default: "off",
        effect: "trace texture lowering",
    },
    EnvVar {
        name: "METAL2VULKAN_TIR_DBG",
        default: "off",
        effect: "trace typed-IR construction",
    },
    EnvVar {
        name: "METAL2VULKAN_TIR_ONLY",
        default: "off",
        effect: "panic if an op falls back to the string parser (migration gate)",
    },
    EnvVar {
        name: "METAL2VULKAN_WHY",
        default: "off",
        effect: "print the structured-CFG admit/reject reason",
    },
    EnvVar {
        name: "METAL2VULKAN_RELOOP_WHY",
        default: "off",
        effect: "trace relooper entry, block counts, and exact bailout reasons (byte-neutral)",
    },
    EnvVar {
        name: "METAL2VULKAN_SPI_WHY",
        default: "off",
        effect: "trace exact structured-plan rejection points and plan flags (byte-neutral)",
    },
    EnvVar {
        name: "METAL2VULKAN_CONVERGE_INLOOP",
        default: "off",
        effect:
            "measurement override: force in-loop merge convergence on every structured-plan attempt",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_LATCH_TRAMPOLINE_FOLD",
        default: "off",
        effect: "measurement override: keep each do-while latch's private selection trampoline \
                 (skip the b57 fold that branches it straight to the loop merge)",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_WIDEN_SHL",
        default: "off",
        effect: "measurement override: keep each zero-extended masked 16-bit shift as emitted \
                 (skip the b57 rewrite of u32(iand16(ishl16(x, c), m)) to ishl32(u32(iand16(x, m >> c)), c))",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_WIDE_HALF_LOAD",
        default: "off",
        effect: "measurement override: emit a half vector's device words without the AIR's 8-byte alignment \
                 (skip the b57 hint the driver fuses into one v2uint load)",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_BYTE_WORDS",
        default: "off",
        effect: "measurement override: keep each all-byte Function variable byte-typed \
                 (skip the b78 retype to 32-bit words that keeps Mesa from one u8 phi per byte)",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_LOAD_HOIST",
        default: "off",
        effect: "measurement override: keep each device load where the emitter put it \
                 (skip the b57 hoist above its block's threadgroup stores)",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_POINTER_REMAT",
        default: "off",
        effect: "measurement override: leave a non-dominating use of a pure access chain / copy \
                 as it is (skip the b68 repair; the function then goes to construction)",
    },
    EnvVar {
        name: "METAL2VULKAN_NO_LEAF_MEMCPY",
        default: "off",
        effect: "measurement override: leave a memcpy between two differently typed aggregates \
                 as an owned llvm.memcpy call (skip the b70 leaf-by-leaf copy; the pipeline is refused)",
    },
    EnvVar {
        name: "METAL2VULKAN_UNMODELED_WHY",
        default: "off",
        effect: "trace synthesized unmodeled-pointer placeholders and their provenance",
    },
    EnvVar {
        name: "METAL2VULKAN_FLM_WHY",
        default: "off",
        effect: "trace loop-merge plans, selection collisions, and merge phis",
    },
    EnvVar {
        name: "METAL2VULKAN_EXIT_WHY",
        default: "off",
        effect: "trace illegal structured-exit edges and their innermost constructs",
    },
    EnvVar {
        name: "METAL2VULKAN_SWITCH_TAIL_WHY",
        default: "off",
        effect: "trace switch-case shared-continuation cloning candidates",
    },
];

pub fn help_text() -> String {
    let mut out = String::from(
        "environment variables (debug toggles are enabled by presence, regardless of value):\n",
    );
    for v in REGISTRY {
        out.push_str(&format!(
            "  {:<32} {} [default: {}]\n",
            v.name, v.effect, v.default
        ));
    }
    out
}

fn present(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

pub fn val_par() -> usize {
    std::env::var("METAL2VULKAN_VAL_PAR")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(3)
}

pub fn relooper_max_blocks(fallback: usize) -> usize {
    std::env::var("METAL2VULKAN_RELOOPER_MAX_BLOCKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

pub fn repro_dir() -> Option<OsString> {
    std::env::var_os("METAL2VULKAN_REPRO_DIR")
}

pub fn retry_dump() -> Option<OsString> {
    std::env::var_os("METAL2VULKAN_RETRY_DUMP")
}

pub fn phase_dump() -> Option<OsString> {
    std::env::var_os("METAL2VULKAN_PHASE_DUMP")
}

pub fn tool_path_override(cmd: &str) -> Option<OsString> {
    std::env::var_os(format!(
        "METAL2VULKAN_{}",
        cmd.replace('-', "_").to_ascii_uppercase()
    ))
}

pub fn dbg_rawbyte() -> bool {
    present("METAL2VULKAN_DBG_RAWBYTE")
}
pub fn pass_contract() -> bool {
    present("METAL2VULKAN_PASS_CONTRACT")
}
pub fn retry_debug() -> bool {
    present("METAL2VULKAN_RETRY_DEBUG")
}
pub fn tier_census() -> bool {
    present("METAL2VULKAN_TIER_CENSUS")
}
pub fn param_pointee_dbg() -> bool {
    present("METAL2VULKAN_PARAM_POINTEE_DBG")
}
pub fn pointee_dbg() -> bool {
    present("METAL2VULKAN_POINTEE_DBG")
}
pub fn whole_part() -> bool {
    present("METAL2VULKAN_WHOLE_PART")
}
pub fn reinterp_real() -> bool {
    present("METAL2VULKAN_REINTERP_REAL")
}
pub fn straddle_admit() -> bool {
    present("METAL2VULKAN_STRADDLE_ADMIT")
}
pub fn ptr_network_why() -> bool {
    present("METAL2VULKAN_PTR_NETWORK_WHY")
}
pub fn unmodeled_why() -> bool {
    present("METAL2VULKAN_UNMODELED_WHY")
}
pub fn exit_why() -> bool {
    present("METAL2VULKAN_EXIT_WHY")
}
pub fn switch_tail_why() -> bool {
    present("METAL2VULKAN_SWITCH_TAIL_WHY")
}
pub fn flm_why() -> bool {
    present("METAL2VULKAN_FLM_WHY")
}
pub fn reloop_why() -> bool {
    present("METAL2VULKAN_RELOOP_WHY")
}
pub fn converge_inloop() -> bool {
    present("METAL2VULKAN_CONVERGE_INLOOP")
}
pub fn no_latch_trampoline_fold() -> bool {
    present("METAL2VULKAN_NO_LATCH_TRAMPOLINE_FOLD")
}
pub fn no_widen_shl() -> bool {
    present("METAL2VULKAN_NO_WIDEN_SHL")
}
pub fn no_wide_half_load() -> bool {
    present("METAL2VULKAN_NO_WIDE_HALF_LOAD")
}
pub fn no_byte_words() -> bool {
    present("METAL2VULKAN_NO_BYTE_WORDS")
}
pub fn no_load_hoist() -> bool {
    present("METAL2VULKAN_NO_LOAD_HOIST")
}
pub fn no_pointer_remat() -> bool {
    present("METAL2VULKAN_NO_POINTER_REMAT")
}
pub fn no_leaf_memcpy() -> bool {
    present("METAL2VULKAN_NO_LEAF_MEMCPY")
}
pub fn spi_why() -> bool {
    present("METAL2VULKAN_SPI_WHY")
}
pub fn storage_dbg() -> bool {
    present("METAL2VULKAN_STORAGE_DBG")
}
pub fn tex_dbg() -> bool {
    present("METAL2VULKAN_TEX_DBG")
}
pub fn tir_dbg() -> bool {
    present("METAL2VULKAN_TIR_DBG")
}
pub fn tir_only() -> bool {
    present("METAL2VULKAN_TIR_ONLY")
}
pub fn why() -> bool {
    present("METAL2VULKAN_WHY")
}

pub fn owned_dump() -> Option<OsString> {
    std::env::var_os("METAL2VULKAN_OWNED_DUMP")
}
pub fn rq_debug() -> bool {
    present("METAL2VULKAN_RQ_DEBUG")
}
