use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::reflect::{KernelDispatch, ShaderReflection};
use metal2vulkan::{
    detect_stage, tools, translate_passthrough, translate_reflected_with_options,
    translate_with_options,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs, process};

struct ReproContext {
    original_args: Vec<String>,
    src: String,
    out: String,
    requested_stage: String,
    effective_stage: Option<String>,
    translation_options: Vec<String>,
}

impl ReproContext {
    fn stage_for_repro(&self) -> &str {
        self.effective_stage
            .as_deref()
            .unwrap_or(&self.requested_stage)
    }
}

fn main() {
    let original_args: Vec<String> = env::args().collect();
    let args: Vec<String> = original_args.iter().skip(1).cloned().collect();
    let mut stage = "auto".to_string();
    let mut emit_meta: Option<String> = None;
    let mut raster_sample_count = None;
    let mut vertex_amplification_count = 1;
    let mut kernel_local_size = [64, 1, 1];
    let mut kernel_dispatch = None;
    let mut translation_options = Vec::new();
    let mut pos: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    r#"usage: metal2vulkan <in.air|.ll> [out.spv] [options]

If out.spv is omitted, the default is <input-without-extension>.vk.spv.

options:
  --stage auto|vertex|fragment|passthrough|kernel
      shader stage (default: auto from AIR metadata; compute aliases kernel)
  --emit-meta out.json
      write ShaderReflection JSON (requires the serde feature; not passthrough)
  --raster-samples 1|2|4|8|16|32|64
      exact graphics-pipeline sample count for AIR sample-count queries
  --vertex-amplification-count N
      vertex amplification count the render pass encodes (default: 1, Metal's own default)
  --local X,Y,Z
      nominal Metal kernel threadgroup size (default: 64,1,1)
  --threads-per-grid X,Y,Z
      reflect one fixed Metal dispatchThreads grid for exact boundary decomposition
  --threads-per-grid-push-constant OFFSET
      place the dynamic 48-byte dispatch-region payload at OFFSET
  --whole-workgroups
      assert every dispatch covers complete workgroups and use one fixed-local-size pipeline
  -h, --help
      show this help
"#
                );
                print!("{}", metal2vulkan::env_vars::help_text());
                process::exit(0);
            }
            "--stage" => {
                if let Some(v) = args.get(i + 1) {
                    stage = v.clone();
                }
                i += 2;
            }
            "--emit-meta" => {
                emit_meta = args.get(i + 1).cloned();
                i += 2;
            }
            "--raster-samples" => {
                raster_sample_count = args.get(i + 1).and_then(|value| value.parse().ok());
                if !matches!(raster_sample_count, Some(1 | 2 | 4 | 8 | 16 | 32 | 64)) {
                    fail("--raster-samples requires one of 1, 2, 4, 8, 16, 32, or 64");
                }
                translation_options.extend(["--raster-samples".to_string(), args[i + 1].clone()]);
                i += 2;
            }
            "--vertex-amplification-count" => {
                match args.get(i + 1).and_then(|value| value.parse::<u32>().ok()) {
                    Some(count) if count >= 1 => vertex_amplification_count = count,
                    _ => fail("--vertex-amplification-count requires a count of 1 or more"),
                }
                translation_options.extend([
                    "--vertex-amplification-count".to_string(),
                    args[i + 1].clone(),
                ]);
                i += 2;
            }
            "--local" => {
                kernel_local_size = args
                    .get(i + 1)
                    .and_then(|value| parse_u32x3(value))
                    .filter(|size| !size.contains(&0))
                    .unwrap_or_else(|| fail("--local requires three non-zero dimensions X,Y,Z"));
                translation_options.extend(["--local".to_string(), args[i + 1].clone()]);
                i += 2;
            }
            "--threads-per-grid" => {
                if kernel_dispatch.is_some() {
                    fail("kernel dispatch grid options are mutually exclusive");
                }
                let threads_per_grid = args
                    .get(i + 1)
                    .and_then(|value| parse_u32x3(value))
                    .unwrap_or_else(|| fail("--threads-per-grid requires three dimensions X,Y,Z"));
                kernel_dispatch = Some(KernelDispatch::ThreadsFixed { threads_per_grid });
                translation_options.extend(["--threads-per-grid".to_string(), args[i + 1].clone()]);
                i += 2;
            }
            "--threads-per-grid-push-constant" => {
                if kernel_dispatch.is_some() {
                    fail("kernel dispatch grid options are mutually exclusive");
                }
                let offset = args
                    .get(i + 1)
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or_else(|| {
                        fail("--threads-per-grid-push-constant requires a byte offset")
                    });
                let dispatch = KernelDispatch::ThreadsDynamic { offset };
                if let Err(error) = dispatch.validate() {
                    fail(&error);
                }
                kernel_dispatch = Some(dispatch);
                translation_options.extend([
                    "--threads-per-grid-push-constant".to_string(),
                    args[i + 1].clone(),
                ]);
                i += 2;
            }
            "--whole-workgroups" => {
                if kernel_dispatch.is_some() {
                    fail("kernel dispatch grid options are mutually exclusive");
                }
                kernel_dispatch = Some(KernelDispatch::Workgroups);
                translation_options.push("--whole-workgroups".to_string());
                i += 1;
            }
            "--entry" => {
                i += 2;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                fail(&format!(
                    concat!(
                        "unrecognized option `{}`; the input and output are positional ",
                        "(`metal2vulkan <in.air|.ll> <out.spv>`), and --help lists every option"
                    ),
                    other
                ));
            }
            _ => {
                pos.push(args[i].clone());
                i += 1;
            }
        }
    }
    if pos.is_empty() {
        eprintln!(
            "usage: metal2vulkan <in.air|.ll> <out.spv> [--stage auto|vertex|fragment|passthrough|kernel]  (default: auto)"
        );
        fail("no input");
    }
    let src = pos[0].clone();
    let out = pos
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{}.vk.spv", tools::strip_ext(&src)));
    let mut repro = ReproContext {
        original_args,
        src: src.clone(),
        out: out.clone(),
        requested_stage: stage.clone(),
        effective_stage: None,
        translation_options,
    };

    let tmp = env::temp_dir().join(format!("metal2vulkan_{}", process::id()));
    let _ = fs::create_dir_all(&tmp);

    let mut options = TransformOptions {
        kernel_local_size,
        kernel_dispatch,
        raster_sample_count,
        vertex_amplification_count,
        ..TransformOptions::default()
    };
    // METAL2VULKAN_TILE=<bits of metal2vulkan::tile_mode>: translate as a tile-shading driver does
    if let Some(bits) = metal2vulkan::env_vars::tile_flags() {
        use metal2vulkan::tile_mode;
        tile_mode::set_flags(bits);
        // METAL2VULKAN_TILE_TG_OFFSETS=<byte offset of threadgroup(0)>,<of threadgroup(1)>,...
        if let Some(offsets) = metal2vulkan::env_vars::tile_tg_offsets() {
            tile_mode::set_tg_offsets(&offsets);
        }
        if tile_mode::has(tile_mode::BAND) && fs::read_to_string(&src).is_ok_and(|text| tile_mode::mentions_imageblock(&text)) {
            options.descriptor_layout = tile_mode::band_layout(options.descriptor_layout);
        }
    }
    let want_meta = emit_meta.is_some();
    let translated = match stage.as_str() {
        "passthrough" => translate_passthrough(&src, &tmp).map(|spv| (spv, None)),
        "auto" => match detect_stage(&src, &tmp) {
            Ok(st) => {
                let name = match st {
                    Stage::Vertex => "vertex",
                    Stage::Fragment => "fragment",
                    Stage::Kernel => "kernel",
                };
                repro.effective_stage = Some(name.to_string());
                eprintln!("metal2vulkan: auto-detected stage {name}");
                translate_maybe_reflect(&src, st, &tmp, want_meta, options)
            }
            Err(e) => {
                eprintln!("metal2vulkan: {e}");
                fail_with_repro("stage auto-detect failed", &repro, Some(&e), &tmp);
            }
        },
        "vertex" => {
            repro.effective_stage = Some("vertex".to_string());
            translate_maybe_reflect(&src, Stage::Vertex, &tmp, want_meta, options)
        }
        "fragment" => {
            repro.effective_stage = Some("fragment".to_string());
            translate_maybe_reflect(&src, Stage::Fragment, &tmp, want_meta, options)
        }
        "kernel" | "compute" => {
            repro.effective_stage = Some("kernel".to_string());
            translate_maybe_reflect(&src, Stage::Kernel, &tmp, want_meta, options)
        }
        other => {
            eprintln!(
                "metal2vulkan: unsupported --stage {other} (only vertex|fragment|passthrough|kernel|compute)"
            );
            fail_with_repro("bad stage", &repro, None, &tmp);
        }
    };
    let (spv, reflection) = match translated {
        Ok(v) => v,
        Err(e) => {
            eprintln!("metal2vulkan: {e}");
            fail_with_repro("translate failed", &repro, Some(&e), &tmp);
        }
    };
    if let Err(e) = fs::write(&out, &spv) {
        eprintln!("metal2vulkan: cannot write {out}: {e}");
        fail_with_repro("write failed", &repro, Some(&e.to_string()), &tmp);
    }

    println!("metal2vulkan: wrote {out}; spirv-val vulkan1.2: PASS");

    if let Some(meta_path) = &emit_meta {
        match reflection.as_ref() {
            Some(r) => match write_meta(meta_path, r) {
                Ok(()) => println!("metal2vulkan: wrote reflection {meta_path}"),
                Err(e) => {
                    eprintln!("metal2vulkan: {e}");
                    fail_with_repro("emit-meta failed", &repro, Some(&e), &tmp);
                }
            },
            None => {
                eprintln!("metal2vulkan: --emit-meta is not supported for --stage passthrough");
                fail_with_repro("emit-meta unsupported for stage", &repro, None, &tmp);
            }
        }
    }

    let _ = fs::remove_dir_all(&tmp);
}

