use crate::fxhash::HashMap;
use crate::native;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

pub const TOOL_TIMEOUT_SECS: u64 = 60;

pub const TIMEOUT_MARKER: &str = "timed out after";

pub const NO_VERDICT_MARKER: &str = "no verdict";

fn val_gate() -> &'static (Mutex<usize>, Condvar) {
    static GATE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    GATE.get_or_init(|| (Mutex::new(0usize), Condvar::new()))
}

fn val_par_limit() -> usize {
    crate::env_vars::val_par()
}

struct ValPermit;
impl ValPermit {
    fn acquire() -> ValPermit {
        let limit = val_par_limit();
        let (lock, cv) = val_gate();
        let mut n = lock.lock().unwrap();
        while *n >= limit {
            n = cv.wait(n).unwrap();
        }
        *n += 1;
        ValPermit
    }
}
impl Drop for ValPermit {
    fn drop(&mut self) {
        let (lock, cv) = val_gate();
        let mut n = lock.lock().unwrap();
        *n = n.saturating_sub(1);
        cv.notify_one();
    }
}

pub fn run(cmd: &str, args: &[&str]) -> Result<(Vec<u8>, Vec<u8>), String> {
    run_with_timeout(cmd, args, TOOL_TIMEOUT_SECS)
}

pub fn run_with_timeout(
    cmd: &str,
    args: &[&str],
    timeout_secs: u64,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    use std::io::Read;
    use std::process::Stdio;
    use wait_timeout::ChildExt;

    let bin = tool_bin(cmd);
    let mut child = Command::new(&bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run {cmd} ({}) [{NO_VERDICT_MARKER}]: {e}",
                bin.display()
            )
        })?;
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let to = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let te = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    match child.wait_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Some(status)) => {
            let out = to.join().unwrap_or_default();
            let err = te.join().unwrap_or_default();
            if status.success() {
                Ok((out, err))
            } else if status.code().is_none() {
                Err(format!(
                    "{cmd} killed by signal [{NO_VERDICT_MARKER}]:\n{}",
                    String::from_utf8_lossy(&err)
                ))
            } else {
                Err(format!("{cmd} failed:\n{}", String::from_utf8_lossy(&err)))
            }
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = to.join();
            let _ = te.join();
            Err(format!("{cmd} {TIMEOUT_MARKER} {timeout_secs}s -- killed"))
        }
        Err(e) => {
            let _ = child.kill();
            Err(format!("waiting on {cmd} [{NO_VERDICT_MARKER}]: {e}"))
        }
    }
}

