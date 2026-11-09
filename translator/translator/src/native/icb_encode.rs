//! GPU-side indirect command buffer encoding: `air.*_render_command` and `air.*_compute_command` become plain stores
//! into the command's record.
//!
//! The command buffer a shader holds is the address of its record array (the plugin hands that address out as the
//! buffer's resource id). The record of command `i` starts at `base + i * RECORD_BYTES`; the layout below is the
//! contract with the plugin's executor (plugin/nvmtl_icb_record.h repeats it and both sides test the same table).
//!
//! Only commands the executor can replay are lowered; every other `_command` intrinsic stays as it was and is refused
//! by the translator, by name.

use super::air_text::{call_arguments, split_args, LineBuffer};

pub(crate) const RECORD_BYTES: u64 = 576;
pub(crate) const OFF_FLAGS: u64 = 0;
pub(crate) const OFF_TYPE: u64 = 4;
pub(crate) const OFF_COUNT: u64 = 8;
pub(crate) const OFF_INSTANCES: u64 = 12;
pub(crate) const OFF_START: u64 = 16;
pub(crate) const OFF_BASE_INSTANCE: u64 = 20;
pub(crate) const OFF_BASE_VERTEX: u64 = 24;
pub(crate) const OFF_PSO: u64 = 32;
pub(crate) const OFF_INDEX: u64 = 40;
pub(crate) const OFF_GRID: u64 = 48;
pub(crate) const OFF_THREADGROUP: u64 = 64;
pub(crate) const OFF_BUFFERS: u64 = 80;
pub(crate) const OFF_FRAGMENT_BUFFERS: u64 = 328;
pub(crate) const BUFFER_SLOTS: u64 = 31;
pub(crate) const FLAG_ARMED: u32 = 1;
pub(crate) const FLAG_COMPUTE: u32 = 2;
pub(crate) const FLAG_INDEXED: u32 = 4;
pub(crate) const FLAG_INDEX32: u32 = 8;

fn is_encoder_call(trimmed: &str) -> bool {
    trimmed.contains("call ") && (trimmed.contains("_render_command") || trimmed.contains("_compute_command"))
}

fn callee(trimmed: &str) -> Option<&str> {
    let at = trimmed.find('@')?;
    let rest = &trimmed[at + 1..];
    Some(&rest[..rest.find('(')?])
}

/// the value of an argument ("ptr addrspace(1) %6" -> "%6"; "<3 x i32> <i32 4, i32 1, i32 1>" -> the vector constant)
fn value_of(arg: &str) -> &str {
    let a = arg.trim();
    if a.starts_with('<') {
        let ty_end = a.find('>').map(|i| i + 1).unwrap_or(0);
        return a[ty_end..].trim();
    }
    a.rsplit(' ').next().unwrap_or("")
}
fn type_of(arg: &str) -> &str {
    let a = arg.trim();
    if a.starts_with('<') {
        return &a[..a.find('>').map(|i| i + 1).unwrap_or(0)];
    }
    a.rsplit_once(' ').map(|(t, _)| t).unwrap_or("")
}

