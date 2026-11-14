//! Metal 4 shader tensors (AIR 2.8 and later) as plain device memory.
//!
//! A `tensor<device T, ...>` kernel argument is an opaque handle in AIR. The driver binds the tensor's entry in the tensor
//! table (plugin NVMTLResourceIDs.h, `nvmtl_tensor_entry`) as an ordinary buffer binding, so the argument is a pointer to
//! that entry, and this pass makes the handle that entry:
//!
//! ```text
//! struct nvmtl_tensor_entry { u64 data_addr; u64 scales_addr; u32 rank; u32 dtype; u32 element_bits; u32 block_factor0;
//!                             i32 dims[16]; i32 strides[16]; i32 scale_dims[16]; i32 scale_strides[16]; }   // 288 bytes
//! ```
//!
//! Extents and strides are in elements, dimension 0 innermost, as MTLTensor reports them; an element sits at bit offset
//! (sum of index * stride) * element_bits from data_addr. With the entry concrete, `air.get_extent`, `air.get_stride` and
//! `air.get_data_pointer` become loads and an index computation (a sub-byte element yields the byte that holds it), and the
//! Metal Performance Primitives `matmul2d` run call, which AIR leaves as an externally defined function, becomes a function
//! defined here. Each lane of the executing simdgroup computes the output elements `lane`, `lane + threads`, ...; the
//! multiply is a plain loop, so it does not use tensor cores, but it is exact for the element types listed in `Kind`.
//! Sub-byte and block-scaled operands are not among them: their run functions keep the declaration and the translator
//! refuses them by name.
//!
//! Not covered, and left untouched so the translator still names them unsupported: tensors in threadgroup memory,
//! tensors built inside the shader from a pointer, scaled operands, cooperative-tensor destinations, convolution, and a
//! multiply-accumulate run by more than one simdgroup (the lanes of the extra simdgroups cannot be told apart here).

use super::air_text::{call_arguments, last_token, split_args};
use std::borrow::Cow;
use std::collections::BTreeSet;

/// Layout of `nvmtl_tensor_entry` (plugin NVMTLResourceIDs.h). Changing one changes the other.
pub(crate) const DESC_BYTES: usize = 288;
const DESC_TYPE: &str = "%nv.tensor_t";
const DESC_TYPE_DEF: &str = "%nv.tensor_t = type { i64, i64, i32, i32, i32, i32, [16 x i32], [16 x i32], [16 x i32], [16 x i32] }";
const F_DATA: u32 = 0;
const F_SCALES: u32 = 1;
const F_BF0: u32 = 5;
const F_SSTR: u32 = 9;
/// The scale data type a 2.9 matmul call names for a block-scaled operand: metal_fp8_ue8m0_format.
const SCALE_UE8M0: i64 = 0x5000_0008;
const F_BITS: u32 = 4;
const F_EXT: u32 = 6;
const F_STR: u32 = 7;
const MAX_RANK: i64 = 16;
const RUN: &str = "__tensorops_impl_matmul2d_op_run_";

/// Element types the generated multiply knows. The packed ones (I4 .. E5M2) are read by bit offset, low bits first, and
/// decoded as Apple GPUs decode them (measured with an identity multiply over every byte value, tests/packed): int2/int4
/// sign-extended, fp4 e2m1, fp8 e5m2 with IEEE infinities and NaNs, fp8 e4m3 with none (0x7f is 480).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    F16,
    F32,
    B16,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I4,
    U4,
    I2,
    U2,
    Fp4,
    E4m3,
    E5m2,
}

impl Kind {
    fn parse(token: &str) -> Option<Kind> {
        Some(match token {
            "f16" => Kind::F16,
            "f32" => Kind::F32,
            "b16" => Kind::B16,
            "i8" | "s8" => Kind::I8,
            "u8" | "ui8" => Kind::U8,
            "i16" | "s16" => Kind::I16,
            "u16" | "ui16" => Kind::U16,
            "i32" | "s32" => Kind::I32,
            "u32" | "ui32" => Kind::U32,
            "i4" => Kind::I4,
            "ui4" => Kind::U4,
            "i2" => Kind::I2,
            "ui2" => Kind::U2,
            "fp4e2m1" => Kind::Fp4,
            "fp8e4m3" => Kind::E4m3,
            "fp8e5m2" => Kind::E5m2,
            _ => return None,
        })
    }
    fn ty(self) -> &'static str {
        match self {
            Kind::F16 => "half",
            Kind::F32 => "float",
            Kind::I8 | Kind::U8 => "i8",
            Kind::I16 | Kind::U16 | Kind::B16 => "i16",
            Kind::I32 | Kind::U32 => "i32",
            // read by bit offset (packed_bits); the byte that holds an element
            Kind::I4 | Kind::U4 | Kind::I2 | Kind::U2 | Kind::Fp4 | Kind::E4m3 | Kind::E5m2 => "i8",
        }
    }
    fn align(self) -> u32 {
        match self {
            Kind::I8 | Kind::U8 | Kind::I4 | Kind::U4 | Kind::I2 | Kind::U2 | Kind::Fp4 | Kind::E4m3 | Kind::E5m2 => 1,
            Kind::F16 | Kind::I16 | Kind::U16 | Kind::B16 => 2,
            _ => 4,
        }
    }
    fn is_float(self) -> bool {
        matches!(self, Kind::F16 | Kind::F32 | Kind::B16 | Kind::Fp4 | Kind::E4m3 | Kind::E5m2)
    }
    fn signed(self) -> bool {
        matches!(self, Kind::I8 | Kind::I16 | Kind::I32 | Kind::I4 | Kind::I2)
    }
    /// Bits of a packed element, None for a byte-addressable one.
    fn packed_bits(self) -> Option<u32> {
        match self {
            Kind::I4 | Kind::U4 | Kind::Fp4 => Some(4),
            Kind::I2 | Kind::U2 => Some(2),
            Kind::E4m3 | Kind::E5m2 => Some(8),
            _ => None,
        }
    }
    /// (exponent bits, mantissa bits, bias, IEEE infinities and NaNs) of a packed float.
    fn minifloat(self) -> Option<(u32, u32, i32, bool)> {
        match self {
            Kind::Fp4 => Some((2, 1, 1, false)),
            Kind::E4m3 => Some((4, 3, 7, false)),
            Kind::E5m2 => Some((5, 2, 15, true)),
            _ => None,
        }
    }
}