fn parse_u32x3(value: &str) -> Option<[u32; 3]> {
    let dimensions = value
        .split([',', 'x'])
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    dimensions.try_into().ok()
}

fn translate_maybe_reflect(
    src: &str,
    stage: Stage,
    tmp: &Path,
    want_meta: bool,
    options: TransformOptions,
) -> Result<(Vec<u8>, Option<ShaderReflection>), String> {
    if want_meta {
        let (spv, reflection) = translate_reflected_with_options(src, stage, tmp, options)?;
        Ok((spv, Some(reflection)))
    } else {
        Ok((translate_with_options(src, stage, tmp, options)?, None))
    }
}

#[cfg(feature = "serde")]
fn write_meta(path: &str, reflection: &ShaderReflection) -> Result<(), String> {
    let json = serde_json::to_string_pretty(reflection)
        .map_err(|e| format!("serialize reflection: {e}"))?;
    fs::write(path, json).map_err(|e| format!("cannot write {path}: {e}"))
}

#[cfg(not(feature = "serde"))]
fn write_meta(_path: &str, _reflection: &ShaderReflection) -> Result<(), String> {
    Err("--emit-meta requires building metal2vulkan with --features serde".to_string())
}

fn fail(why: &str) -> ! {
    eprintln!("metal2vulkan: FALLBACK ({why})");
    process::exit(2);
}