fn tool_bin(cmd: &str) -> PathBuf {
    if let Some(path) = crate::env_vars::tool_path_override(cmd) {
        return PathBuf::from(path);
    }
    for dir in [
        "/opt/homebrew/opt/llvm/bin",
        "/usr/local/opt/llvm/bin",
        "/opt/homebrew/bin",
        "/usr/local/bin",
    ] {
        let candidate = Path::new(dir).join(cmd);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(cmd)
}

pub const VULKAN_TARGET_ENV: &str = "vulkan1.2";
pub const VULKAN_TRIPLE: &str = "spirv-unknown-vulkan1.2";

pub fn air_to_sanitized_ll(src: &str, tmp: &Path) -> Result<String, String> {
    Ok(air_to_sanitized_ll_with_datalayout(src, tmp)?.0)
}

pub fn air_to_sanitized_ll_with_datalayout(
    src: &str,
    tmp: &Path,
) -> Result<(String, Option<String>), String> {
    let ll_text = if src.ends_with(".ll") {
        std::fs::read_to_string(src).map_err(|e| format!("read {src}: {e}"))?
    } else {
        let ll = scratch_file(tmp, "k", "ll");
        let text = (|| {
            run("llvm-dis", &[src, "-o", ll.to_str().unwrap()])?;
            std::fs::read_to_string(&ll).map_err(|e| format!("read {}: {e}", ll.display()))
        })();
        let _ = std::fs::remove_file(&ll);
        text?
    };

    Ok(sanitize_ll_text_with_datalayout(&ll_text))
}

pub fn sanitize_ll_text_with_datalayout(ll_text: &str) -> (String, Option<String>) {
    let mut out = String::with_capacity(ll_text.len());
    let mut datalayout = None;
    for line in ll_text.lines() {
        let t = line.trim_start();
        if t.starts_with("target triple") {
            out.push_str(&format!("target triple = \"{VULKAN_TRIPLE}\"\n"));
            continue;
        }
        if t.starts_with("target datalayout") {
            datalayout = datalayout_value(t);
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if t.starts_with("; ModuleID =") {
            continue;
        }
        if t.starts_with("@llvm.global_ctors")
            || t.starts_with("@llvm.global_dtors")
            || t.starts_with("@llvm.used")
            || t.starts_with("@llvm.compiler.used")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, datalayout)
}

fn datalayout_value(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn emit_vulkan_spirv(san_ll: &str, _tmp: &Path) -> Result<Vec<u8>, String> {
    native::emit_vulkan_spirv(san_ll)
}

pub(crate) fn emit_vulkan_spirv_with_sidecar(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
) -> Result<crate::emit_sidecar::EmittedSpirv, String> {
    native::emit_vulkan_spirv_with_sidecar(san_ll, kern, entry_name, buffer_layouts)
}

pub(crate) fn emit_vulkan_spirv_with_outcome(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
) -> Result<crate::emit_sidecar::EmittedSpirv, crate::emit_sidecar::EmissionFailure> {
    native::emit_vulkan_spirv_with_outcome(san_ll, kern, entry_name, buffer_layouts)
}

pub fn emit_vulkan_spirv_all_buffers_raw(san_ll: &str, _tmp: &Path) -> Result<Vec<u8>, String> {
    native::emit_vulkan_spirv_all_buffers_raw(san_ll)
}

pub(crate) fn emit_vulkan_spirv_all_buffers_raw_with_sidecar(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
    known_ordinary_plan_rejections: &crate::fxhash::HashSet<String>,
    known_ownership_plan_rejections: &crate::fxhash::HashSet<String>,
) -> Result<crate::emit_sidecar::EmittedSpirv, String> {
    native::emit_vulkan_spirv_all_buffers_raw_with_sidecar(
        san_ll,
        kern,
        entry_name,
        buffer_layouts,
        known_ordinary_plan_rejections,
        known_ownership_plan_rejections,
    )
}

pub fn emit_vulkan_spirv_all_buffers_raw_with_workgroup(
    san_ll: &str,
    _tmp: &Path,
) -> Result<Vec<u8>, String> {
    native::emit_vulkan_spirv_all_buffers_raw_with_workgroup(san_ll)
}

pub(crate) fn emit_vulkan_spirv_all_buffers_raw_with_workgroup_sidecar(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
    known_ordinary_plan_rejections: &crate::fxhash::HashSet<String>,
    known_ownership_plan_rejections: &crate::fxhash::HashSet<String>,
) -> Result<crate::emit_sidecar::EmittedSpirv, String> {
    native::emit_vulkan_spirv_all_buffers_raw_with_workgroup_sidecar(
        san_ll,
        kern,
        entry_name,
        buffer_layouts,
        known_ordinary_plan_rejections,
        known_ownership_plan_rejections,
    )
}

pub fn emit_vulkan_spirv_all_buffers_raw_relooper_feed(
    san_ll: &str,
    _tmp: &Path,
) -> Result<Vec<u8>, String> {
    native::emit_vulkan_spirv_all_buffers_raw_relooper_feed(san_ll)
}

pub(crate) fn emit_vulkan_spirv_all_buffers_raw_relooper_feed_with_sidecar(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
) -> Result<crate::emit_sidecar::EmittedSpirv, String> {
    native::emit_vulkan_spirv_all_buffers_raw_relooper_feed_with_sidecar(
        san_ll,
        kern,
        entry_name,
        buffer_layouts,
    )
}

pub fn emit_vulkan_spirv_all_buffers_raw_bda(san_ll: &str, _tmp: &Path) -> Result<Vec<u8>, String> {
    native::emit_vulkan_spirv_all_buffers_raw_bda(san_ll)
}

pub(crate) fn emit_vulkan_spirv_all_buffers_raw_bda_with_sidecar(
    san_ll: &str,
    _tmp: &Path,
    kern: Option<&crate::meta::KernMeta>,
    entry_name: Option<&str>,
    buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
    known_ordinary_plan_rejections: &crate::fxhash::HashSet<String>,
    known_ownership_plan_rejections: &crate::fxhash::HashSet<String>,
) -> Result<crate::emit_sidecar::EmittedSpirv, String> {
    native::emit_vulkan_spirv_all_buffers_raw_bda_with_sidecar(
        san_ll,
        kern,
        entry_name,
        buffer_layouts,
        known_ordinary_plan_rejections,
        known_ownership_plan_rejections,
    )
}

pub fn spirv_val(spv_path: &str) -> Result<(), String> {
    let _permit = ValPermit::acquire();
    let args = ["--target-env", VULKAN_TARGET_ENV, spv_path];
    const CAPS_SECS: [u64; 4] = [60, 120, 240, 600];
    for (i, &cap) in CAPS_SECS.iter().enumerate() {
        let last = i + 1 == CAPS_SECS.len();
        match run_with_timeout("spirv-val", &args, cap) {
            Ok(_) => return Ok(()),
            Err(e) if (e.contains(TIMEOUT_MARKER) || e.contains(NO_VERDICT_MARKER)) && !last => {
                continue
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("CAPS_SECS is non-empty")
}

pub fn spirv_val_bytes(spv: &[u8], tmp: &Path) -> Result<(), String> {
    std::fs::create_dir_all(tmp).map_err(|e| format!("spirv_val_bytes create tmp: {e}"))?;
    let path = scratch_file(tmp, "a2v_val", "spv");
    std::fs::write(&path, spv).map_err(|e| format!("spirv_val_bytes write: {e}"))?;
    let result = spirv_val(path.to_str().ok_or("spirv_val_bytes: bad tmp path")?);
    let _ = std::fs::remove_file(&path);
    result
}

fn scratch_file(tmp: &Path, stem: &str, extension: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    tmp.join(format!(
        "{stem}_{}_{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

pub fn strip_ext(p: &str) -> String {
    match p.rfind('.') {
        Some(i) => p[..i].to_string(),
        None => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_validations_sharing_one_scratch_directory_do_not_collide() {
        if Command::new("spirv-val").arg("--version").output().is_err() {
            return;
        }
        let module = crate::translate_sanitized_native(
            r#"
define void @k(ptr addrspace(1) %out) {
entry:
  store i32 7, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#,
            crate::passes::Stage::Kernel,
            &std::env::temp_dir().join(format!("m2v_val_race_build_{}", std::process::id())),
        )
        .expect("the probe kernel translates");

        let shared = std::env::temp_dir().join(format!("m2v_val_race_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&shared);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let module = &module;
                let shared = &shared;
                scope.spawn(move || {
                    for _ in 0..16 {
                        spirv_val_bytes(module, shared)
                            .expect("a valid module validates, whoever else is validating");
                    }
                });
            }
        });
        let _ = std::fs::remove_dir_all(&shared);
    }

    #[test]
    fn sanitizer_captures_and_preserves_target_datalayout() {
        let tmp = std::env::temp_dir();
        let src = tmp.join("m2v_r4_datalayout_test.ll");
        std::fs::write(
            &src,
            "target datalayout = \"e-m:o-i64:64-i128:128-n32:64-S128\"\n\
             target triple = \"air64-apple-macosx\"\n\
             define void @k() {\n  ret void\n}\n",
        )
        .unwrap();
        let (san, datalayout) =
            air_to_sanitized_ll_with_datalayout(src.to_str().unwrap(), &tmp).unwrap();
        assert_eq!(
            datalayout.as_deref(),
            Some("e-m:o-i64:64-i128:128-n32:64-S128")
        );
        assert!(san.contains("target datalayout = \"e-m:o-i64:64-i128:128-n32:64-S128\""));
        assert!(san.contains(&format!("target triple = \"{VULKAN_TRIPLE}\"")));
        assert_eq!(
            air_to_sanitized_ll(src.to_str().unwrap(), &tmp).unwrap(),
            san
        );
        std::fs::remove_file(&src).ok();
    }

    #[test]
    fn text_sanitizer_matches_file_sanitizer_rules() {
        let (san, datalayout) = sanitize_ll_text_with_datalayout(
            "; ModuleID = '/tmp/random/case.air'\n\
             target datalayout = \"e-p:64:64\"\n\
             target triple = \"air64-apple-ios\"\n\
             @llvm.global_ctors = appending global [0 x { i32, ptr, ptr }] []\n\
             @llvm.compiler.used = appending global [0 x ptr] [], section \"llvm.metadata\"\n\
             define void @k() {\n  ret void\n}\n",
        );

        assert_eq!(datalayout.as_deref(), Some("e-p:64:64"));
        assert!(san.contains(&format!("target triple = \"{VULKAN_TRIPLE}\"")));
        assert!(san.contains("target datalayout = \"e-p:64:64\""));
        assert!(!san.contains("ModuleID"));
        assert!(!san.contains("@llvm.global_ctors"));
        assert!(!san.contains("@llvm.compiler.used"));
        assert!(san.contains("define void @k()"));
    }

    #[test]
    fn sanitizer_identity_ignores_llvm_dis_scratch_path() {
        let first = sanitize_ll_text_with_datalayout(
            "; ModuleID = '/tmp/first/case.air'\nsource_filename = \"stable\"\ndefine void @k() { ret void }\n",
        )
        .0;
        let second = sanitize_ll_text_with_datalayout(
            "; ModuleID = '/tmp/second/case.air'\nsource_filename = \"stable\"\ndefine void @k() { ret void }\n",
        )
        .0;
        assert_eq!(first, second);
    }

    #[test]
    fn datalayout_value_extracts_quoted_string() {
        assert_eq!(
            datalayout_value("target datalayout = \"e-p:32:32\""),
            Some("e-p:32:32".to_string())
        );
        assert_eq!(datalayout_value("target datalayout = malformed"), None);
    }
}