struct Emit<'a> {
    out: &'a mut LineBuffer,
    indent: &'a str,
    n: usize,
    k: usize,
}
impl Emit<'_> {
    fn name(&mut self) -> String {
        self.k += 1;
        format!("%icbenc{}.{}", self.n, self.k)
    }
    fn line(&mut self, s: String) {
        self.out.push_fmt(format_args!("{}{}", self.indent, s));
    }
    /// the record address of command `idx` of the buffer `cb`
    fn record(&mut self, cb: &str, idx: &str) -> String {
        let (wide, off, rec) = (self.name(), self.name(), self.name());
        self.line(format!("{wide} = zext i32 {idx} to i64"));
        self.line(format!("{off} = mul i64 {wide}, {RECORD_BYTES}"));
        self.line(format!("{rec} = getelementptr i8, ptr addrspace(1) {cb}, i64 {off}"));
        rec
    }
    fn at(&mut self, rec: &str, off: &str) -> String {
        let p = self.name();
        self.line(format!("{p} = getelementptr i8, ptr addrspace(1) {rec}, i64 {off}"));
        p
    }
    fn store32(&mut self, rec: &str, off: u64, value: &str) {
        let p = self.at(rec, &off.to_string());
        self.line(format!("store i32 {value}, ptr addrspace(1) {p}, align 4"));
    }
    fn store64(&mut self, rec: &str, off: u64, value: &str) {
        let p = self.at(rec, &off.to_string());
        self.line(format!("store i64 {value}, ptr addrspace(1) {p}, align 8"));
    }
    fn address(&mut self, arg: &str) -> String {
        let a = self.name();
        self.line(format!("{a} = ptrtoint {} {} to i64", type_of(arg), value_of(arg)));
        a
    }
    /// stores an address into the table at `base` for a slot that is a runtime value
    fn store_slot(&mut self, rec: &str, base: u64, slot: &str, addr: &str) {
        let (wide, ix, off, p) = (self.name(), self.name(), self.name(), self.name());
        self.line(format!("{wide} = zext i32 {slot} to i64"));
        self.line(format!("{ix} = urem i64 {wide}, {BUFFER_SLOTS}"));
        self.line(format!("{off} = shl i64 {ix}, 3"));
        let q = self.name();
        self.line(format!("{q} = add i64 {off}, {base}"));
        self.line(format!("{p} = getelementptr i8, ptr addrspace(1) {rec}, i64 {q}"));
        self.line(format!("store i64 {addr}, ptr addrspace(1) {p}, align 8"));
    }
    fn lane(&mut self, vec: &str, k: u32) -> String {
        let v = self.name();
        self.line(format!("{v} = extractelement <3 x i32> {vec}, i32 {k}"));
        v
    }
}