pub(crate) fn lower_tensors(san_ll: &str) -> Cow<'_, str> {
    let has_handles = san_ll.contains("_tensor.") || san_ll.contains("!\"air.tensor\"");
    let has_runs = san_ll.contains(RUN);
    if !has_handles && !has_runs {
        return Cow::Borrowed(san_ll);
    }
    let mut combos: Vec<RunCombo> = Vec::new();
    let mut fn_start = 0usize;
    let mut kept_calls: BTreeSet<String> = BTreeSet::new();
    let mut touched = false;
    let mut decl_lines: Vec<usize> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for line in san_ll.lines() {
        let t = line.trim_start();
        if t.starts_with("define ") {
            fn_start = lines.len();
        }
        if t.starts_with('!') && t.contains("!\"air.tensor\"") {
            lines.push(rewrite_metadata(line));
            touched = true;
            continue;
        }
        if t.starts_with("declare ") && t.contains(&format!("@{RUN}")) {
            decl_lines.push(lines.len());
            lines.push(line.to_string());
            continue;
        }
        if t.contains("@air.get_extent_global_tensor.") || t.contains("@air.get_stride_global_tensor.") {
            if let Some(new) = rewrite_extent_or_stride(line) {
                lines.push(new);
                touched = true;
                continue;
            }
        }
        if t.contains("@air.get_data_pointer_global_tensor.") {
            if let Some(new) = rewrite_data_pointer(line) {
                lines.push(new);
                touched = true;
                continue;
            }
        }
        if t.contains(&format!("@{RUN}")) && !t.starts_with("declare ") {
            match rewrite_run_call(line, &lines[fn_start..], san_ll, &mut combos) {
                Some(new) => {
                    lines.push(new);
                    touched = true;
                    continue;
                }
                None => {
                    if let Some(name) = callee(t) {
                        kept_calls.insert(name.to_string());
                    }
                }
            }
        }
        lines.push(line.to_string());
    }
    if !touched {
        return Cow::Borrowed(san_ll);
    }
    // A declaration goes away only when no call to it remains; the others stay so the translator names them.
    for &i in decl_lines.iter().rev() {
        if let Some(name) = callee(lines[i].trim_start()) {
            if !kept_calls.contains(name) && combos.iter().any(|c| c.name == name) {
                lines.remove(i);
            }
        }
    }
    // The descriptor type is defined ahead of every use: LLVM resolves a type name where it is first written.
    let mut text = String::with_capacity(san_ll.len() + 4096);
    text.push_str(DESC_TYPE_DEF);
    text.push('\n');
    text.push_str(&lines.join("\n"));
    text.push('\n');
    if !combos.is_empty() {
        for needed in [
            "declare float @air.simd_prefix_exclusive_sum.f32(float)",
            "declare void @air.simdgroup.barrier(i32, i32, i32)",
        ] {
            let name = needed.split('@').nth(1).and_then(|s| s.split('(').next()).unwrap_or("");
            let probe = format!("@{name}(");
            if !text.lines().any(|l| l.starts_with("declare ") && l.contains(&probe)) {
                text.push_str(needed);
                text.push('\n');
            }
        }
    }
    for combo in &combos {
        if let Some(def) = generate_run(combo) {
            text.push_str(&def);
        }
    }
    Cow::Owned(text)
}

fn callee(line: &str) -> Option<&str> {
    let at = line.find('@')?;
    let rest = &line[at + 1..];
    let end = rest.find('(')?;
    Some(&rest[..end])
}

// ---- kernel argument metadata ----

fn rewrite_metadata(line: &str) -> String {
    let mut s = line.replacen("!\"air.tensor\"", "!\"air.buffer\"", 1);
    // A constant-space tensor is read through its descriptor like any other: the descriptor is a device buffer.
    s = s.replacen("!\"air.address_space\", i32 2", "!\"air.address_space\", i32 1", 1);
    s = s.replacen(
        "!\"air.arg_type_name\"",
        &format!(
            "!\"air.arg_type_size\", i32 {DESC_BYTES}, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\""
        ),
        1,
    );
    s
}

// ---- queries and element address ----

fn result_of(line: &str) -> Option<(&str, &str)> {
    let (lhs, rhs) = line.split_once(" = ")?;
    Some((lhs.trim(), rhs))
}

fn indent_of(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

fn literal(v: &str) -> Option<i64> {
    v.parse::<i64>().ok()
}

fn rewrite_extent_or_stride(line: &str) -> Option<String> {
    let (res, rhs) = result_of(line)?;
    let is_stride = rhs.contains("@air.get_stride_global_tensor.");
    // only the i32 query is lowered; the i16 and i64 forms keep going to the translator's refusal
    if !rhs.contains(" i32 @air.get_") {
        return None;
    }
    let args = split_args(call_arguments(&rhs[rhs.find('@')?..])?);
    // AIR 2.9 appends a plane operand; 2.8 does not
    let (handle, dim) = match args.as_slice() {
        [handle, _rank, dim] | [handle, _rank, dim, _] => (handle, dim),
        _ => return None,
    };
    let handle = last_token(handle);
    let dim = last_token(dim);
    let ind = indent_of(line);
    let rn = format!("%nvt.{}", res.trim_start_matches('%'));
    let field = if is_stride { F_STR } else { F_EXT };
    let mut out = String::new();
    let index = match literal(dim) {
        Some(v) => v.to_string(),
        None => {
            out.push_str(&format!("{ind}{rn}.dim = sext i16 {dim} to i64\n"));
            format!("{rn}.dim")
        }
    };
    out.push_str(&format!(
        "{ind}{rn}.p = getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {handle}, i64 0, i32 {field}, i64 {index}\n"
    ));
    out.push_str(&format!("{ind}{res} = load i32, ptr addrspace(1) {rn}.p, align 4"));
    Some(out)
}

fn rewrite_data_pointer(line: &str) -> Option<String> {
    let (res, rhs) = result_of(line)?;
    let name = callee(rhs)?;
    // air.get_data_pointer_global_tensor.<s|u>.<index type>.<global|constant>
    let parts: Vec<&str> = name.split('.').collect();
    let [_, _, _s, idx_ty, space] = parts.as_slice() else { return None };
    let asp = match *space {
        "global" => 1,
        "constant" => 2,
        _ => return None,
    };
    let (idx_bits, idx_align) = match *idx_ty {
        "i32" => (32, 4),
        "i16" => (16, 2),
        "i64" => (64, 8),
        _ => return None,
    };
    let args = split_args(call_arguments(&rhs[rhs.find('@')?..])?);
    // AIR 2.9 appends a static-extents flag and a plane operand; 2.8 has handle, rank and the index array only
    let (handle, rank, indices) = match args.as_slice() {
        [handle, rank, indices] => (handle, rank, indices),
        // The fourth operand is 0 or 1 for the plain scalar element types (1 when the extents are static) and a format
        // code for the packed types (16 int2b, 20 int4b, 21 uint4b, 22 fp4 e2m1, 24/25 fp8, ...). An element of a packed
        // type is not byte addressable, and Apple GPUs answer an element access with the byte that holds it (measured:
        // elements 0 and 1 of an int4 tensor both read byte 0, element x of an fp8 tensor reads byte x as an integer),
        // which is the address computed below from element_bits.
        [handle, rank, indices, flag, _] if literal(last_token(flag)).is_some() => (handle, rank, indices),
        _ => return None,
    };
    let handle = last_token(handle);
    let rank = literal(last_token(rank))?;
    if !(0..=MAX_RANK).contains(&rank) {
        return None;
    }
    let indices = last_token(indices);
    let ind = indent_of(line);
    let rn = format!("%nvt.{}", res.trim_start_matches('%'));
    let mut o = String::new();
    o.push_str(&format!("{ind}{rn}.bp = getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {handle}, i64 0, i32 {F_DATA}\n"));
    o.push_str(&format!("{ind}{rn}.b = load i64, ptr addrspace(1) {rn}.bp, align 8\n"));
    o.push_str(&format!("{ind}{rn}.ep = getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {handle}, i64 0, i32 {F_BITS}\n"));
    o.push_str(&format!("{ind}{rn}.e32 = load i32, ptr addrspace(1) {rn}.ep, align 4\n"));
    o.push_str(&format!("{ind}{rn}.e = zext i32 {rn}.e32 to i64\n"));
    let mut acc = String::from("0");
    for k in 0..rank {
        o.push_str(&format!("{ind}{rn}.i{k}p = getelementptr inbounds i{idx_bits}, ptr {indices}, i64 {k}\n"));
        o.push_str(&format!("{ind}{rn}.i{k} = load i{idx_bits}, ptr {rn}.i{k}p, align {idx_align}\n"));
        let idx = if idx_bits == 64 {
            format!("{rn}.i{k}")
        } else {
            o.push_str(&format!("{ind}{rn}.i{k}x = sext i{idx_bits} {rn}.i{k} to i64\n"));
            format!("{rn}.i{k}x")
        };
        o.push_str(&format!("{ind}{rn}.sp{k} = getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {handle}, i64 0, i32 {F_STR}, i64 {k}\n"));
        o.push_str(&format!("{ind}{rn}.s{k} = load i32, ptr addrspace(1) {rn}.sp{k}, align 4\n"));
        o.push_str(&format!("{ind}{rn}.sx{k} = sext i32 {rn}.s{k} to i64\n"));
        o.push_str(&format!("{ind}{rn}.m{k} = mul i64 {idx}, {rn}.sx{k}\n"));
        o.push_str(&format!("{ind}{rn}.a{k} = add i64 {acc}, {rn}.m{k}\n"));
        acc = format!("{rn}.a{k}");
    }
    // element_bits is a bit width: the byte offset of element `acc` is acc * bits / 8 (the holding byte for sub-byte types)
    o.push_str(&format!("{ind}{rn}.ob = mul i64 {acc}, {rn}.e\n"));
    o.push_str(&format!("{ind}{rn}.o = lshr i64 {rn}.ob, 3\n"));
    o.push_str(&format!("{ind}{rn}.addr = add i64 {rn}.b, {rn}.o\n"));
    o.push_str(&format!("{ind}{res} = inttoptr i64 {rn}.addr to ptr addrspace({asp})"));
    Some(o)
}

// ---- matmul2d ----

#[derive(Clone, PartialEq, Eq)]
struct RunCombo {
    name: String,
    /// descriptor struct type and the three operand struct types, as spelled in the caller (`%"struct..."`)
    desc_ty: String,
    op_ty: [String; 3],
    /// 0 = a handle, 1 = a handle with two offsets (a slice)
    op_kind: [u8; 3],
    /// the AIR 2.9 form (20 operands, trailing _v2 in the name) rather than the 2.8 form (8 operands)
    v2: bool,
    /// left and right carry ue8m0 block scales (one per 32 elements along dimension 0, in the tensor's scales plane)
    scaled: [bool; 2],
    id: usize,
}

impl RunCombo {
    fn symbol(&self) -> String {
        format!("{}.v{}", self.name, self.id)
    }
}

/// The struct type an SSA pointer names: an alloca, possibly behind no-op bitcasts, in the lines of this function so far.
fn alloca_type(fn_lines: &[String], value: &str) -> Option<String> {
    let mut v = value.to_string();
    for _ in 0..4 {
        let prefix = format!("{v} = ");
        let def = fn_lines.iter().rev().find(|l| l.trim_start().starts_with(&prefix))?;
        let rhs = def.trim_start()[prefix.len()..].trim();
        if let Some(rest) = rhs.strip_prefix("alloca ") {
            return Some(rest.split(',').next()?.trim().to_string());
        }
        let rest = rhs.strip_prefix("bitcast ptr ")?;
        v = rest.split(" to ").next()?.trim().to_string();
    }
    None
}

fn type_body<'a>(src: &'a str, ty: &str) -> Option<&'a str> {
    let key = format!("{ty} = type ");
    let at = src.find(&key)?;
    let rest = &src[at + key.len()..];
    Some(rest.lines().next()?.trim())
}

