//! Tile functions as compute kernels (NVMTL_TILE_SHADING=1).
//!
//! A Metal tile function reads and writes the colour attachments of the render pass through an implicit-layout imageblock
//! (`air.load.implicit_imageblock.*` / `air.store.implicit_imageblock.*`). Vulkan has no tile memory, so the function is run
//! as a compute kernel between two render-pass segments: one threadgroup per tile, and every imageblock access becomes a
//! read or write of the attachment's storage image at `threadgroup_position * tile + coordinate`.
//!
//! This rewrite adds, per attachment `A` the function touches, a `read_write` texture argument at location
//! `TILE_TEXTURE_BASE + A`, adds a `threadgroup_position_in_grid` argument, and removes the imageblock argument.
//! Anything it cannot carry exactly (explicit layouts, threadgroup imageblock memory, a computed attachment index,
//! integer attachments) is an error, never a silent approximation.

use super::air_text::{call_arguments, split_args};

pub const TILE_TEXTURE_BASE: u32 = 120;

struct Elem { name: &'static str, ty: &'static str, tex: &'static str }
const F16: Elem = Elem { name: "f16", ty: "half", tex: "half" };
const F32: Elem = Elem { name: "f32", ty: "float", tex: "float" };

/// "v4f16" -> (lanes 4, half); "f32" -> (lanes 1, float)
fn shape(suffix: &str) -> Option<(u32, &'static Elem)> {
    let (lanes, rest) = match suffix.strip_prefix('v') {
        Some(r) => { let n: u32 = r.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?; (n, &r[r.chars().take_while(char::is_ascii_digit).count()..]) }
        None => (1, suffix),
    };
    if !(1..=4).contains(&lanes) { return None; }
    match rest { "f16" => Some((lanes, &F16)), "f32" => Some((lanes, &F32)), _ => None }
}
fn vec_ty(lanes: u32, e: &Elem) -> String { if lanes == 1 { e.ty.to_string() } else { format!("<{lanes} x {}>", e.ty) } }

fn node_line_id(line: &str) -> Option<u32> {
    let t = line.trim_start();
    let rest = t.strip_prefix('!')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].trim_start().starts_with('=') { return None; }
    digits.parse().ok()
}
fn node_refs(body: &str) -> Vec<u32> {
    body.split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace())
        .filter_map(|t| t.strip_prefix('!').and_then(|d| d.parse().ok())).collect()
}