fn fail_with_repro(why: &str, ctx: &ReproContext, detail: Option<&str>, tmp: &Path) -> ! {
    match write_repro(ctx, why, detail) {
        Ok(dir) => eprintln!("metal2vulkan: wrote repro {}", dir.display()),
        Err(e) => eprintln!("metal2vulkan: repro write failed: {e}"),
    }
    let _ = fs::remove_dir_all(tmp);
    fail(why)
}

fn write_repro(ctx: &ReproContext, why: &str, detail: Option<&str>) -> Result<PathBuf, String> {
    let base = metal2vulkan::env_vars::repro_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("metal2vulkan-repros"));
    fs::create_dir_all(&base).map_err(|e| format!("create {}: {e}", base.display()))?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("time before UNIX_EPOCH: {e}"))?
        .as_nanos();
    let dir = base.join(format!("metal2vulkan-repro-{}-{stamp}", process::id()));
    fs::create_dir(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let input_name = repro_input_name(&ctx.src);
    let copied_input = dir.join(&input_name);
    fs::copy(&ctx.src, &copied_input)
        .map_err(|e| format!("copy {} -> {}: {e}", ctx.src, copied_input.display()))?;

    let repro_out = dir.join("out.spv");
    let stage = ctx.stage_for_repro();
    let translation_options = if ctx.translation_options.is_empty() {
        String::new()
    } else {
        format!(" {}", shell_join(&ctx.translation_options))
    };
    let repro_command = format!(
        "metal2vulkan {} {} --stage {}{}\n",
        shell_quote(&input_name),
        shell_quote(repro_out.file_name().unwrap().to_string_lossy().as_ref()),
        shell_quote(stage),
        translation_options,
    );
    fs::write(dir.join("repro-command.txt"), &repro_command)
        .map_err(|e| format!("write repro-command.txt: {e}"))?;

    let repro_script = format!(
        "#!/usr/bin/env sh\nset -eu\nDIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nmetal2vulkan \"$DIR/{input_name}\" \"$DIR/out.spv\" --stage {}{}\n",
        shell_quote(stage),
        translation_options,
    );
    let script_path = dir.join("repro.sh");
    fs::write(&script_path, repro_script).map_err(|e| format!("write repro.sh: {e}"))?;
    make_executable(&script_path)?;

    let mut failure = String::new();
    failure.push_str(&format!("why: {why}\n"));
    failure.push_str(&format!("source: {}\n", ctx.src));
    failure.push_str(&format!("output: {}\n", ctx.out));
    failure.push_str(&format!("requested_stage: {}\n", ctx.requested_stage));
    failure.push_str(&format!("effective_stage: {}\n", ctx.stage_for_repro()));
    failure.push_str(&format!(
        "original_command: {}\n",
        shell_join(&ctx.original_args)
    ));
    if let Some(detail) = detail {
        failure.push_str("\ndetail:\n");
        failure.push_str(detail);
        if !detail.ends_with('\n') {
            failure.push('\n');
        }
    }
    fs::write(dir.join("failure.txt"), failure).map_err(|e| format!("write failure.txt: {e}"))?;

    Ok(dir)
}

fn repro_input_name(src: &str) -> String {
    let ext = Path::new(src)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("input");
    match ext {
        "air" | "ll" => format!("input.{ext}"),
        _ => "input.bin".to_string(),
    }
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'='))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}