/// Call arguments in order: descriptor, left, left type, left scale type, 2 scale block sizes, 2 static extents, then
/// the same seven for right, then destination, its type, 2 static extents, and the thread count.
fn rewrite_run_call(line: &str, fn_lines: &[String], src: &str, known: &mut Vec<RunCombo>) -> Option<String> {
    let at = line.find(&format!("@{RUN}"))?;
    let name = callee(&line[at..])?.to_string();
    let args = split_args(call_arguments(&line[at..])?);
    let v2 = match args.len() {
        20 => true,
        8 => false,
        _ => return None,
    };
    let v = |i: usize| literal(last_token(&args[i]));
    // positions of the three operand pointers and their descriptor types
    let (ptrs, types) = if v2 { ([1usize, 8, 15], [2usize, 9, 16]) } else { ([1usize, 3, 5], [2usize, 4, 6]) };
    let kinds = [v(types[0])?, v(types[1])?, v(types[2])?];
    // handle (0) or slice of a handle (1) only
    if !kinds.iter().all(|t| *t == 0 || *t == 1) {
        return None;
    }
    let ks = kinds_of(&name)?;
    // a 2.9 operand may carry ue8m0 block scales of 32 x 1 (MetalPerformancePrimitives allows no other); only the
    // packed float formats are measured with scales, an integer operand with scales stays refused (Apple GPUs return
    // zeros for it)
    let mut scaled = [false; 2];
    if v2 {
        for (n, (sd, b0, b1)) in [(3usize, 4usize, 5usize), (10, 11, 12)].into_iter().enumerate() {
            match v(sd)? {
                0 => {}
                SCALE_UE8M0 if v(b0)? == 32 && v(b1)? == 1 && ks[n].minifloat().is_some() => scaled[n] = true,
                _ => return None,
            }
        }
    }
    let desc_ty = alloca_type(fn_lines, last_token(&args[0]))?;
    if type_body(src, &desc_ty)? != "{ i32, i32, i32, i8, i8, i8, i32 }" {
        return None;
    }
    let mut op_ty: [String; 3] = Default::default();
    let mut op_kind = [0u8; 3];
    for (n, arg) in ptrs.into_iter().enumerate() {
        let ty = alloca_type(fn_lines, last_token(&args[arg]))?;
        let body = type_body(src, &ty)?;
        let kind = if body == "{ ptr addrspace(1) }" {
            0
        } else if body.starts_with("{ %") && body.ends_with(", ptr addrspace(1) }") && body.matches(',').count() == 1 {
            1
        } else {
            return None;
        };
        if kind as i64 != kinds[n] {
            return None;
        }
        op_ty[n] = ty;
        op_kind[n] = kind;
    }
    let mut combo = RunCombo { name: name.clone(), desc_ty, op_ty, op_kind, v2, scaled, id: 0 };
    match known.iter().position(|c| c.name == combo.name && c.desc_ty == combo.desc_ty && c.op_ty == combo.op_ty && c.op_kind == combo.op_kind && c.v2 == combo.v2 && c.scaled == combo.scaled) {
        Some(i) => combo.id = known[i].id,
        None => {
            combo.id = known.len();
            known.push(combo.clone());
        }
    }
    Some(line.replacen(&format!("@{name}("), &format!("@{}(", combo.symbol()), 1))
}

fn kinds_of(name: &str) -> Option<[Kind; 3]> {
    // __tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2 (AIR 2.9) or ..._dv_f32 (AIR 2.8)
    let rest = name.strip_prefix(RUN)?;
    let t: Vec<&str> = rest.split('_').collect();
    let counted = match t.len() {
        6 => true,
        7 => t[6].starts_with('v'),
        _ => false,
    };
    if !counted || t[0] != "dv" || t[2] != "dv" || t[4] != "dv" {
        return None;
    }
    Some([Kind::parse(t[1])?, Kind::parse(t[3])?, Kind::parse(t[5])?])
}

struct Gen {
    s: String,
    n: u32,
}

impl Gen {
    fn line(&mut self, l: impl AsRef<str>) {
        self.s.push_str("  ");
        self.s.push_str(l.as_ref());
        self.s.push('\n');
    }
    fn tmp(&mut self, hint: &str) -> String {
        self.n += 1;
        format!("%{hint}{}", self.n)
    }
    fn op(&mut self, hint: &str, rhs: String) -> String {
        let t = self.tmp(hint);
        self.line(format!("{t} = {rhs}"));
        t
    }
}

/// Reads one operand struct (a handle, or a handle with two offsets) and the descriptor behind it.
struct Operand {
    base: String,
    off: [String; 2],
    ext: [String; 2],
    stride: [String; 2],
    /// the descriptor, for the scales plane of a scaled operand
    handle: String,
}