pub(crate) fn rewrite(ll: &str, tile: [u32; 2], samples: u32) -> Result<String, String> {
    let ms = samples > 1;
    if !ll.contains("implicit_imageblock") { return Err("the function has no implicit imageblock access to rewrite".into()); }
    for banned in ["air.imageblock_data", "air.get_imageblock_width", "air.get_imageblock_height", "explicit_imageblock", "threadgroup_imageblock"] {
        if ll.contains(banned) { return Err(format!("tile function uses {banned}, which is not carried (only implicit-layout imageblocks)")); }
    }
    let lines: Vec<&str> = ll.lines().collect();
    // the kernel's argument list node
    let kern = lines.iter().find(|l| l.trim_start().starts_with("!air.kernel")).ok_or("no !air.kernel")?;
    let kernel_node = node_refs(kern.split_once('=').ok_or("bad !air.kernel")?.1).first().copied().ok_or("empty !air.kernel")?;
    let node_text = |id: u32| lines.iter().find(|l| node_line_id(l) == Some(id)).map(|l| l.split_once('=').unwrap().1.trim().to_string());
    let knode = node_text(kernel_node).ok_or("kernel node missing")?;
    let name = knode.split('@').nth(1).and_then(|s| s.split(|c: char| c == ',' || c.is_whitespace()).next()).ok_or("kernel name")?.to_string();
    let arg_list_id = *node_refs(&knode).get(1).ok_or("kernel has no argument list")?;
    let arg_nodes = node_refs(&node_text(arg_list_id).ok_or("argument list missing")?);
    // which argument is the imageblock
    let mut ib_param = None;
    for (k, id) in arg_nodes.iter().enumerate() {
        let t = node_text(*id).unwrap_or_default();
        if t.contains("\"air.imageblock\"") {
            if !t.contains("\"implicit\"") { return Err("explicit-layout imageblocks are not carried".into()); }
            ib_param = Some(k);
        }
    }
    let ib_param = ib_param.ok_or("the kernel has no imageblock argument")?;
    let max_id = lines.iter().filter_map(|l| node_line_id(l)).max().unwrap_or(0);

    // gather the accesses
    let mut atts: std::collections::BTreeMap<u32, &'static Elem> = std::collections::BTreeMap::new();
    for l in &lines {
        let t = l.trim_start();
        if !(t.contains("call ") && t.contains("implicit_imageblock.")) { continue; }
        let callee = t.split('@').nth(1).and_then(|s| s.split('(').next()).unwrap_or("");
        let suffix = callee.rsplit('.').next().unwrap_or("");
        let (_, e) = shape(suffix).ok_or_else(|| format!("imageblock access {callee}: only half and float attachments are carried"))?;
        let args = split_args(call_arguments(t).unwrap_or(""));
        let a_arg = if callee.contains(".store.") { args.get(1) } else { args.first() }.ok_or("imageblock access with no attachment index")?;
        let a: u32 = a_arg.rsplit(' ').next().unwrap_or("").parse().map_err(|_| format!("imageblock access with a computed attachment index: {t}"))?;
        let rate = args.get(if callee.contains(".store.") { 3 } else { 2 }).and_then(|r| r.rsplit(' ').next()).unwrap_or("");
        if ms && rate != "2" { return Err(format!("a multisampled attachment is accessed per sample only (imageblock_data_rate::sample); this access has rate {rate}: {t}")); }
        if a >= 8 { return Err(format!("attachment {a}: a render pass has at most 8 colour attachments")); }
        if let Some(prev) = atts.insert(a, e) { if prev.name != e.name { return Err(format!("attachment {a} is accessed as both {} and {}", prev.name, e.name)); } }
    }
    if atts.is_empty() { return Err("no imageblock access found".into()); }

    let (tw, th) = (tile[0], tile[1]);
    let mut out = Vec::<String>::with_capacity(lines.len() + 64);
    let mut n = 0usize;
    for l in &lines {
        let t = l.trim_start();
        let indent = &l[..l.len() - t.len()];
        if t.starts_with("define ") && t.contains(&format!("@{name}(")) {
            // parameter list edit
            let open = t.find(&format!("@{name}(")).unwrap() + name.len() + 2;
            let mut depth = 1i32; let mut close = open;
            for (i, ch) in t[open..].char_indices() { match ch { '(' => depth += 1, ')' => { depth -= 1; if depth == 0 { close = open + i; break; } } _ => {} } }
            let mut params = split_args(&t[open..close]);
            if ib_param >= params.len() { return Err("imageblock argument index outside the parameter list".into()); }
            params.remove(ib_param);
            for a in atts.keys() { params.push(format!("ptr addrspace(1) %tile.tex{a}")); }
            params.push("<2 x i32> %tile.tg".to_string());
            out.push(format!("{indent}{}{}{}", &t[..open], params.join(", "), &t[close..]));
            continue;
        }
        if t.contains("call ") && t.contains("@air.get_imageblock_samples") {
            let result = t.split('=').next().unwrap().trim().to_string();
            out.push(format!("{indent}{result} = add i16 {samples}, 0"));
            continue;
        }
        if t.contains("call ") && t.contains("implicit_imageblock.") {
            n += 1;
            let callee = t.split('@').nth(1).and_then(|s| s.split('(').next()).unwrap().to_string();
            let suffix = callee.rsplit('.').next().unwrap();
            let (lanes, e) = shape(suffix).unwrap();
            let args = split_args(call_arguments(t).unwrap());
            let store = callee.contains(".store.");
            let (a_i, c_i) = if store { (1, 2) } else { (0, 1) };
            let a: u32 = args[a_i].rsplit(' ').next().unwrap().parse().unwrap();
            let coord = args[c_i].rsplit(' ').next().unwrap().to_string();
            let p = |s: &str| format!("%tile.{n}.{s}");
            out.push(format!("{indent}{} = zext <2 x i16> {coord} to <2 x i32>", p("c")));
            out.push(format!("{indent}{} = mul <2 x i32> %tile.tg, <i32 {tw}, i32 {th}>", p("o")));
            out.push(format!("{indent}{} = add <2 x i32> {}, {}", p("p"), p("o"), p("c")));
            let t4 = format!("<4 x {}>", e.ty);
            let tex = format!("ptr addrspace(1) %tile.tex{a}");
            let smp = if ms {
                let idx = args[if store { 4 } else { 3 }].rsplit(' ').next().unwrap().to_string();
                out.push(format!("{indent}{} = zext i16 {idx} to i32", p("smp"))); p("smp")
            } else { String::new() };
            if store {
                let v = args[0].rsplit(' ').next().unwrap().to_string();
                let v4 = if lanes == 4 { v } else if lanes == 1 {
                    out.push(format!("{indent}{} = insertelement {t4} zeroinitializer, {} {v}, i32 0", p("v4"), e.ty)); p("v4")
                } else {
                    let idx = (0..4).map(|i| format!("i32 {i}")).collect::<Vec<_>>().join(", ");
                    out.push(format!("{indent}{} = shufflevector {} {v}, {} zeroinitializer, <4 x i32> <{idx}>", p("v4"), vec_ty(lanes, e), vec_ty(lanes, e))); p("v4")
                };
                if ms { out.push(format!("{indent}call void @air.write_texture_2d_ms.v4{}({tex}, <2 x i32> {}, i32 {smp}, {t4} {v4}, i32 0, i32 3)", e.name, p("p"))); }
                else { out.push(format!("{indent}call void @air.write_texture_2d.v4{}({tex}, <2 x i32> {}, {t4} {v4}, i32 0, i32 3)", e.name, p("p"))); }
            } else {
                let result = t.split('=').next().unwrap().trim().to_string();
                out.push(format!("{indent}{} = call ptr addrspace(2) @air.get_read_sampler()", p("s")));
                if ms { out.push(format!("{indent}{} = call {{ {t4}, i8 }} @air.read_texture_2d_ms.v4{}({tex}, ptr addrspace(2) {}, <2 x i32> {}, i32 {smp}, i32 1)", p("r"), e.name, p("s"), p("p"))); }
                else { out.push(format!("{indent}{} = call {{ {t4}, i8 }} @air.read_texture_2d.v4{}({tex}, ptr addrspace(2) {}, <2 x i32> {}, <2 x i32> zeroinitializer, i32 0, i32 3)", p("r"), e.name, p("s"), p("p"))); }
                out.push(format!("{indent}{} = extractvalue {{ {t4}, i8 }} {}, 0", p("t"), p("r")));
                if lanes == 4 { out.push(format!("{indent}{result} = bitcast {t4} {} to {t4}", p("t"))); }
                else if lanes == 1 { out.push(format!("{indent}{result} = extractelement {t4} {}, i32 0", p("t"))); }
                else {
                    let idx = (0..lanes).map(|i| format!("i32 {i}")).collect::<Vec<_>>().join(", ");
                    out.push(format!("{indent}{result} = shufflevector {t4} {}, {t4} poison, <{lanes} x i32> <{idx}>", p("t")));
                }
            }
            continue;
        }
        // the argument list: renumber, drop the imageblock node, add ours
        if node_line_id(l) == Some(arg_list_id) {
            let keep: Vec<u32> = arg_nodes.iter().enumerate().filter(|(k, _)| *k != ib_param).map(|(_, id)| *id).collect();
            let extra = atts.len() as u32 + 1;
            let mut ids: Vec<String> = keep.iter().map(|i| format!("!{i}")).collect();
            for k in 0..extra { ids.push(format!("!{}", max_id + 1 + k)); }
            out.push(format!("!{arg_list_id} = !{{{}}}", ids.join(", ")));
            continue;
        }
        if let Some(id) = node_line_id(l) {
            if let Some(k) = arg_nodes.iter().position(|x| *x == id) {
                if k == ib_param { continue; }
                if k > ib_param {   // the argument index is the first field
                    let body = l.split_once("!{i32 ").ok_or("argument node shape")?;
                    let rest: String = body.1.chars().skip_while(char::is_ascii_digit).collect();
                    out.push(format!("{}!{{i32 {}{}", body.0, k - 1, rest));
                    continue;
                }
            }
        }
        out.push(l.to_string());
    }
    // new metadata nodes
    let first_new = max_id + 1;
    let base_idx = (arg_nodes.len() - 1) as u32;
    for (k, (a, e)) in atts.iter().enumerate() {
        out.push(format!("!{} = !{{i32 {}, !\"air.texture\", !\"air.location_index\", i32 {}, i32 1, !\"air.read_write\", !\"air.arg_type_name\", !\"texture2d{}<{}, read_write>\", !\"air.arg_name\", !\"tile_tex{a}\"}}",
            first_new + k as u32, base_idx + k as u32, TILE_TEXTURE_BASE + a, if ms { "_ms" } else { "" }, e.tex));
    }
    out.push(format!("!{} = !{{i32 {}, !\"air.threadgroup_position_in_grid\", !\"air.arg_type_name\", !\"uint2\", !\"air.arg_name\", !\"tile_tg\"}}",
        first_new + atts.len() as u32, base_idx + atts.len() as u32));
    let mut text = out.join("\n");
    text.push('\n');
    let mut declares = std::collections::BTreeSet::new();
    declares.insert("declare ptr addrspace(2) @air.get_read_sampler()".to_string());
    for e in atts.values() {
        let t4 = format!("<4 x {}>", e.ty);
        if ms {
            declares.insert(format!("declare {{ {t4}, i8 }} @air.read_texture_2d_ms.v4{}(ptr addrspace(1), ptr addrspace(2), <2 x i32>, i32, i32)", e.name));
            declares.insert(format!("declare void @air.write_texture_2d_ms.v4{}(ptr addrspace(1), <2 x i32>, i32, {t4}, i32, i32)", e.name));
        } else {
            declares.insert(format!("declare {{ {t4}, i8 }} @air.read_texture_2d.v4{}(ptr addrspace(1), ptr addrspace(2), <2 x i32>, <2 x i32>, i32, i32)", e.name));
            declares.insert(format!("declare void @air.write_texture_2d.v4{}(ptr addrspace(1), <2 x i32>, {t4}, i32, i32)", e.name));
        }
    }
    for d in declares {
        let sym = d.split('@').nth(1).unwrap().split('(').next().unwrap().to_string();
        if !text.contains(&format!("@{sym}(")) || !text.lines().any(|l| l.starts_with("declare") && l.contains(&format!("@{sym}("))) { text.push_str(&d); text.push('\n'); }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the sanitized shape of a real tile kernel (AIR from Apple's compiler, pointers opaque)
    const TILE: &str = r#"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }
define void @tile_k(%"struct.metal::_imageblock_base" %0, <2 x i16> noundef %1, ptr addrspace(2) nocapture noundef readonly align 4 dereferenceable(4) "air-buffer-no-alias" %2) local_unnamed_addr #0 {
  %4 = tail call fast <4 x half> @air.load.implicit_imageblock.v4f16(i32 0, <2 x i16> %1, i32 0, i16 0) #3
  %5 = tail call fast float @air.load.implicit_imageblock.f32(i32 1, <2 x i16> %1, i32 0, i16 0) #3
  %6 = load float, ptr addrspace(2) %2, align 4
  %7 = fadd float %5, %6
  tail call void @air.store.implicit_imageblock.v4f16(<4 x half> %4, i32 0, <2 x i16> %1, i32 0, i16 0) #4
  tail call void @air.store.implicit_imageblock.f32(float %7, i32 1, <2 x i16> %1, i32 0, i16 0) #4
  ret void
}
declare <4 x half> @air.load.implicit_imageblock.v4f16(i32, <2 x i16>, i32, i16) local_unnamed_addr #1
declare float @air.load.implicit_imageblock.f32(i32, <2 x i16>, i32, i16) local_unnamed_addr #1
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16) local_unnamed_addr #2
declare void @air.store.implicit_imageblock.f32(float, i32, <2 x i16>, i32, i16) local_unnamed_addr #2
!air.kernel = !{!9}
!9 = !{ptr @tile_k, !10, !11}
!10 = !{}
!11 = !{!12, !14, !15}
!12 = !{i32 0, !"air.imageblock", !"implicit", !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<Px, layout_implicit>", !"air.arg_name", !"ib"}
!13 = !{i32 0, i32 8, i32 0, !"half4", !"c", !"air.render_target", i32 0, i32 8, i32 4, i32 0, !"float", !"d", !"air.render_target", i32 1}
!14 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!15 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"g"}
"#;

    #[test]
    fn the_rewrite_removes_the_imageblock_and_adds_attachment_textures() {
        let out = rewrite(TILE, [32, 32], 1).unwrap();
        assert!(!out.contains("implicit_imageblock.v4f16(i32") || out.lines().all(|l| !l.contains("call") || !l.contains("implicit_imageblock")), "{out}");
        assert!(!out.contains("\"air.imageblock\""), "the imageblock argument node is gone:\n{out}");
        assert!(out.contains("define void @tile_k(<2 x i16> noundef %1,"), "the imageblock parameter is gone:\n{out}");
        assert!(out.contains("ptr addrspace(1) %tile.tex0, ptr addrspace(1) %tile.tex1, <2 x i32> %tile.tg)"), "{out}");
        assert!(out.contains("i32 120, i32 1, !\"air.read_write\"") && out.contains("i32 121, i32 1, !\"air.read_write\""), "textures at 120 + attachment:\n{out}");
        assert!(out.contains("!14 = !{i32 0, !\"air.thread_position_in_threadgroup\""), "later arguments are renumbered:\n{out}");
        assert!(out.contains("!15 = !{i32 1, !\"air.buffer\""), "{out}");
        assert!(out.contains("mul <2 x i32> %tile.tg, <i32 32, i32 32>"), "the tile origin is threadgroup * tile");
        assert!(out.contains("@air.read_texture_2d.v4f16(") && out.contains("@air.write_texture_2d.v4f32("));
    }

    #[test]
    fn what_cannot_be_carried_is_refused_by_name() {
        assert!(rewrite(&TILE.replace("\"implicit\"", "\"explicit\""), [32, 32], 1).unwrap_err().contains("explicit"));
        assert!(rewrite(&TILE.replace("i32 1, <2 x i16> %1, i32 0, i16 0) #3", "i32 %6, <2 x i16> %1, i32 0, i16 0) #3"), [32, 32], 1).unwrap_err().contains("computed attachment"));
        assert!(rewrite(&format!("{TILE}\ndeclare i32 @air.get_imageblock_width()"), [32, 32], 1).unwrap_err().contains("not carried"));
        assert!(rewrite("define void @k() {\n ret void\n}\n", [32, 32], 1).is_err());
    }

    #[test]
    fn the_rewritten_tile_kernel_translates_to_valid_spirv_with_two_storage_images() {
        let out = rewrite(TILE, [32, 32], 1).unwrap();
        let tmp = std::env::temp_dir().join(format!("tile_rewrite_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let bytes = crate::translate_sanitized_native(&out, crate::passes::Stage::Kernel, &tmp).unwrap_or_else(|e| panic!("translate: {e}\n{out}"));
        crate::tools::spirv_val_bytes(&bytes, &tmp).expect("spirv-val");
        let asm = crate::disassemble(&bytes).expect("disassemble");
        assert!(asm.matches("OpImageWrite").count() >= 2, "both attachments are written:\n{asm}");
        assert!(asm.contains("OpImageRead") || asm.contains("OpImageFetch"), "the attachments are read:\n{asm}");
        assert!(asm.contains("WorkgroupId") || asm.contains("GlobalInvocationId"), "the tile index is the workgroup id");
        let reflection = crate::reflect_sanitized(&out, crate::passes::Stage::Kernel, crate::passes::TransformOptions::default()).expect("reflect");
        let textures: Vec<u32> = reflection.bindings.iter().filter(|b| format!("{:?}", b.kind).contains("Image")).map(|b| b.metal_index as u32).collect();
        assert!(textures.contains(&120) && textures.contains(&121), "texture bindings at 120 and 121, got {textures:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// the shape of a per-sample tile function (AIR from Apple's compiler, pointers opaque): the M1 oracle arm's kernel
    const TILE_MS: &str = r#"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }
define void @tile_ms(%"struct.metal::_imageblock_base" %0, <2 x i16> noundef %1) local_unnamed_addr #0 {
entry:
  %n = tail call i16 @air.get_imageblock_samples() #4
  br label %loop
loop:
  %s = phi i16 [ 0, %entry ], [ %s2, %loop ]
  %v = tail call fast <4 x half> @air.load.implicit_imageblock.v4f16(i32 0, <2 x i16> %1, i32 2, i16 %s) #5
  %x = extractelement <4 x half> %v, i64 0
  %f = uitofp i16 %s to half
  %m = fmul half %f, 0xH3000
  %y = fadd half %x, %m
  %w = insertelement <4 x half> %v, half %y, i64 0
  tail call void @air.store.implicit_imageblock.v4f16(<4 x half> %w, i32 0, <2 x i16> %1, i32 2, i16 %s) #6
  %s2 = add i16 %s, 1
  %c = icmp ult i16 %s2, %n
  br i1 %c, label %loop, label %done
done:
  ret void
}
declare i16 @air.get_imageblock_samples() local_unnamed_addr #1
declare <4 x half> @air.load.implicit_imageblock.v4f16(i32, <2 x i16>, i32, i16) local_unnamed_addr #2
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16) local_unnamed_addr #3
!air.kernel = !{!9}
!9 = !{ptr @tile_ms, !10, !11}
!10 = !{}
!11 = !{!12, !14}
!12 = !{i32 0, !"air.imageblock", !"implicit", !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<Pm, layout_implicit>", !"air.arg_name", !"ib"}
!13 = !{i32 0, i32 8, i32 0, !"half4", !"c", !"air.render_target", i32 0}
!14 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;

    #[test]
    fn a_multisampled_tile_function_reads_and_writes_each_sample_of_a_storage_image() {
        let out = rewrite(TILE_MS, [16, 16], 4).unwrap();
        assert!(out.contains("texture2d_ms<half, read_write>"), "the attachment is declared multisampled:\n{out}");
        assert!(out.contains("@air.read_texture_2d_ms.v4f16(") && out.contains("@air.write_texture_2d_ms.v4f16("), "{out}");
        assert!(out.contains("%n = add i16 4, 0"), "the sample count query becomes the pipeline's sample count:\n{out}");
        let tmp = std::env::temp_dir().join(format!("tile_ms_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let bytes = crate::translate_sanitized_native(&out, crate::passes::Stage::Kernel, &tmp).unwrap_or_else(|e| panic!("translate: {e}\n{out}"));
        crate::tools::spirv_val_bytes(&bytes, &tmp).expect("spirv-val");
        let asm = crate::disassemble(&bytes).expect("disassemble");
        assert!(asm.contains("OpTypeImage") && asm.lines().any(|l| l.contains("OpTypeImage") && l.contains("2D 0 0 1 2")), "a multisampled storage image type (MS = 1, Sampled = 2):\n{asm}");
        assert!(asm.lines().any(|l| l.contains("OpImageWrite") && l.contains("Sample")), "the write carries the Sample operand:\n{asm}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_pixel_rate_access_to_a_multisampled_attachment_is_refused() {
        let ll = TILE_MS.replace("i32 2, i16 %s)", "i32 0, i16 %s)");
        assert!(rewrite(&ll, [16, 16], 4).unwrap_err().contains("per sample only"));
    }
}