/// Lowers every supported encoder call; returns None when there is nothing to do.
pub(crate) fn lower_command_encoders(san_ll: &str) -> std::borrow::Cow<'_, str> {
    if !san_ll.lines().any(|l| is_encoder_call(l.trim_start())) {
        return std::borrow::Cow::Borrowed(san_ll);
    }
    let mut out = LineBuffer::with_capacity(san_ll.len() + san_ll.len() / 2);
    let mut n = 0usize;
    for line in san_ll.lines() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        if !is_encoder_call(trimmed) {
            out.push(line);
            continue;
        }
        let (Some(name), Some(args)) = (callee(trimmed), call_arguments(trimmed)) else {
            out.push(line);
            continue;
        };
        let a = split_args(args);
        let v = |i: usize| a.get(i).map(|s| value_of(s).to_string());
        n += 1;
        let mut e = Emit { out: &mut out, indent, n, k: 0 };
        let (Some(cb), Some(idx)) = (v(0), v(1)) else {
            e.out.push(line);
            continue;
        };
        let base = name.split('.').take(2).collect::<Vec<_>>().join(".");
        match (base.as_str(), a.len()) {
            ("air.set_pipeline_state_render_command", 3) | ("air.set_pipeline_state_compute_command", 3) => {
                let rec = e.record(&cb, &idx);
                let p = e.address(&a[2]);
                e.store64(&rec, OFF_PSO, &p);
            }
            ("air.set_vertex_buffer_render_command", 5) | ("air.set_kernel_buffer_compute_command", 5) => {
                let rec = e.record(&cb, &idx);
                let p = e.address(&a[2]);
                e.store_slot(&rec, OFF_BUFFERS, &v(4).unwrap(), &p);
            }
            ("air.set_fragment_buffer_render_command", 4) => {
                let rec = e.record(&cb, &idx);
                let p = e.address(&a[2]);
                e.store_slot(&rec, OFF_FRAGMENT_BUFFERS, &v(3).unwrap(), &p);
            }
            ("air.draw_primitives_render_command", 7) => {
                let rec = e.record(&cb, &idx);
                e.store32(&rec, OFF_TYPE, &v(2).unwrap());
                e.store32(&rec, OFF_START, &v(3).unwrap());
                e.store32(&rec, OFF_COUNT, &v(4).unwrap());
                e.store32(&rec, OFF_INSTANCES, &v(5).unwrap());
                e.store32(&rec, OFF_BASE_INSTANCE, &v(6).unwrap());
                e.store32(&rec, OFF_FLAGS, &FLAG_ARMED.to_string());
            }
            ("air.draw_indexed_primitives_render_command", 8) => {
                let rec = e.record(&cb, &idx);
                e.store32(&rec, OFF_TYPE, &v(2).unwrap());
                e.store32(&rec, OFF_COUNT, &v(3).unwrap());
                let ib = e.address(&a[4]);
                e.store64(&rec, OFF_INDEX, &ib);
                e.store32(&rec, OFF_INSTANCES, &v(5).unwrap());
                e.store32(&rec, OFF_BASE_VERTEX, &v(6).unwrap());
                e.store32(&rec, OFF_BASE_INSTANCE, &v(7).unwrap());
                let flags = FLAG_ARMED | FLAG_INDEXED | if name.ends_with("i32") { FLAG_INDEX32 } else { 0 };
                e.store32(&rec, OFF_FLAGS, &flags.to_string());
            }
            ("air.concurrent_dispatch_threadgroups_compute_command", 4) | ("air.serial_dispatch_threadgroups_compute_command", 4) => {
                let rec = e.record(&cb, &idx);
                let (g, t) = (value_of(&a[2]).to_string(), value_of(&a[3]).to_string());
                for k in 0..3u32 {
                    let (gl, tl) = (e.lane(&g, k), e.lane(&t, k));
                    e.store32(&rec, OFF_GRID + 4 * k as u64, &gl);
                    e.store32(&rec, OFF_THREADGROUP + 4 * k as u64, &tl);
                }
                e.store32(&rec, OFF_FLAGS, &(FLAG_ARMED | FLAG_COMPUTE).to_string());
            }
            ("air.concurrent_dispatch_threads_compute_command", 4) | ("air.serial_dispatch_threads_compute_command", 4) => {
                // threadgroups = ceil(threads / threadsPerThreadgroup), per axis
                let rec = e.record(&cb, &idx);
                let (g, t) = (value_of(&a[2]).to_string(), value_of(&a[3]).to_string());
                for k in 0..3u32 {
                    let (gl, tl) = (e.lane(&g, k), e.lane(&t, k));
                    let (sum, groups) = (e.name(), e.name());
                    e.line(format!("{sum} = add i32 {gl}, {tl}"));
                    let dec = e.name();
                    e.line(format!("{dec} = sub i32 {sum}, 1"));
                    // a zero threadgroup size is an application error; clamp so the division is defined
                    let safe = e.name();
                    e.line(format!("{safe} = call i32 @llvm.umax.i32(i32 {tl}, i32 1)"));
                    e.line(format!("{groups} = udiv i32 {dec}, {safe}"));
                    e.store32(&rec, OFF_GRID + 4 * k as u64, &groups);
                    e.store32(&rec, OFF_THREADGROUP + 4 * k as u64, &tl);
                }
                e.store32(&rec, OFF_FLAGS, &(FLAG_ARMED | FLAG_COMPUTE).to_string());
            }
            ("air.reset_render_command", 2) | ("air.reset_compute_command", 2) => {
                let rec = e.record(&cb, &idx);
                e.store32(&rec, OFF_FLAGS, "0");
            }
            ("air.set_barrier_compute_command", _) => {} // the executor orders its dispatches
            _ => e.out.push(line),
        }
    }
    let mut text = out.text;
    if san_ll.ends_with('\n') {
        text.push('\n');
    }
    if text.contains("@llvm.umax.i32") && !text.contains("declare i32 @llvm.umax.i32") {
        text.push_str("\ndeclare i32 @llvm.umax.i32(i32, i32)\n");
    }
    std::borrow::Cow::Owned(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERNEL: &str = r#"
define void @encode(ptr addrspace(1) %cb, ptr addrspace(1) %verts, ptr addrspace(1) %ib, ptr addrspace(1) %pso, i32 %i) {
entry:
  tail call void @air.set_pipeline_state_render_command(ptr addrspace(1) %cb, i32 %i, ptr addrspace(1) %pso)
  tail call void @air.set_vertex_buffer_render_command.p1i8(ptr addrspace(1) %cb, i32 %i, ptr addrspace(1) %verts, i64 -1, i32 0)
  tail call void @air.set_fragment_buffer_render_command.p1i8(ptr addrspace(1) %cb, i32 %i, ptr addrspace(1) %verts, i32 1)
  tail call void @air.draw_primitives_render_command(ptr addrspace(1) %cb, i32 %i, i32 3, i32 0, i32 3, i32 1, i32 %i)
  tail call void @air.draw_indexed_primitives_render_command.p1i32(ptr addrspace(1) %cb, i32 %i, i32 3, i32 6, ptr addrspace(1) %ib, i32 2, i32 0, i32 %i)
  tail call void @air.concurrent_dispatch_threadgroups_compute_command(ptr addrspace(1) %cb, i32 %i, <3 x i32> <i32 4, i32 1, i32 1>, <3 x i32> <i32 8, i32 1, i32 1>)
  tail call void @air.concurrent_dispatch_threads_compute_command(ptr addrspace(1) %cb, i32 %i, <3 x i32> <i32 100, i32 1, i32 1>, <3 x i32> <i32 8, i32 1, i32 1>)
  tail call void @air.reset_render_command(ptr addrspace(1) %cb, i32 %i)
  ret void
}
declare void @air.set_pipeline_state_render_command(ptr addrspace(1), i32, ptr addrspace(1))
declare void @air.set_vertex_buffer_render_command.p1i8(ptr addrspace(1), i32, ptr addrspace(1), i64, i32)
declare void @air.set_fragment_buffer_render_command.p1i8(ptr addrspace(1), i32, ptr addrspace(1), i32)
declare void @air.draw_primitives_render_command(ptr addrspace(1), i32, i32, i32, i32, i32, i32)
declare void @air.draw_indexed_primitives_render_command.p1i32(ptr addrspace(1), i32, i32, i32, ptr addrspace(1), i32, i32, i32)
declare void @air.concurrent_dispatch_threadgroups_compute_command(ptr addrspace(1), i32, <3 x i32>, <3 x i32>)
declare void @air.concurrent_dispatch_threads_compute_command(ptr addrspace(1), i32, <3 x i32>, <3 x i32>)
declare void @air.reset_render_command(ptr addrspace(1), i32)
"#;

    #[test]
    fn a_module_without_encoder_calls_is_borrowed() {
        let ll = "define void @k() {\nentry:\n  ret void\n}\n";
        assert!(matches!(lower_command_encoders(ll), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn every_supported_encoder_call_becomes_stores_and_no_call_remains() {
        let out = lower_command_encoders(KERNEL);
        assert!(!out.lines().any(|l| l.trim_start().starts_with("tail call void @air.") ), "a call survived:\n{out}");
        // the draw writes its counts at the layout's offsets from the record
        for off in [OFF_TYPE, OFF_START, OFF_COUNT, OFF_INSTANCES, OFF_BASE_INSTANCE, OFF_BASE_VERTEX, OFF_PSO, OFF_INDEX, OFF_GRID, OFF_THREADGROUP] {
            assert!(out.contains(&format!("i64 {off}")), "no store at offset {off}:\n{out}");
        }
        assert!(out.contains(&format!("mul i64 %icbenc1.1, {RECORD_BYTES}")), "slot addressing is idx * record size");
        assert!(out.contains(&format!("store i32 {}, ptr addrspace(1)", FLAG_ARMED | FLAG_INDEXED | FLAG_INDEX32)), "indexed 32-bit flags");
        assert!(out.contains(&format!("store i32 {}, ptr addrspace(1)", FLAG_ARMED | FLAG_COMPUTE)), "compute flags");
        assert!(out.contains("udiv i32"), "dispatch threads rounds up");
        assert!(out.contains("declare i32 @llvm.umax.i32"), "the clamp's declaration is added");
    }

    #[test]
    fn an_unsupported_command_is_left_for_the_translator_to_refuse() {
        let ll = KERNEL.replace(
            "  ret void",
            "  tail call void @air.set_object_buffer_render_command.p1i8(ptr addrspace(1) %cb, i32 %i, ptr addrspace(1) %verts, i32 2)\n  ret void",
        );
        let out = lower_command_encoders(&ll);
        assert!(out.contains("call void @air.set_object_buffer_render_command.p1i8"), "{out}");
    }

    #[test]
    fn the_record_layout_has_no_overlaps_and_fits() {
        let mut spans = vec![
            (OFF_FLAGS, 4), (OFF_TYPE, 4), (OFF_COUNT, 4), (OFF_INSTANCES, 4), (OFF_START, 4), (OFF_BASE_INSTANCE, 4),
            (OFF_BASE_VERTEX, 4), (OFF_PSO, 8), (OFF_INDEX, 8), (OFF_GRID, 12), (OFF_THREADGROUP, 12),
            (OFF_BUFFERS, 8 * BUFFER_SLOTS), (OFF_FRAGMENT_BUFFERS, 8 * BUFFER_SLOTS),
        ];
        spans.sort();
        for w in spans.windows(2) { assert!(w[0].0 + w[0].1 <= w[1].0, "{w:?} overlap"); }
        let last = spans.last().unwrap();
        assert!(last.0 + last.1 <= RECORD_BYTES);
        assert_eq!(RECORD_BYTES % 8, 0);
    }

    #[test]
    fn the_lowered_encoder_kernel_translates_to_valid_spirv() {
        // lowered once on its own before the full pipeline below; the result itself is not inspected
        let _lowered = lower_command_encoders(KERNEL).into_owned();
        let tmp = std::env::temp_dir().join(format!("icb_encode_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        // the full pipeline, which validates its own output: the encoder kernel with its buffers as plain arguments
        let ll = format!("{KERNEL}\n!air.kernel = !{{!0}}\n!0 = !{{ptr @encode, !1, !2}}\n!1 = !{{}}\n!2 = !{{!3, !4, !5, !6, !7}}\n\
            !3 = !{{i32 0, !\"air.buffer\", !\"air.location_index\", i32 0, i32 1, !\"air.read_write\", !\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 8, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\", !\"uchar\", !\"air.arg_name\", !\"cb\"}}\n\
            !4 = !{{i32 1, !\"air.buffer\", !\"air.location_index\", i32 1, i32 1, !\"air.read\", !\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 4, !\"air.arg_type_align_size\", i32 4, !\"air.arg_type_name\", !\"float\", !\"air.arg_name\", !\"verts\"}}\n\
            !5 = !{{i32 2, !\"air.buffer\", !\"air.location_index\", i32 2, i32 1, !\"air.read\", !\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 4, !\"air.arg_type_align_size\", i32 4, !\"air.arg_type_name\", !\"uint\", !\"air.arg_name\", !\"ib\"}}\n\
            !6 = !{{i32 3, !\"air.buffer\", !\"air.location_index\", i32 3, i32 1, !\"air.read\", !\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 8, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\", !\"uchar\", !\"air.arg_name\", !\"pso\"}}\n\
            !7 = !{{i32 4, !\"air.thread_position_in_grid\", !\"air.arg_type_name\", !\"uint\", !\"air.arg_name\", !\"i\"}}\n");
        let lowered_probe = lower_command_encoders(&ll);
        assert!(!lowered_probe.contains("tail call void @air.draw_primitives_render_command"));
        let bytes = crate::translate_sanitized_native(&ll, crate::passes::Stage::Kernel, &tmp).unwrap_or_else(|e| panic!("translate: {e}"));
        crate::tools::spirv_val_bytes(&bytes, &tmp).expect("spirv-val");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