fn read_operand(g: &mut Gen, tag: &str, ptr: &str, ty: &str, kind: u8) -> Operand {
    // kind 0: { handle }; kind 1: { { { { i32[2] } } } offsets; handle }. The element types are spelled as the caller
    // allocated them so the access chains stay legal under Logical addressing.
    let hidx = if kind == 1 { 1 } else { 0 };
    let hp = g.op(&format!("{tag}hp"), format!("getelementptr inbounds {ty}, ptr {ptr}, i64 0, i32 {hidx}"));
    let h = g.op(&format!("{tag}h"), format!("load ptr addrspace(1), ptr {hp}, align 8"));
    let mut off = [String::from("0"), String::from("0")];
    if kind == 1 {
        for k in 0..2usize {
            let p = g.op(&format!("{tag}op{k}"), format!("getelementptr inbounds {ty}, ptr {ptr}, i64 0, i32 0, i32 0, i32 0, i32 0, i64 {k}"));
            let v = g.op(&format!("{tag}o{k}"), format!("load i32, ptr {p}, align 4"));
            off[k] = g.op(&format!("{tag}ox{k}"), format!("sext i32 {v} to i64"));
        }
    }
    let bp = g.op(&format!("{tag}bp"), format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_DATA}"));
    let base = g.op(&format!("{tag}b"), format!("load i64, ptr addrspace(1) {bp}, align 8"));
    let mut ext = [String::new(), String::new()];
    let mut stride = [String::new(), String::new()];
    for k in 0..2usize {
        let ep = g.op(&format!("{tag}ep{k}"), format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_EXT}, i64 {k}"));
        let e = g.op(&format!("{tag}e{k}"), format!("load i32, ptr addrspace(1) {ep}, align 4"));
        ext[k] = g.op(&format!("{tag}ex{k}"), format!("sext i32 {e} to i64"));
        let sp = g.op(&format!("{tag}sp{k}"), format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_STR}, i64 {k}"));
        let s = g.op(&format!("{tag}s{k}"), format!("load i32, ptr addrspace(1) {sp}, align 4"));
        stride[k] = g.op(&format!("{tag}sx{k}"), format!("sext i32 {s} to i64"));
    }
    Operand { base, off, ext, stride, handle: h }
}

/// The ue8m0 block scale of the element at (d0, d1) of a scaled operand: 2^(byte - 127) from its scales plane, one scale
/// per block_factor0 elements along dimension 0, scale rows scale_strides apart (0 is 2^-127, 255 is NaN).
fn scale_of(g: &mut Gen, o: &Operand, d0: &str, d1: &str) -> String {
    let h = &o.handle;
    let sp = g.op("scp", format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_SCALES}"));
    let sa = g.op("sca", format!("load i64, ptr addrspace(1) {sp}, align 8"));
    let bp = g.op("scbp", format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_BF0}"));
    let b32 = g.op("scb", format!("load i32, ptr addrspace(1) {bp}, align 4"));
    let bpos = g.op("scbn", format!("icmp sgt i32 {b32}, 0"));
    let bsafe = g.op("scbs", format!("select i1 {bpos}, i32 {b32}, i32 1"));
    let bf = g.op("scbx", format!("zext i32 {bsafe} to i64"));
    let mut st = Vec::new();
    for k in 0..2u32 {
        let p = g.op("scsp", format!("getelementptr inbounds {DESC_TYPE}, ptr addrspace(1) {h}, i64 0, i32 {F_SSTR}, i64 {k}"));
        let v = g.op("scs", format!("load i32, ptr addrspace(1) {p}, align 4"));
        st.push(g.op("scsx", format!("sext i32 {v} to i64")));
    }
    let blk = g.op("scbk", format!("sdiv i64 {d0}, {bf}"));
    let i0 = g.op("sci", format!("mul i64 {blk}, {}", st[0]));
    let i1 = g.op("sci", format!("mul i64 {d1}, {}", st[1]));
    let idx = g.op("sci", format!("add i64 {i0}, {i1}"));
    let addr = g.op("scad", format!("add i64 {sa}, {idx}"));
    let ptr = g.op("scpt", format!("inttoptr i64 {addr} to ptr addrspace(1)"));
    let b8 = g.op("scv", format!("load i8, ptr addrspace(1) {ptr}, align 1"));
    let e = g.op("sce", format!("zext i8 {b8} to i32"));
    let bits0 = g.op("scf", format!("shl i32 {e}, 23"));
    let zero = g.op("scz", format!("icmp eq i32 {e}, 0"));
    let bits1 = g.op("scf", format!("select i1 {zero}, i32 4194304, i32 {bits0}"));
    let nan = g.op("scn", format!("icmp eq i32 {e}, 255"));
    let bits = g.op("scf", format!("select i1 {nan}, i32 2143289344, i32 {bits1}"));
    g.op("scale", format!("bitcast i32 {bits} to float"))
}

fn select_i64(g: &mut Gen, cond: &str, a: &str, b: &str) -> String {
    g.op("sel", format!("select i1 {cond}, i64 {a}, i64 {b}"))
}

fn min_i64(g: &mut Gen, a: &str, b: &str) -> String {
    let c = g.op("lt", format!("icmp slt i64 {a}, {b}"));
    select_i64(g, &c, a, b)
}

fn zero_floor(g: &mut Gen, a: &str) -> String {
    let c = g.op("neg", format!("icmp slt i64 {a}, 0"));
    select_i64(g, &c, "0", a)
}

/// Element `idx` of the operand at `base`, converted to the accumulator type.
fn load_elem(g: &mut Gen, base: &str, idx: &str, k: Kind, acc_float: bool) -> String {
    let Some(bits) = k.packed_bits() else {
        let p = g.op("ep", format!("getelementptr inbounds {}, ptr addrspace(1) {base}, i64 {idx}", k.ty()));
        let v = g.op("ev", format!("load {}, ptr addrspace(1) {p}, align {}", k.ty(), k.align()));
        return conv_in(g, &v, k, acc_float);
    };
    let bit = g.op("bit", format!("mul i64 {idx}, {bits}"));
    let byte = g.op("byte", format!("lshr i64 {bit}, 3"));
    let sh64 = g.op("sh", format!("and i64 {bit}, 7"));
    let sh = g.op("sh", format!("trunc i64 {sh64} to i32"));
    let p = g.op("ep", format!("getelementptr inbounds i8, ptr addrspace(1) {base}, i64 {byte}"));
    let b8 = g.op("eb", format!("load i8, ptr addrspace(1) {p}, align 1"));
    let b32 = g.op("eb", format!("zext i8 {b8} to i32"));
    let shifted = g.op("es", format!("lshr i32 {b32}, {sh}"));
    let x = g.op("ex", format!("and i32 {shifted}, {}", (1u32 << bits) - 1));
    if let Some((eb, mb, bias, ieee)) = k.minifloat() {
        // sign, exponent, mantissa; a normal value's float bits are assembled directly, a subnormal one is m * 2^(1-bias-mb)
        let s = g.op("fs", format!("lshr i32 {x}, {}", eb + mb));
        let e0 = g.op("fe", format!("lshr i32 {x}, {mb}"));
        let e = g.op("fe", format!("and i32 {e0}, {}", (1u32 << eb) - 1));
        let m = g.op("fm", format!("and i32 {x}, {}", (1u32 << mb) - 1));
        let sb = g.op("fsb", format!("shl i32 {s}, 31"));
        let eadj = g.op("fea", format!("add i32 {e}, {}", 127 - bias));
        let emax = (1u32 << eb) - 1;
        let special = g.op("fsp", format!("icmp eq i32 {e}, {emax}"));
        let ef = if ieee { g.op("fef", format!("select i1 {special}, i32 255, i32 {eadj}")) } else { eadj };
        let eb_ = g.op("feb", format!("shl i32 {ef}, 23"));
        let mbits = g.op("fmb", format!("shl i32 {m}, {}", 23 - mb));
        let nb0 = g.op("fnb", format!("or i32 {sb}, {eb_}"));
        let nb = g.op("fnb", format!("or i32 {nb0}, {mbits}"));
        let normal = g.op("fn", format!("bitcast i32 {nb} to float"));
        let mf = g.op("fmf", format!("uitofp i32 {m} to float"));
        let scale = 2f64.powi(1 - bias - mb as i32);
        let sub = g.op("fsub", format!("fmul float {mf}, {}", llvm_float(scale)));
        let neg = g.op("fng", format!("fsub float -0.000000e+00, {sub}"));
        let sneg = g.op("fsn", format!("icmp ne i32 {s}, 0"));
        let subs = g.op("fss", format!("select i1 {sneg}, float {neg}, float {sub}"));
        let isz = g.op("fz", format!("icmp eq i32 {e}, 0"));
        let v = g.op("fv", format!("select i1 {isz}, float {subs}, float {normal}"));
        return if acc_float { v } else { g.op("cv", format!("fptosi float {v} to i32")) };
    }
    // a signed packed integer is sign-extended from its own width
    let v = if k.signed() {
        let up = g.op("eu", format!("shl i32 {x}, {}", 32 - bits));
        g.op("ei", format!("ashr i32 {up}, {}", 32 - bits))
    } else {
        x
    };
    if acc_float { g.op("cv", format!("sitofp i32 {v} to float")) } else { v }
}

/// A float as an LLVM IR constant (hexadecimal double, exact for any float).
fn llvm_float(v: f64) -> String {
    format!("0x{:016X}", v.to_bits())
}

fn conv_in(g: &mut Gen, v: &str, k: Kind, acc_float: bool) -> String {
    if acc_float {
        match k {
            Kind::F32 => v.to_string(),
            Kind::F16 => g.op("ext", format!("fpext half {v} to float")),
            Kind::B16 => {
                // bfloat16 is the upper half of a float
                let w = g.op("bw", format!("zext i16 {v} to i32"));
                let u = g.op("bu", format!("shl i32 {w}, 16"));
                g.op("bf", format!("bitcast i32 {u} to float"))
            }
            _ if k.signed() => g.op("cv", format!("sitofp {} {v} to float", k.ty())),
            _ => g.op("cv", format!("uitofp {} {v} to float", k.ty())),
        }
    } else {
        match k {
            Kind::I32 | Kind::U32 => v.to_string(),
            _ if k.signed() => g.op("cv", format!("sext {} {v} to i32", k.ty())),
            _ => g.op("cv", format!("zext {} {v} to i32", k.ty())),
        }
    }
}

fn generate_run(combo: &RunCombo) -> Option<String> {
    let [kl, kr, kd] = kinds_of(&combo.name)?;
    let acc_float = kd.is_float();
    if !acc_float && (kl.is_float() || kr.is_float()) {
        return None;
    }
    let acc_ty = if acc_float { "float" } else { "i32" };
    let mut g = Gen { s: String::new(), n: 0 };
    // Descriptor type: the translator spells it in the declaration; any pointer type works for an opaque pointer.
    let params = if combo.v2 {
        "ptr %desc, ptr %left, i32 %lt, i32 %lsd, i32 %lb0, i32 %lb1, i32 %lse0, i32 %lse1, \
         ptr %right, i32 %rt, i32 %rsd, i32 %rb0, i32 %rb1, i32 %rse0, i32 %rse1, ptr %dest, i32 %dt, i32 %dse0, i32 %dse1, \
         i32 %threads"
    } else {
        "ptr %desc, ptr %left, i32 %lt, ptr %right, i32 %rt, ptr %dest, i32 %dt, i32 %threads"
    };
    g.s.push_str(&format!("\ndefine internal void @{}({params}) {{\nentry:\n", combo.symbol()));
    // descriptor: m, n, k, transpose_left (i8), transpose_right (i8), relaxed (i8), mode (i32 at 16)
    let dty = combo.desc_ty.clone();
    let rd32 = |g: &mut Gen, field: u32, hint: &str| {
        let p = g.op(&format!("{hint}p"), format!("getelementptr inbounds {dty}, ptr %desc, i64 0, i32 {field}"));
        g.op(hint, format!("load i32, ptr {p}, align 4"))
    };
    let rd8 = |g: &mut Gen, field: u32, hint: &str| {
        let p = g.op(&format!("{hint}p"), format!("getelementptr inbounds {dty}, ptr %desc, i64 0, i32 {field}"));
        let v = g.op(&format!("{hint}v"), format!("load i8, ptr {p}, align 1"));
        g.op(hint, format!("icmp ne i8 {v}, 0"))
    };
    let dm = rd32(&mut g, 0, "dm");
    let dn = rd32(&mut g, 1, "dn");
    let dk = rd32(&mut g, 2, "dk");
    let tl = rd8(&mut g, 3, "tl");
    let tr = rd8(&mut g, 4, "tr");
    let mode = rd32(&mut g, 6, "mode");
    let accumulate = g.op("accm", format!("icmp eq i32 {mode}, 1"));

    let a = read_operand(&mut g, "a", "%left", &combo.op_ty[0], combo.op_kind[0]);
    let b = read_operand(&mut g, "b", "%right", &combo.op_ty[1], combo.op_kind[1]);
    let c = read_operand(&mut g, "c", "%dest", &combo.op_ty[2], combo.op_kind[2]);

    // Tensor extents list dimension 0 (innermost) first. C(m, n) is C[n, m]. A(m, k) is A[k, m], or A[m, k] when
    // transposed. B(k, n) is B[n, k], or B[k, n] when transposed.
    // A: m axis dimension is 0 when transposed, else 1; k axis the other.
    let a_m_ext = select_i64(&mut g, &tl, &a.ext[0], &a.ext[1]);
    let a_k_ext = select_i64(&mut g, &tl, &a.ext[1], &a.ext[0]);
    let a_m_off = select_i64(&mut g, &tl, &a.off[0], &a.off[1]);
    let a_k_off = select_i64(&mut g, &tl, &a.off[1], &a.off[0]);
    let a_m_str = select_i64(&mut g, &tl, &a.stride[0], &a.stride[1]);
    let a_k_str = select_i64(&mut g, &tl, &a.stride[1], &a.stride[0]);
    // B: k axis dimension is 0 when transposed, else 1; n axis the other.
    let b_k_ext = select_i64(&mut g, &tr, &b.ext[0], &b.ext[1]);
    let b_n_ext = select_i64(&mut g, &tr, &b.ext[1], &b.ext[0]);
    let b_k_off = select_i64(&mut g, &tr, &b.off[0], &b.off[1]);
    let b_n_off = select_i64(&mut g, &tr, &b.off[1], &b.off[0]);
    let b_k_str = select_i64(&mut g, &tr, &b.stride[0], &b.stride[1]);
    let b_n_str = select_i64(&mut g, &tr, &b.stride[1], &b.stride[0]);
    // C: n is dimension 0, m is dimension 1.
    let (c_n_ext, c_m_ext, c_n_off, c_m_off, c_n_str, c_m_str) =
        (c.ext[0].clone(), c.ext[1].clone(), c.off[0].clone(), c.off[1].clone(), c.stride[0].clone(), c.stride[1].clone());

    // Tile sizes: the descriptor's when static, the tensors' remaining extents when dynamic or when the tensors end first.
    let big = "2147483647";
    let tile = |g: &mut Gen, d: &str| -> String {
        let x = g.op("dx", format!("sext i32 {d} to i64"));
        let st = g.op("dst", format!("icmp sgt i64 {x}, 0"));
        select_i64(g, &st, &x, big)
    };
    let tm = tile(&mut g, &dm);
    let tn = tile(&mut g, &dn);
    let tk = tile(&mut g, &dk);
    let rem = |g: &mut Gen, ext: &str, off: &str| -> String {
        let r = g.op("rem", format!("sub i64 {ext}, {off}"));
        zero_floor(g, &r)
    };
    let (r1, r2) = (rem(&mut g, &c_m_ext, &c_m_off), rem(&mut g, &a_m_ext, &a_m_off));
    let me0 = min_i64(&mut g, &tm, &r1);
    let me = min_i64(&mut g, &me0, &r2);
    let (r4, r5) = (rem(&mut g, &c_n_ext, &c_n_off), rem(&mut g, &b_n_ext, &b_n_off));
    let ne0 = min_i64(&mut g, &tn, &r4);
    let ne = min_i64(&mut g, &ne0, &r5);
    let (r6, r7) = (rem(&mut g, &a_k_ext, &a_k_off), rem(&mut g, &b_k_ext, &b_k_off));
    let ke0 = min_i64(&mut g, &tk, &r6);
    let ke = min_i64(&mut g, &ke0, &r7);
    let total = g.op("total", format!("mul i64 {me}, {ne}"));

    // base pointers
    let ap = g.op("abp", format!("inttoptr i64 {} to ptr addrspace(1)", a.base));
    let bp = g.op("bbp", format!("inttoptr i64 {} to ptr addrspace(1)", b.base));
    let cp = g.op("cbp", format!("inttoptr i64 {} to ptr addrspace(1)", c.base));

    // lane within the simdgroup: the exclusive prefix sum of one is the lane index
    let lf = g.op("lf", "call float @air.simd_prefix_exclusive_sum.f32(float 1.000000e+00)".to_string());
    let lane32 = g.op("lane32", format!("fptosi float {lf} to i32"));
    let lane = g.op("lane", format!("sext i32 {lane32} to i64"));
    let pos = g.op("tpos", "icmp sgt i32 %threads, 0".to_string());
    let t1 = g.op("t1", "select i1 ".to_string() + &format!("{pos}, i32 %threads, i32 1"));
    let step = g.op("step", format!("sext i32 {t1} to i64"));
    g.line("br label %outer");

    g.s.push_str("outer:\n");
    g.line(format!("%idx = phi i64 [ {lane}, %entry ], [ %idx.next, %store ]"));
    g.line(format!("%oc = icmp slt i64 %idx, {total}"));
    g.line("br i1 %oc, label %body, label %done");

    g.s.push_str("body:\n");
    g.line(format!("%m = sdiv i64 %idx, {ne}"));
    g.line(format!("%n = srem i64 %idx, {ne}"));
    g.line(format!("%am0 = add i64 {a_m_off}, %m"));
    g.line(format!("%am = mul i64 %am0, {a_m_str}"));
    g.line(format!("%bn0 = add i64 {b_n_off}, %n"));
    g.line(format!("%bn = mul i64 %bn0, {b_n_str}"));
    g.line("br label %kloop");

    g.s.push_str("kloop:\n");
    g.line("%k = phi i64 [ 0, %body ], [ %k.next, %kbody ]");
    let zero = if acc_float { "0.000000e+00" } else { "0" };
    g.line(format!("%acc = phi {acc_ty} [ {zero}, %body ], [ %acc.next, %kbody ]"));
    g.line(format!("%kc = icmp slt i64 %k, {ke}"));
    g.line("br i1 %kc, label %kbody, label %kdone");

    g.s.push_str("kbody:\n");
    g.line(format!("%ak0 = add i64 {a_k_off}, %k"));
    g.line(format!("%ak = mul i64 %ak0, {a_k_str}"));
    g.line("%ai = add i64 %am, %ak");

    g.line(format!("%bk0 = add i64 {b_k_off}, %k"));
    g.line(format!("%bk = mul i64 %bk0, {b_k_str}"));
    g.line("%bi = add i64 %bk, %bn");
    let mut af = load_elem(&mut g, &ap, "%ai", kl, acc_float);
    let mut bf = load_elem(&mut g, &bp, "%bi", kr, acc_float);
    // block scales: A's dimension 0 is k (m when transposed), B's is n (k when transposed)
    if combo.scaled[0] {
        let d0 = select_i64(&mut g, &tl, "%am0", "%ak0");
        let d1 = select_i64(&mut g, &tl, "%ak0", "%am0");
        let sc = scale_of(&mut g, &a, &d0, &d1);
        af = g.op("asc", format!("fmul float {af}, {sc}"));
    }
    if combo.scaled[1] {
        let d0 = select_i64(&mut g, &tr, "%bk0", "%bn0");
        let d1 = select_i64(&mut g, &tr, "%bn0", "%bk0");
        let sc = scale_of(&mut g, &b, &d0, &d1);
        bf = g.op("bsc", format!("fmul float {bf}, {sc}"));
    }
    if acc_float {
        g.line(format!("%prod = fmul float {af}, {bf}"));
        g.line("%acc.next = fadd float %acc, %prod");
    } else {
        g.line(format!("%prod = mul i32 {af}, {bf}"));
        g.line("%acc.next = add i32 %acc, %prod");
    }
    g.line("%k.next = add i64 %k, 1");
    g.line("br label %kloop");

    g.s.push_str("kdone:\n");
    g.line(format!("%cm0 = add i64 {c_m_off}, %m"));
    g.line(format!("%cm = mul i64 %cm0, {c_m_str}"));
    g.line(format!("%cn0 = add i64 {c_n_off}, %n"));
    g.line(format!("%cn = mul i64 %cn0, {c_n_str}"));
    g.line("%ci = add i64 %cm, %cn");
    g.line(format!("%cep = getelementptr inbounds {}, ptr addrspace(1) {cp}, i64 %ci", kd.ty()));
    g.line(format!("%old = load {}, ptr addrspace(1) %cep, align {}", kd.ty(), kd.align()));
    let oldf = conv_in(&mut g, "%old", kd, acc_float);
    let summed = if acc_float {
        g.op("sum", format!("fadd float %acc, {oldf}"))
    } else {
        g.op("sum", format!("add i32 %acc, {oldf}"))
    };
    let res = g.op("res", format!("select i1 {accumulate}, {acc_ty} {summed}, {acc_ty} %acc"));
    let out_v = match kd {
        Kind::F32 | Kind::I32 | Kind::U32 => res,
        Kind::F16 => g.op("trunc", format!("fptrunc float {res} to half")),
        Kind::I8 | Kind::U8 | Kind::I16 | Kind::U16 => g.op("trunc", format!("trunc i32 {res} to {}", kd.ty())),
        Kind::B16 => {
            // float to bfloat16, rounding to nearest even; a NaN stays a NaN
            let w = g.op("rw", format!("bitcast float {res} to i32"));
            let lsb0 = g.op("rl", format!("lshr i32 {w}, 16"));
            let lsb = g.op("rl", format!("and i32 {lsb0}, 1"));
            let bias = g.op("rb", format!("add i32 {lsb}, 32767"));
            let r = g.op("rr", format!("add i32 {w}, {bias}"));
            let isnan = g.op("rn", format!("fcmp uno float {res}, 0.000000e+00"));
            let q = g.op("rq", format!("or i32 {w}, 4194304"));
            let pick = g.op("rp", format!("select i1 {isnan}, i32 {q}, i32 {r}"));
            let hi = g.op("rh", format!("lshr i32 {pick}, 16"));
            g.op("trunc", format!("trunc i32 {hi} to i16"))
        }
        // a packed destination is not one of MetalPerformancePrimitives' combinations
        _ => return None,
    };
    g.line(format!("store {} {out_v}, ptr addrspace(1) %cep, align {}", kd.ty(), kd.align()));
    g.line("br label %store");

    g.s.push_str("store:\n");
    g.line("%idx.next = add i64 %idx, " .to_string() + &step);
    g.line("br label %outer");

    g.s.push_str("done:\n");
    // every lane may read the tile afterwards, so the writes are made visible across the simdgroup
    g.line("call void @air.simdgroup.barrier(i32 1, i32 5, i32 4)");
    g.line("ret void");
    g.s.push_str("}\n");
    Some(g.s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENT: &str = "  %10 = tail call i32 @air.get_extent_global_tensor.i32(ptr addrspace(1) readonly captures(none) %0, i16 3, i16 2, i16 0) #4, !alias.scope !24\n";

    #[test]
    fn a_module_without_tensors_is_returned_unchanged() {
        let ll = "define void @k() {\nentry:\n  ret void\n}\n";
        assert!(matches!(lower_tensors(ll), Cow::Borrowed(v) if std::ptr::eq(v, ll)));
    }

    #[test]
    fn an_extent_query_becomes_a_load_from_the_descriptor() {
        let out = lower_tensors(EXTENT).into_owned();
        assert!(out.contains("getelementptr inbounds %nv.tensor_t, ptr addrspace(1) %0, i64 0, i32 6, i64 2"), "{out}");
        assert!(out.contains("%10 = load i32, ptr addrspace(1) %nvt.10.p"), "{out}");
        assert!(!out.contains("@air.get_extent_global_tensor"), "{out}");
    }

    #[test]
    fn a_runtime_dimension_index_is_widened_first() {
        let ll = EXTENT.replace("i16 2, i16 0)", "i16 %46, i16 0)");
        let out = lower_tensors(&ll).into_owned();
        assert!(out.contains("%nvt.10.dim = sext i16 %46 to i64"), "{out}");
    }

    #[test]
    fn a_stride_query_reads_the_stride_array() {
        let ll = "  %11 = tail call i32 @air.get_stride_global_tensor.i32(ptr addrspace(1) %0, i16 3, i16 1, i16 0) #4\n";
        let out = lower_tensors(ll).into_owned();
        assert!(out.contains("i32 7, i64 1"), "{out}");
    }

    #[test]
    fn a_data_pointer_sums_index_times_stride_and_scales_by_the_element_size() {
        let ll = "  %21 = call ptr addrspace(1) @air.get_data_pointer_global_tensor.s.i32.global(ptr addrspace(1) readonly %0, i16 2, ptr nonnull readonly %17, i8 0, i16 0) #4\n";
        let out = lower_tensors(ll).into_owned();
        assert!(out.contains("%nvt.21.m0 = mul i64 %nvt.21.i0x, %nvt.21.sx0"), "{out}");
        assert!(out.contains("%nvt.21.m1 = mul i64 %nvt.21.i1x, %nvt.21.sx1"), "{out}");
        assert!(out.contains("%nvt.21.ob = mul i64 %nvt.21.a1, %nvt.21.e"), "{out}");
        assert!(out.contains("%nvt.21.o = lshr i64 %nvt.21.ob, 3"), "{out}");
        assert!(out.contains("%21 = inttoptr i64 %nvt.21.addr to ptr addrspace(1)"), "{out}");
    }

    #[test]
    fn a_rank_sixteen_data_pointer_walks_all_sixteen_strides() {
        let ll = "  %9 = call ptr addrspace(1) @air.get_data_pointer_global_tensor.s.i32.global(ptr addrspace(1) %0, i16 16, ptr %8) #4\n";
        let out = lower_tensors(ll).into_owned();
        assert!(out.contains("%nvt.9.m15 = mul i64"), "{out}");
        let too_big = ll.replace("i16 16", "i16 17");
        assert!(lower_tensors(&too_big).contains("@air.get_data_pointer_global_tensor"));
    }

    #[test]
    fn the_descriptor_type_matches_the_table_entry_the_driver_writes() {
        // 8 + 8 + 4 * 4 + 4 * 16 * 4 bytes
        assert_eq!(8 + 8 + 16 + 4 * 16 * 4, DESC_BYTES);
    }

    /// Every device-memory combination MetalPerformancePrimitives' matmul2d declares (SDK 27, MPPTensorOpsMatMul2dImpl.h).
    const MPP_COMBOS: &str = "b16_dv_b16_dv_b16 b16_dv_b16_dv_f32 b16_dv_f16_dv_b16 b16_dv_f16_dv_f16 b16_dv_f16_dv_f32 b16_dv_f32_dv_f32 b16_dv_i2_dv_b16 b16_dv_i2_dv_f32 b16_dv_i4_dv_b16 b16_dv_i4_dv_f32 b16_dv_i8_dv_b16 b16_dv_i8_dv_f32 b16_dv_ui2_dv_b16 b16_dv_ui2_dv_f32 b16_dv_ui4_dv_b16 b16_dv_ui4_dv_f32 b16_dv_ui8_dv_b16 b16_dv_ui8_dv_f32 f16_dv_b16_dv_b16 f16_dv_b16_dv_f16 f16_dv_b16_dv_f32 f16_dv_f16_dv_f16 f16_dv_f16_dv_f32 f16_dv_f32_dv_f32 f16_dv_fp4e2m1_dv_f16 f16_dv_fp4e2m1_dv_f32 f16_dv_fp8e4m3_dv_f16 f16_dv_fp8e4m3_dv_f32 f16_dv_fp8e5m2_dv_f16 f16_dv_fp8e5m2_dv_f32 f16_dv_i2_dv_f16 f16_dv_i2_dv_f32 f16_dv_i4_dv_f16 f16_dv_i4_dv_f32 f16_dv_i8_dv_f16 f16_dv_i8_dv_f32 f16_dv_ui2_dv_f16 f16_dv_ui2_dv_f32 f16_dv_ui4_dv_f16 f16_dv_ui4_dv_f32 f16_dv_ui8_dv_f16 f16_dv_ui8_dv_f32 f32_dv_b16_dv_f32 f32_dv_f16_dv_f32 f32_dv_f32_dv_f32 f32_dv_i8_dv_f32 f32_dv_ui8_dv_f32 fp4e2m1_dv_fp4e2m1_dv_f16 fp4e2m1_dv_fp4e2m1_dv_f32 fp8e4m3_dv_fp8e4m3_dv_f16 fp8e4m3_dv_fp8e4m3_dv_f32 fp8e5m2_dv_fp8e5m2_dv_f16 fp8e5m2_dv_fp8e5m2_dv_f32 i8_dv_b16_dv_b16 i8_dv_b16_dv_f32 i8_dv_f16_dv_f16 i8_dv_f16_dv_f32 i8_dv_f32_dv_f32 i8_dv_i2_dv_i32 i8_dv_i4_dv_i32 i8_dv_i8_dv_i32 ui8_dv_b16_dv_b16 ui8_dv_b16_dv_f32 ui8_dv_f16_dv_f16 ui8_dv_f16_dv_f32 ui8_dv_f32_dv_f32 ui8_dv_ui2_dv_i32 ui8_dv_ui4_dv_i32 ui8_dv_ui8_dv_i32";

    #[test]
    fn every_matmul2d_type_combination_has_a_generated_multiply() {
        for c in MPP_COMBOS.split(' ') {
            let name = format!("{RUN}dv_{c}_v2");
            assert!(kinds_of(&name).is_some(), "{name}");
        }
    }

    #[test]
    fn a_packed_format_element_addresses_the_byte_that_holds_it() {
        let ll = "  %9 = call ptr addrspace(1) @air.get_data_pointer_global_tensor.s.i32.global(ptr addrspace(1) %0, i16 2, ptr %6, i8 20, i16 0) #3\n";
        let out = lower_tensors(ll).into_owned();
        // bit offset = sum(index * stride) * element_bits, byte = bit offset >> 3
        assert!(out.contains("%nvt.9.ob = mul i64 %nvt.9.a1, %nvt.9.e"), "{out}");
        assert!(out.contains("%nvt.9.o = lshr i64 %nvt.9.ob, 3"), "{out}");
        assert!(!out.contains("@air.get_data_pointer_global_tensor"), "{out}");
    }

    #[test]
    fn a_constant_space_data_pointer_keeps_its_address_space() {
        let ll = "  %27 = call ptr addrspace(2) @air.get_data_pointer_global_tensor.s.i32.constant(ptr addrspace(1) %1, i16 2, ptr %24, i8 0, i16 0) #4\n";
        assert!(lower_tensors(ll).contains("inttoptr i64 %nvt.27.addr to ptr addrspace(2)"));
    }

    #[test]
    fn tensor_argument_metadata_becomes_a_descriptor_buffer() {
        let ll = "!13 = !{i32 1, !\"air.tensor\", !\"air.location_index\", i32 1, i32 1, !\"air.read\", !\"air.address_space\", i32 2, !\"air.arg_type_name\", !\"tensor<float, dextents<int, 2>>\", !\"air.arg_name\", !\"c\"}\n";
        let out = lower_tensors(ll).into_owned();
        assert!(out.contains("!\"air.buffer\""), "{out}");
        assert!(out.contains("!\"air.address_space\", i32 1"), "{out}");
        assert!(out.contains("!\"air.arg_type_size\", i32 288, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\""), "{out}");
        assert!(!out.contains("air.tensor"), "{out}");
    }

    const RUN_DECL: &str = "declare void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2(ptr noundef nonnull align 4 dereferenceable(20), ptr noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, ptr noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef, ptr noundef, i32 noundef, i32 noundef, i32 noundef, i32 noundef) local_unnamed_addr #3 section \"air.externally_defined\"\n";
    const RUN_CALL: &str = "  call void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2(ptr noundef nonnull align 4 dereferenceable(20) %4, ptr noundef nonnull %13, i32 noundef 0, i32 noundef 0, i32 noundef 0, i32 noundef 0, i32 noundef -1, i32 noundef -1, ptr noundef nonnull %14, i32 noundef 1, i32 noundef 0, i32 noundef 0, i32 noundef 0, i32 noundef -1, i32 noundef -1, ptr noundef nonnull %15, i32 noundef 0, i32 noundef -1, i32 noundef -1, i32 noundef %11) #7\n";

    const TYPES: &str = "%\"struct.mpp::tensor_ops::matmul2d_descriptor\" = type { i32, i32, i32, i8, i8, i8, i32 }\n\
        %\"struct.metal::tensor\" = type { ptr addrspace(1) }\n\
        %\"struct.metal::tensor.7\" = type { %\"struct.metal::__tensor_base.8\", ptr addrspace(1) }\n";
    const FRAME: &str = "  %4 = alloca %\"struct.mpp::tensor_ops::matmul2d_descriptor\", align 4\n\
        \x20 %5 = alloca %\"struct.metal::tensor\", align 8\n  %6 = alloca %\"struct.metal::tensor.7\", align 8\n\
        \x20 %15 = alloca %\"struct.metal::tensor\", align 8\n  %13 = bitcast ptr %5 to ptr\n  %14 = bitcast ptr %6 to ptr\n";

    fn run_module(decl: &str, call: &str) -> String {
        format!("{TYPES}{decl}define void @k() {{\nentry:\n{FRAME}{call}  ret void\n}}\n")
    }

    #[test]
    fn a_matmul_run_gets_a_defined_function_per_operand_form() {
        let ll = run_module(RUN_DECL, RUN_CALL);
        let out = lower_tensors(&ll).into_owned();
        assert!(out.contains("call void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2.v0("), "{out}");
        assert!(out.contains("define internal void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2.v0("), "{out}");
        assert!(out.contains("getelementptr inbounds %\"struct.metal::tensor.7\", ptr %right, i64 0, i32 1"), "{out}");
        assert!(!out.contains("section \"air.externally_defined\""), "{out}");
        assert!(out.contains("fmul float"), "{out}");
        assert!(out.contains("declare float @air.simd_prefix_exclusive_sum.f32(float)"), "{out}");
    }

    #[test]
    fn two_calls_with_the_same_operand_forms_share_one_function() {
        let ll = run_module(RUN_DECL, &format!("{RUN_CALL}{RUN_CALL}"));
        let out = lower_tensors(&ll).into_owned();
        assert_eq!(out.matches("define internal void").count(), 1, "{out}");
        assert_eq!(out.matches("call void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32_v2.v0(").count(), 2, "{out}");
    }

    #[test]
    fn an_unrecognised_run_keeps_its_declaration_so_the_translator_names_it() {
        let call = RUN_CALL.replace("dv_f16_dv_f16_dv_f32_v2", "dv_f16_tg_f16_dv_f32_v2");
        let decl = RUN_DECL.replace("dv_f16_dv_f16_dv_f32_v2", "dv_f16_tg_f16_dv_f32_v2");
        let ll = run_module(&decl, &call);
        let out = lower_tensors(&ll).into_owned();
        assert!(out.contains(&decl), "{out}");
    }

    #[test]
    fn an_operand_whose_kind_disagrees_with_its_struct_is_left_alone() {
        // the call says the right operand is a plain handle (0) but the struct carries offsets
        let call = RUN_CALL.replacen("ptr noundef nonnull %14, i32 noundef 1", "ptr noundef nonnull %14, i32 noundef 0", 1);
        let out = lower_tensors(&run_module(RUN_DECL, &call)).into_owned();
        assert!(out.contains(RUN_DECL), "{out}");
    }

    #[test]
    fn an_integer_matmul_accumulates_in_i32() {
        let n = "dv_i8_dv_i8_dv_i32_v2";
        let ll = run_module(&RUN_DECL.replace("dv_f16_dv_f16_dv_f32_v2", n), &RUN_CALL.replace("dv_f16_dv_f16_dv_f32_v2", n));
        let out = lower_tensors(&ll).into_owned();
        assert!(out.contains("sext i8"), "{out}");
        assert!(out.contains("%acc = phi i32"), "{out}");
    }

    const OLD_DECL: &str = "declare void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32(ptr noundef nonnull align 4 dereferenceable(20), ptr noundef, i32 noundef, ptr noundef, i32 noundef, ptr noundef, i32 noundef, i32 noundef) local_unnamed_addr #1 section \"air.externally_defined\"\n";
    const OLD_CALL: &str = "  call void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32(ptr noundef nonnull align 4 dereferenceable(20) %4, ptr noundef nonnull %13, i32 noundef 0, ptr noundef nonnull %14, i32 noundef 1, ptr noundef nonnull %15, i32 noundef 0, i32 noundef %11) #7\n";

    #[test]
    fn the_air_2_8_matmul_form_without_scales_is_lowered_too() {
        let out = lower_tensors(&run_module(OLD_DECL, OLD_CALL)).into_owned();
        assert!(out.contains("call void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32.v0("), "{out}");
        assert!(out.contains("define internal void @__tensorops_impl_matmul2d_op_run_dv_f16_dv_f16_dv_f32.v0(ptr %desc, ptr %left, i32 %lt, ptr %right"), "{out}");
        assert!(out.contains("getelementptr inbounds %\"struct.metal::tensor.7\", ptr %right, i64 0, i32 1"), "{out}");
    }

    #[test]
    fn the_two_forms_never_share_a_function() {
        let both = run_module(&format!("{RUN_DECL}{OLD_DECL}"), &format!("{RUN_CALL}{OLD_CALL}"));
        let out = lower_tensors(&both).into_owned();
        assert_eq!(out.matches("define internal void").count(), 2, "{out}");
    }

    #[test]
    fn the_air_2_8_queries_have_no_trailing_operands() {
        let extent = "  %5 = tail call i32 @air.get_extent_global_tensor.i32(ptr addrspace(1) readonly %0, i16 2, i16 1) #4\n";
        assert!(lower_tensors(extent).contains("i32 6, i64 1"));
        let data = "  %21 = call ptr addrspace(1) @air.get_data_pointer_global_tensor.s.i32.global(ptr addrspace(1) readonly %0, i16 2, ptr nonnull readonly %18) #6\n";
        assert!(lower_tensors(data).contains("%21 = inttoptr i64"));
    }

    /// Writes the lowered text of the file named by NV_TENSOR_IN to NV_TENSOR_OUT, so a harness can compile the plain
    /// IR for a CPU and run it against a reference. Ignored unless asked for.
    #[test]
    #[ignore]
    fn dump_lowered_text_for_a_cpu_harness() {
        let (Ok(i), Ok(o)) = (std::env::var("NV_TENSOR_IN"), std::env::var("NV_TENSOR_OUT")) else { return };
        let src = std::fs::read_to_string(i).expect("read input");
        std::fs::write(o, lower_tensors(&src).as_ref()).expect("write output");
    }
}
