use metal2vulkan::passes::Stage;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, Ordering};

static CAPS: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
pub extern "C" fn nvmtl_translate_set_caps(caps: u32) { CAPS.store(caps, Ordering::Relaxed); }

const NVMTL_TG_MAX: usize = 32;
thread_local! {
    static TG_LEN: std::cell::RefCell<[u32; NVMTL_TG_MAX]> = const {
        std::cell::RefCell::new([0; NVMTL_TG_MAX])
    };
}

#[no_mangle]
pub extern "C" fn nvmtl_translate_set_threadgroup_lengths(lens: *const u32, count: u32) {
    let n = if lens.is_null() { 0 } else { (count as usize).min(NVMTL_TG_MAX) };
    TG_LEN.with(|storage| {
        let mut lengths = storage.borrow_mut();
        lengths.fill(0);
        for i in 0..n {
            lengths[i] = unsafe { *lens.add(i) };
        }
    });
}

fn threadgroup_lengths() -> Vec<(u32, u32)> {
    TG_LEN.with(|storage| {
        storage.borrow().iter().enumerate()
            .filter_map(|(i, &v)| (v != 0).then_some((i as u32, v)))
            .collect()
    })
}

// The pipeline's raster sample count, for air.get_num_samples (fragment shaders read it; ANGLE's WebGL shaders do).
// WAS: never set by the driver, so every such shader failed to translate ("requires TransformOptions::raster_sample_count")
// and WebGL pages said the browser has no WebGL. 0 = the plugin did not say: one sample, which is every pipeline without
// MSAA. The plugin can name the real count for an MSAA pipeline with nvmtl_translate_set_raster_sample_count.
thread_local! { static RSC: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
#[no_mangle]
pub extern "C" fn nvmtl_translate_set_raster_sample_count(n: u32) { RSC.with(|c| c.set(n)); }
fn apply_rsc(mut o: metal2vulkan::passes::TransformOptions) -> metal2vulkan::passes::TransformOptions {
    if o.raster_sample_count.is_none() {
        let n = RSC.with(|c| c.get());
        o.raster_sample_count = Some(if matches!(n, 1 | 2 | 4 | 8 | 16 | 32 | 64) { n } else { 1 });
    }
    o
}

fn apply_tg(mut o: metal2vulkan::passes::TransformOptions) -> metal2vulkan::passes::TransformOptions {
    o = apply_rsc(o);
    for (index, length) in threadgroup_lengths() {
        match o.with_threadgroup_memory_length(index, length) {
            Ok(next) => o = next,
            Err(e) => { eprintln!("nvmtl_translate: threadgroup index {index} length {length} refused: {e}"); break; }
        }
    }
    o
}

fn formatless_options(ll: &str, st: Stage, caps: u32) -> Option<metal2vulkan::passes::TransformOptions> {
    use metal2vulkan::reflect::{ResourceAccess as A, ResourceKind as K};
    let refl = metal2vulkan::reflect_sanitized(ll, st, metal2vulkan::passes::TransformOptions::default()).ok()?;
    let mut idx: Vec<usize> = refl.bindings.iter().filter(|b| {
        (b.kind == K::StorageImage || (matches!(b.kind, K::TextureArray | K::EmbeddedArgBufferTexture) && b.access == Some(A::Storage)))
            && b.texture_shape.as_ref().is_some_and(|t| matches!(t.component, metal2vulkan::meta::TextureComponent::Float))
    }).filter_map(|b| usize::try_from(b.metal_index).ok()).collect();
    idx.sort_unstable(); idx.dedup();
    if idx.is_empty() { return None; }
    use metal2vulkan::reflect::{RuntimeStorageImageCapabilities as C, RuntimeStorageImageFormat as F, RuntimeStorageImageState as S};
    let mut o = metal2vulkan::passes::TransformOptions::default();
    let state = S { format: F::Bgra8Unorm, capabilities: C { storage_image: true, storage_image_atomic: false,
        read_without_format: caps & 1 != 0, write_without_format: caps & 2 != 0 } };
    for i in idx { if let Some(s) = o.runtime_storage_image_states.get_mut(i) { *s = Some(state); } }
    Some(o)
}
fn translate_one(ll: &str, st: Stage) -> Result<Vec<u8>, String> {
    translate_with_pointer_retry(ll, |ll| translate_one_as_written(ll, st))
}

// Keep the same translation options on both attempts. In particular, the
// sampler-specialized path must retain pixel coordinates and A8 outputs.
fn translate_with_pointer_retry(
    ll: &str, translate: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    match translate(ll) {
        Ok(v) => Ok(v),
        // A future loop-edge pointer load can refer to the same immutable local
        // object field as an earlier load. Before scalar replacement its source
        // is not yet emitted, so pointer-phi construction cannot recover that
        // identity. Inline the helpers and forward only fields whose sole store
        // dominates the reads and whose complete address-use graph is known.
        Err(e) if e.contains("pointer merge storage mismatch")
            || (e.contains("pointer phi") && e.contains("differing structural sources")) =>
        {
            let Some(inlined) = metal2vulkan::inline_and_forward_object_fields(ll) else { return Err(e) };
            if let Some(path) = std::env::var_os("NVMTL_DUMP_RETRY_LL") {
                let _ = std::fs::write(path, &inlined);
            }
            translate(&inlined)
                .map_err(|retry| format!("{e}; retry with immutable local pointer fields forwarded: {retry}"))
        }
        // A module whose pointers (a sampler's address in an argument buffer, a threadgroup array's address) reach their use
        // only through a local object's fields gets one retry with its helpers inlined and those fields forwarded, and
        // samplers in argument buffers read from the sampler heap. Only a failure gets here, so a module that translates as
        // written never changes.
        //
        // A sampler in an argument buffer that also holds buffer pointers ("is not a sampler"), or in an array of samplers
        // there ("exact sampler state was not recovered"), fails the same way with no helper to inline: it gets the retry
        // on its text as written, its argument-buffer samplers read from the sampler heap.
        Err(e) if e.contains("sampler operand is a pointer") || e.contains("pointer placeholder")
            || e.contains("missing pointer storage") || e.contains("sampler operand is not a sampler")
            || e.contains("runtime sampler specialization cannot recover an exact state after pointer selection")
            // ANGLE's user main left as a call takes the uniform-buffer pointer as an argument, which the emitter types
            // two ways (#406's "owned IAdd operands", #419; patch by eisda); inlined, the pointer never crosses a call
            || e.contains("owned OpFunctionCall argument type disagrees") || e.contains("owned IAdd operands do not match")
            || (e.contains("owned AccessChain") && e.contains("references undefined id")) =>
        {
            let inlined = metal2vulkan::inline_and_forward_object_fields(ll);
            if inlined.is_none() && !e.contains("sampler operand") { return Err(e) }
            let retry = inlined.as_deref().unwrap_or(ll);
            // NVMTL_DUMP_RETRY_LL=<file>: the retry's input text, to inspect what inlining and forwarding produced
            if let Some(path) = std::env::var_os("NVMTL_DUMP_RETRY_LL") {
                let _ = std::fs::write(path, retry);
            }
            let how = if inlined.is_some() { "with helpers inlined and object fields forwarded" }
                      else { "with argument-buffer samplers from the sampler heap" };
            // The call-crossing failures hold no argument-buffer sampler; in the sampler-heap mode their inlined
            // uniform-buffer read came out as a null constant (#419), so the ordinary translation goes first for them.
            let call_crossing = e.contains("owned OpFunctionCall argument type disagrees")
                || e.contains("owned IAdd operands do not match") || e.contains("references undefined id");
            if call_crossing {
                if let Some(text) = inlined.as_deref() {
                    if let Ok(v) = translate(text) { return Ok(v); }
                }
            }
            metal2vulkan::with_argument_buffer_samplers(|| translate(retry))
                // Forwarding can hand the entry's own [[sampler]] straight to the sample call, the shape Metal's inliner
                // leaves; such a module reads no sampler from an argument buffer, so it gets the ordinary translation.
                .or_else(|e2| match inlined.as_deref() {
                    Some(text) => translate(text).map_err(|e3| format!("{e2}; as ordinary samplers: {e3}")),
                    None => Err(e2),
                })
                .map_err(|e2| format!("{e}; retry {how}: {e2}"))
        }
        Err(e) => Err(e),
    }
}

fn translate_one_as_written(ll: &str, st: Stage) -> Result<Vec<u8>, String> {
    let caps = CAPS.load(Ordering::Relaxed);
    if let Some(o) = if caps != 0 { formatless_options(ll, st, caps) } else { None } {
        match metal2vulkan::translate_native_no_retry_constructed_with_options(ll, st, apply_tg(o)) {
            Ok(v) => return Ok(v),
            Err(e) => eprintln!("nvmtl_translate: formatless storage images refused, falling back: {e}"),
        }
    }
    metal2vulkan::translate_native_no_retry_constructed_with_options(ll, st, apply_tg(metal2vulkan::passes::TransformOptions::default()))
}

#[no_mangle]
pub extern "C" fn nvmtl_translate(
    ll: *const c_char, stage: *const c_char, out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let n = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, n);
            *err.add(n) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    match translate_one(ll, st) {
        Ok(spv) => {
            let mut b = spv.into_boxed_slice();
            unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
            std::mem::forget(b);
            0
        }
        Err(e) => { dump_failed(ll, st, &e); set_err(&e); -1 }
    }
}

// A kernel that fails inside an app (Geekbench's OpenCL convolve) cannot be reproduced offline without its AIR.
// NVMTL_DUMP_FAILED_DIR=<dir> writes the input and the error beside it; unset, this costs one env read on failure only.
fn dump_failed(ll: &str, st: Stage, e: &str) {
    let Some(dir) = std::env::var_os("NVMTL_DUMP_FAILED_DIR") else { return };
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ll.hash(&mut h);
    let base = std::path::Path::new(&dir).join(format!("{:016x}-{:?}", h.finish(), st));
    if let Err(w) = std::fs::write(base.with_extension("ll"), ll).and_then(|_| std::fs::write(base.with_extension("err"), e)) {
        eprintln!("nvmtl_translate: NVMTL_DUMP_FAILED_DIR write failed: {w}");
    }
}

#[no_mangle]
pub extern "C" fn nvmtl_translate_free(p: *mut u8, len: usize) {
    if !p.is_null() { unsafe { drop(Box::from_raw(std::slice::from_raw_parts_mut(p, len))); } }
}

#[no_mangle]
pub extern "C" fn nvmtl_visible_refs(
    ll: *const c_char, out: *mut *mut u8, out_len: *mut usize, err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || out.is_null() || out_len.is_null() { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let syms = match metal2vulkan::linked_functions::visible_function_reference_symbols(ll) {
        Ok(v) => v, Err(e) => { set_err(&e); return -1; }
    };
    let json = match serde_json::to_vec(&syms) { Ok(v) => v, Err(e) => { set_err(&e.to_string()); return -1; } };
    let mut b = json.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_link_visible(
    ll: *const c_char, stage: *const c_char,
    syms: *const *const c_char, mods: *const *const c_char, n: usize,
    out: *mut *mut u8, out_len: *mut usize, err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    if n > 0 && (syms.is_null() || mods.is_null()) { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    let mut linkage = metal2vulkan::linked_functions::LinkedFunctionLinkage::default();
    for i in 0..n {
        let (sp, mp) = unsafe { (*syms.add(i), *mods.add(i)) };
        if sp.is_null() || mp.is_null() { set_err("null symbol or module"); return -1; }
        let symbol = match unsafe { CStr::from_ptr(sp) }.to_str() { Ok(s) => s.to_string(), Err(_) => { set_err("symbol is not utf-8"); return -1; } };
        let module_ll = match unsafe { CStr::from_ptr(mp) }.to_str() { Ok(s) => s.to_string(), Err(_) => { set_err("module is not utf-8"); return -1; } };
        linkage.visible_references.push(metal2vulkan::linked_functions::LinkedFunctionReference { symbol, module_ll });
    }
    let linked = match metal2vulkan::specialize_linked_module(ll, st, &linkage) {
        Ok(v) => v, Err(e) => { set_err(&format!("link: {e}")); return -1; }
    };
    let mut v = linked.into_bytes(); v.push(0);
    let mut b = v.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_link_externs(
    ll: *const c_char, mods: *const *const c_char, n: usize,
    out: *mut *mut u8, out_len: *mut usize, err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || out.is_null() || out_len.is_null() || (n > 0 && mods.is_null()) { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let mut modules = Vec::with_capacity(n);
    for i in 0..n {
        let mp = unsafe { *mods.add(i) };
        if mp.is_null() { set_err("null module"); return -1; }
        match unsafe { CStr::from_ptr(mp) }.to_str() { Ok(s) => modules.push(s.to_string()), Err(_) => { set_err("module is not utf-8"); return -1; } }
    }
    let linked = match metal2vulkan::linked_functions::link_extern_definitions(ll, &modules) {
        Ok(v) => v, Err(e) => { set_err(&format!("extern link: {e}")); return -1; }
    };
    let mut v = linked.into_bytes(); v.push(0);
    let mut b = v.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_lower_mesh(
    ll: *const c_char, entry: *const c_char,
    out_k: *mut *mut u8, out_k_len: *mut usize, out_v: *mut *mut u8, out_v_len: *mut usize,
    layout: *mut u32, err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || entry.is_null() || out_k.is_null() || out_k_len.is_null() || out_v.is_null() || out_v_len.is_null() || layout.is_null() { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let entry = match unsafe { CStr::from_ptr(entry) }.to_str() { Ok(s) => s, Err(_) => { set_err("entry is not utf-8"); return -1; } };
    let m = match metal2vulkan::mesh_lower::lower_mesh(ll, entry) { Ok(m) => m, Err(e) => { set_err(&e); return -1; } };
    let give = |text: String, o: *mut *mut u8, ol: *mut usize| {
        let mut v = text.into_bytes(); v.push(0);
        let mut b = v.into_boxed_slice();
        unsafe { *o = b.as_mut_ptr(); *ol = b.len(); }
        std::mem::forget(b);
    };
    let l = &m.layout;
    for (i, x) in [l.nv, l.np, l.k, l.vs, l.ps, l.idx_off, l.block].into_iter().enumerate() { unsafe { *layout.add(i) = x; } }
    give(m.kernel_ll, out_k, out_k_len);
    give(m.vertex_ll, out_v, out_v_len);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_lower_mesh2(
    obj_ll: *const c_char, obj_entry: *const c_char, ll: *const c_char, entry: *const c_char,
    mode: u32, cap: u32,
    out_o: *mut *mut u8, out_o_len: *mut usize,
    out_k: *mut *mut u8, out_k_len: *mut usize, out_v: *mut *mut u8, out_v_len: *mut usize,
    layout: *mut u32, err: *mut c_char, err_len: usize,
) -> i32 {
    use metal2vulkan::mesh_lower::*;
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || entry.is_null() || out_k.is_null() || out_k_len.is_null() || out_v.is_null() || out_v_len.is_null() || layout.is_null() { return -1; }
    let text = |p: *const c_char, what: &str| -> Result<String, String> {
        if p.is_null() { return Err(format!("{what} is null")); }
        unsafe { CStr::from_ptr(p) }.to_str().map(|s| s.to_string()).map_err(|_| format!("{what} is not utf-8"))
    };
    let ll = match text(ll, "ll") { Ok(s) => s, Err(e) => { set_err(&e); return -1; } };
    let entry = match text(entry, "entry") { Ok(s) => s, Err(e) => { set_err(&e); return -1; } };
    let link = match mode { 0 => Link::Direct, 1 => Link::Object, 2 => Link::Indirect, _ => { set_err("mode must be 0|1|2"); return -1; } };
    if link != Link::Direct && cap == 0 { set_err("a linked mesh needs a threadgroup cap > 0"); return -1; }
    let mut obj: Option<(String, String)> = None;
    let rs = match link {
        Link::Direct => 0,
        Link::Indirect => {
            match payload_len(&ll, &entry) { Ok(0) => {}, Ok(n) => { set_err(&format!("mesh {entry} reads a {n}-byte payload but is drawn without an object stage")); return -1; }, Err(e) => { set_err(&e); return -1; } }
            record_stride(0)
        }
        Link::Object => {
            if out_o.is_null() || out_o_len.is_null() { return -1; }
            let ol = match text(obj_ll, "obj_ll") { Ok(s) => s, Err(e) => { set_err(&e); return -1; } };
            let oe = match text(obj_entry, "obj_entry") { Ok(s) => s, Err(e) => { set_err(&e); return -1; } };
            let op = match payload_len(&ol, &oe) { Ok(n) => n, Err(e) => { set_err(&e); return -1; } };
            let mp = match payload_len(&ll, &entry) { Ok(n) => n, Err(e) => { set_err(&e); return -1; } };
            if mp > op { set_err(&format!("mesh {entry} reads a {mp}-byte payload but object {oe} declares {op}")); return -1; }
            obj = Some((ol, oe));
            record_stride(op)
        }
    };
    let m = match lower_mesh_linked(&ll, &entry, link, rs, cap) { Ok(m) => m, Err(e) => { set_err(&e); return -1; } };
    let o_text = match &obj {
        Some((ol, oe)) => match lower_object(ol, oe, &m.layout, m.tr, rs) { Ok(t) => Some(t), Err(e) => { set_err(&e); return -1; } },
        None => None,
    };
    let give = |text: String, o: *mut *mut u8, ol: *mut usize| {
        let mut v = text.into_bytes(); v.push(0);
        let mut b = v.into_boxed_slice();
        unsafe { *o = b.as_mut_ptr(); *ol = b.len(); }
        std::mem::forget(b);
    };
    let l = &m.layout;
    for (i, x) in [l.nv, l.np, l.k, l.vs, l.ps, l.idx_off, l.block, m.tr, rs].into_iter().enumerate() { unsafe { *layout.add(i) = x; } }
    if let Some(t) = o_text { give(t, out_o, out_o_len); }
    give(m.kernel_ll, out_k, out_k_len);
    give(m.vertex_ll, out_v, out_v_len);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_link_runtime(
    ll: *const c_char, stage: *const c_char,
    refs: *const *const c_char, ref_mods: *const *const c_char, nref: usize,
    cands: *const *const c_char, cand_mods: *const *const c_char, ncand: usize,
    out: *mut *mut u8, out_len: *mut usize, err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    if (nref > 0 && (refs.is_null() || ref_mods.is_null())) || (ncand > 0 && (cands.is_null() || cand_mods.is_null())) { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    let pair = |a: *const *const c_char, b: *const *const c_char, i: usize| -> Result<(String, String), String> {
        let (sp, mp) = unsafe { (*a.add(i), *b.add(i)) };
        if sp.is_null() || mp.is_null() { return Err("null symbol or module".into()); }
        let s = unsafe { CStr::from_ptr(sp) }.to_str().map_err(|_| "symbol is not utf-8".to_string())?.to_string();
        let m = unsafe { CStr::from_ptr(mp) }.to_str().map_err(|_| "module is not utf-8".to_string())?.to_string();
        Ok((s, m))
    };
    let mut references = Vec::with_capacity(nref);
    for i in 0..nref {
        match pair(refs, ref_mods, i) {
            Ok((symbol, module_ll)) => references.push(metal2vulkan::linked_functions::LinkedFunctionReference { symbol, module_ll }),
            Err(e) => { set_err(&e); return -1; }
        }
    }
    let mut candidates = Vec::with_capacity(ncand);
    for i in 0..ncand {
        match pair(cands, cand_mods, i) { Ok(p) => candidates.push(p), Err(e) => { set_err(&e); return -1; } }
    }
    let linked = match metal2vulkan::specialize_runtime_table_module(ll, st, references, &candidates) {
        Ok(v) => v, Err(e) => { set_err(&format!("runtime-table link: {e}")); return -1; }
    };
    let mut v = linked.into_bytes(); v.push(0);
    let mut b = v.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_translate_fc(
    ll: *const c_char, stage: *const c_char,
    indices: *const u32, sizes: *const u32, payloads: *const *const u8, n: usize,
    out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    if n > 0 && (indices.is_null() || sizes.is_null() || payloads.is_null()) { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    let mut values: Vec<(u32, Vec<u8>)> = Vec::with_capacity(n);
    for i in 0..n {
        let idx = unsafe { *indices.add(i) };
        let len = unsafe { *sizes.add(i) } as usize;
        let ptr = unsafe { *payloads.add(i) };
        if ptr.is_null() { set_err("null payload"); return -1; }
        values.push((idx, unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()));
    }
    let specialized = match metal2vulkan::specialize_air_function_constants(ll, &values) {
        Ok(v) => v,
        Err(e) => { set_err(&format!("AIR specialization: {e}")); return -1; }
    };
    // A library may declare `[[visible]]` function references that only a function-constant-disabled
    // path calls. The pre-emit refusal is a TEXT search for `.MTL_VISIBLE_FN_REF`, so it fired on such a
    // module even once the constants had folded the caller away, and every system process that ships one
    // lost the pipeline (loginwindow, Spotlight, icon services, Dock, measured on GA102 and GB206 10-17).
    // Closing the graph against an EMPTY linkage renames each unsupplied stub off `.MTL_VISIBLE_FN_REF`,
    // so a dead reference folds away here and a LIVE one is still refused at emission, by name. That is
    // the contract batch 51 already uses for a partly supplied pipeline; a pipeline that really links
    // functions arrives through nvmtl_translate_linked and does not reach this path.
    let specialized = match metal2vulkan::linked_functions::specialize_visible_function_references(
        specialized.as_ref(),
        &metal2vulkan::linked_functions::LinkedFunctionLinkage::default(),
    ) {
        Ok(v) => v,
        Err(e) => { set_err(&format!("visible function reference: {e}")); return -1; }
    };
    match translate_one(specialized.as_str(), st) {
        Ok(spv) => {
            let mut b = spv.into_boxed_slice();
            unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
            std::mem::forget(b);
            0
        }
        Err(e) => { set_err(&e); -1 }
    }
}

#[no_mangle]
pub extern "C" fn nvmtl_reflect(
    ll: *const c_char, stage: *const c_char,
    out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    let refl = match metal2vulkan::reflect_sanitized(ll, st, metal2vulkan::passes::TransformOptions::default()) {
        Ok(r) => r,
        Err(e) => { set_err(&e); return -1; }
    };
    let json = match serde_json::to_vec(&refl) { Ok(v) => v, Err(e) => { set_err(&e.to_string()); return -1; } };
    let mut b = json.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

#[no_mangle]
pub extern "C" fn nvmtl_reflect_fc(
    ll: *const c_char, stage: *const c_char,
    indices: *const u32, sizes: *const u32, payloads: *const *const u8, n: usize,
    out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    let set_err = |msg: &str| unsafe {
        if !err.is_null() && err_len > 0 {
            let b = msg.as_bytes();
            let k = b.len().min(err_len - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), err as *mut u8, k);
            *err.add(k) = 0;
        }
    };
    if ll.is_null() || stage.is_null() || out.is_null() || out_len.is_null() { return -1; }
    if n > 0 && (indices.is_null() || sizes.is_null() || payloads.is_null()) { return -1; }
    let ll = match unsafe { CStr::from_ptr(ll) }.to_str() { Ok(s) => s, Err(_) => { set_err("ll is not utf-8"); return -1; } };
    let st = match unsafe { CStr::from_ptr(stage) }.to_str() {
        Ok("vertex") => Stage::Vertex, Ok("fragment") => Stage::Fragment, Ok("kernel") => Stage::Kernel,
        _ => { set_err("stage must be vertex|fragment|kernel"); return -1; }
    };
    let mut values: Vec<(u32, Vec<u8>)> = Vec::with_capacity(n);
    for i in 0..n {
        let idx = unsafe { *indices.add(i) };
        let len = unsafe { *sizes.add(i) } as usize;
        let ptr = unsafe { *payloads.add(i) };
        if ptr.is_null() { set_err("null payload"); return -1; }
        values.push((idx, unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()));
    }
    let specialized = match metal2vulkan::specialize_air_function_constants(ll, &values) {
        Ok(v) => v,
        Err(e) => { set_err(&format!("AIR specialization: {e}")); return -1; }
    };
    let refl = match metal2vulkan::reflect_sanitized(specialized.as_ref(), st, metal2vulkan::passes::TransformOptions::default()) {
        Ok(r) => r,
        Err(e) => { set_err(&e); return -1; }
    };
    let json = match serde_json::to_vec(&refl) { Ok(v) => v, Err(e) => { set_err(&e.to_string()); return -1; } };
    let mut b = json.into_boxed_slice();
    unsafe { *out = b.as_mut_ptr(); *out_len = b.len(); }
    std::mem::forget(b);
    0
}

mod buffer_addresses;
mod wide_loads;
mod align_guard;
mod table_ubo;
mod g2_inline;
mod linearize;
mod uniform_cbuf;

// Pipeline-local GL optimization. The caller identifies the Metal slot from AIR;
// failure leaves its already compiled physical-address pipeline available.
#[no_mangle]
pub unsafe extern "C" fn nvmtl_promote_gl_uniform(
    data: *const u8, len: usize, binding: u32, limit: u32,
    out: *mut *mut u8, out_len: *mut usize, extent: *mut u32,
    err: *mut c_char, err_len: usize,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() || extent.is_null() { return -1; }
    *out = std::ptr::null_mut(); *out_len = 0; *extent = 0;
    match uniform_cbuf::promote(std::slice::from_raw_parts(data, len), binding, limit) {
        Ok((v, size)) => {
            let mut b = v.into_boxed_slice();
            *out = b.as_mut_ptr(); *out_len = b.len(); *extent = size;
            std::mem::forget(b); 0
        }
        Err(e) => {
            if !err.is_null() && err_len > 0 {
                let n = e.len().min(err_len - 1);
                std::ptr::copy_nonoverlapping(e.as_ptr(), err.cast(), n); *err.add(n) = 0;
            }
            -1
        }
    }
}

#[cfg(test)]
mod gl_translation_tests {
    use super::*;
    use spirv_tools::val::Validator;

    #[test]
    fn minecraft_uniform_and_sampler_shaders_validate_through_address_lowering() {
        for ll in [
            include_str!("../../translator/tests/fixtures/gl/ivec3_uniform_through_float_gep.ll"),
            include_str!("../../translator/tests/fixtures/gl/sampler2d_param_chosen_by_constant.ll"),
        ] {
            let spv = translate_one(ll, Stage::Vertex).expect("GL vertex shader translates");
            let lowered = buffer_addresses::lower(&spv).expect("buffer-address lowering");
            let uniform = table_ubo::convert(&lowered).expect("dynamic uniform address table");
            for bytes in [&spv, &lowered, &uniform] {
                let words: Vec<u32> = bytes.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
                let mut options = spirv_tools::val::ValidatorOptions::default();
                options.scalar_block_layout = true;
                spirv_tools::val::create(Some(spirv_tools::TargetEnv::Vulkan_1_2))
                    .validate(&words, Some(options)).expect("GL shader remains valid Vulkan 1.2 SPIR-V");
            }
        }
    }
}

#[cfg(test)]
mod storage_caps_tests {
    use super::*;
    use spirv_tools::val::Validator;
    #[test] #[ignore] fn corpus_formatless_storage_images() {
        use std::collections::BTreeMap;
        let check=|b:&[u8]|->Result<(),String>{ let w:Vec<u32>=b.chunks_exact(4).map(|w|u32::from_le_bytes(w.try_into().unwrap())).collect();
            spirv_tools::val::create(Some(spirv_tools::TargetEnv::Vulkan_1_2)).validate(&w,None).map_err(|e|e.to_string()) };
        let mut files:Vec<_>=std::fs::read_dir("/Library/GPUBundles/nvmtl/aircache").unwrap().filter_map(|e|e.ok()).map(|e|e.path()).collect(); files.sort();
        let t0=std::time::Instant::now();
        let mut nostex=0; let (mut units,mut same,mut differ_ok,mut differ_bad,mut differ_wasbad,mut refused,mut rescued,mut both_err)=(0,0,0,0,0,0,0,0);
        let mut why:BTreeMap<String,(u32,String)>=BTreeMap::new();
        for f in files.iter().step_by(4) {
            if t0.elapsed().as_secs()>480 { println!("STOPPED EARLY at the 480 s cap"); break; }
            let ll=match std::fs::read_to_string(f){Ok(s)=>s,Err(_)=>continue};
            for (tag,st) in [("!air.vertex =",Stage::Vertex),("!air.fragment =",Stage::Fragment),("!air.kernel =",Stage::Kernel)] {
                if !ll.contains(tag) { continue; }
                units+=1;
                let old=metal2vulkan::translate_native_no_retry(&ll,st);
                let Some(o)=formatless_options(&ll,st,3) else { nostex+=1; continue; };
                let new=metal2vulkan::translate_native_no_retry_with_options(&ll,st,o);
                match (old,new) {
                    (Err(_),Err(_))=>both_err+=1,
                    (Err(_),Ok(_))=>rescued+=1,
                    (Ok(_),Err(e))=>{ refused+=1; let k:String=e.chars().filter(|c|!c.is_ascii_digit()).take(120).collect();
                        let v=why.entry(k).or_insert((0,f.file_name().unwrap().to_string_lossy().into())); v.0+=1; }
                    (Ok(a),Ok(b))=>{ if a==b { same+=1; } else { match (check(&a).is_ok(),check(&b)) {
                        (_,Ok(()))=>differ_ok+=1, (false,Err(_))=>differ_wasbad+=1,
                        (true,Err(e))=>{ differ_bad+=1; let k:String=format!("INVALID: {}",e.chars().filter(|c|!c.is_ascii_digit()).take(110).collect::<String>());
                            let v=why.entry(k).or_insert((0,f.file_name().unwrap().to_string_lossy().into())); v.0+=1; } } } }
                }
            }
        }
        println!("FORMATLESS stride 4: units {units}  no-float-storage-image(untouched) {nostex}  byte-identical {same}  differ+valid {differ_ok}  differ+NOW-INVALID {differ_bad}  differ+was-already-invalid {differ_wasbad}  refused->fallback {refused}  rescued {rescued}  both-err {both_err}  in {} s",t0.elapsed().as_secs());
        for (k,(n,f)) in &why { println!("  {n:5}  {k}   e.g. {f}"); }
        assert_eq!(differ_bad,0,"the formatless state turned a VALID module INVALID");
    }
}
#[no_mangle]
pub unsafe extern "C" fn nvmtl_lower_buffer_addresses(
    data: *const u8, len: usize, out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() { return -1; }
    match buffer_addresses::lower(std::slice::from_raw_parts(data,len)) {
        Ok(v) => { let mut b=v.into_boxed_slice(); *out=b.as_mut_ptr(); *out_len=b.len(); std::mem::forget(b); 0 }
        Err(e) => { if !err.is_null() && err_len>0 { let n=e.len().min(err_len-1); std::ptr::copy_nonoverlapping(e.as_ptr(),err.cast(),n); *err.add(n)=0; } -1 }
    }
}
#[no_mangle]
pub unsafe extern "C" fn nvmtl_table_to_uniform(
    data: *const u8, len: usize, out: *mut *mut u8, out_len: *mut usize,
    err: *mut c_char, err_len: usize,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() { return -1; }
    match table_ubo::convert(std::slice::from_raw_parts(data,len)) {
        Ok(v) => { let mut b=v.into_boxed_slice(); *out=b.as_mut_ptr(); *out_len=b.len(); std::mem::forget(b); 0 }
        Err(e) => { if !err.is_null() && err_len>0 { let n=e.len().min(err_len-1); std::ptr::copy_nonoverlapping(e.as_ptr(),err.cast(),n); *err.add(n)=0; } -1 }
    }
}
#[no_mangle]
pub unsafe extern "C" fn nvmtl_lower_buffer_addresses_ex(
    data: *const u8, len: usize, flags: u32, out: *mut *mut u8, out_len: *mut usize, info: *mut u32,
    err: *mut c_char, err_len: usize,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() { return -1; }
    let r = buffer_addresses::lower(std::slice::from_raw_parts(data,len))
        .and_then(|v| if flags & 2 != 0 { table_ubo::convert(&v) } else { Ok(v) })
        .and_then(|v| align_guard::guard(&v, flags & 1 != 0));
    match r {
        Ok((v, rep)) => {
            if !info.is_null() { *info = rep.mask; *info.add(1) = rep.req; *info.add(2) = rep.untraceable as u32; *info.add(3) = rep.accesses; }
            let mut b=v.into_boxed_slice(); *out=b.as_mut_ptr(); *out_len=b.len(); std::mem::forget(b); 0
        }
        Err(e) => { if !err.is_null() && err_len>0 { let n=e.len().min(err_len-1); std::ptr::copy_nonoverlapping(e.as_ptr(),err.cast(),n); *err.add(n)=0; } -1 }
    }
}
#[no_mangle]
pub unsafe extern "C" fn nvmtl_lower_buffer_addresses_g2(
    data: *const u8, len: usize, flags: u32, mask: u32, out: *mut *mut u8, out_len: *mut usize, info: *mut u32,
    err: *mut c_char, err_len: usize,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() { return -1; }
    let mut eligible = 0u32;
    let r = buffer_addresses::lower(std::slice::from_raw_parts(data,len))
        .and_then(|v| if flags & 2 != 0 { table_ubo::convert(&v) } else { Ok(v) })
        .and_then(|v| g2_inline::apply(&v, if flags & 4 != 0 { mask } else { 0 }).map(|(v, e)| { eligible = e; v }))
        .and_then(|v| align_guard::guard(&v, flags & 1 != 0));
    match r {
        Ok((v, rep)) => {
            if !info.is_null() { *info = rep.mask; *info.add(1) = rep.req; *info.add(2) = rep.untraceable as u32; *info.add(3) = rep.accesses; *info.add(4) = eligible; }
            let mut b=v.into_boxed_slice(); *out=b.as_mut_ptr(); *out_len=b.len(); std::mem::forget(b); 0
        }
        Err(e) => { if !err.is_null() && err_len>0 { let n=e.len().min(err_len-1); std::ptr::copy_nonoverlapping(e.as_ptr(),err.cast(),n); *err.add(n)=0; } -1 }
    }
}
#[no_mangle]
pub unsafe extern "C" fn nvmtl_translate_samplers(
    ll:*const c_char, stage:*const c_char, json:*const c_char,
    out:*mut *mut u8, out_len:*mut usize, err:*mut c_char, err_len:usize,
)->i32 {
    let result=(|| -> Result<Vec<u8>,String> {
        if ll.is_null()||stage.is_null()||json.is_null()||out.is_null()||out_len.is_null(){return Err("null sampler argument".into())}
        let ll=CStr::from_ptr(ll).to_str().map_err(|e|e.to_string())?;
        let st=match CStr::from_ptr(stage).to_bytes(){b"vertex"=>Stage::Vertex,b"fragment"=>Stage::Fragment,b"kernel"=>Stage::Kernel,_=>return Err("bad stage".into())};
        let j:serde_json::Value=serde_json::from_str(CStr::from_ptr(json).to_str().map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        let constants:std::collections::BTreeMap<u32,Vec<u8>>=serde_json::from_value(j["constants"].clone()).map_err(|e|e.to_string())?;
        let values:Vec<_>=constants.into_iter().collect();
        let specialized=metal2vulkan::specialize_air_function_constants(ll,&values)?;
        let ll=specialized.as_ref();
        let reflection=metal2vulkan::reflect_sanitized(ll,st,metal2vulkan::passes::TransformOptions::default())?;
        let states:std::collections::BTreeMap<u32,metal2vulkan::reflect::RuntimeSamplerState>=serde_json::from_value(j["samplers"].clone()).map_err(|e|e.to_string())?;
        let mut opts=formatless_options(ll,st,CAPS.load(Ordering::Relaxed)).unwrap_or_default();
        let a8=j["a8_targets"].as_u64().unwrap_or(0) as u32;   // Metal A8Unorm colour attachments, bit per location
        opts=opts.with_a8_render_targets(a8);
        let mut used=0;
        for (idx,state) in states {
            if reflection.bindings.iter().any(|b| b.kind==metal2vulkan::reflect::ResourceKind::Sampler && b.metal_index==idx) {
                opts=opts.with_runtime_sampler(idx,state)?;used+=1;
            }
        }
        if used==0 && a8==0{return translate_one(ll,st)}
        let opts=apply_tg(opts);
        translate_with_pointer_retry(ll, |ll| {
            metal2vulkan::translate_native_no_retry_constructed_with_options(ll,st,opts)
        })
    })();
    match result {
        Ok(bytes)=>{let mut b=bytes.into_boxed_slice();*out=b.as_mut_ptr();*out_len=b.len();std::mem::forget(b);0}
        Err(e)=>{if !err.is_null()&&err_len>0{let n=e.len().min(err_len-1);std::ptr::copy_nonoverlapping(e.as_ptr(),err.cast(),n);*err.add(n)=0;}-1}
    }
}
#[cfg(test)]
mod pixel_sampler_checks {
 use super::*;
 use spirv_tools::val::Validator;
 #[test]
 fn compositor_pixel_sampler_spirv_validates() {
  let ll=std::fs::read_to_string("$HOME/nvmtl-tests/k7-diagnostic/GPUPass.ll").unwrap();
  let ll=std::ffi::CString::new(ll).unwrap();let stage=std::ffi::CString::new("fragment").unwrap();
  for filter in ["Nearest","Linear"] { for gamma in [0u8,4] {
   let state=serde_json::json!({"min_filter":filter,"mag_filter":filter,"mip_filter":"None","address_mode_s":"ClampToEdge","address_mode_t":"ClampToEdge","address_mode_r":"ClampToEdge","coordinates":"Pixel","compare_function":"None","max_anisotropy":1,"lod_min_clamp":0.0,"lod_max_clamp":1000.0,"border_color":"TransparentBlack","reduction":"WeightedAverage","lod_bias":0.0});
   let j=serde_json::json!({"samplers":{"0":state,"14":state},"constants":{"0":[gamma,0,0,0],"1":[0,0,0,0]}});
   let j=std::ffi::CString::new(j.to_string()).unwrap();let(mut out,mut len)=(std::ptr::null_mut(),0);let mut err=[0i8;2048];
   let rc=unsafe {nvmtl_translate_samplers(ll.as_ptr(),stage.as_ptr(),j.as_ptr(),&mut out,&mut len,err.as_mut_ptr(),err.len())};
   assert_eq!(rc,0,"{}",unsafe{CStr::from_ptr(err.as_ptr())}.to_string_lossy());
   let bytes=unsafe{std::slice::from_raw_parts(out,len)};
   let words:Vec<u32>=bytes.chunks_exact(4).map(|c|u32::from_le_bytes(c.try_into().unwrap())).collect();
   spirv_tools::val::create(Some(spirv_tools::TargetEnv::Vulkan_1_2)).validate(&words,None).unwrap();
   nvmtl_translate_free(out,len);
   println!("PASS compositor {filter} gamma={gamma}: Vulkan 1.2 SPIR-V valid; unused bound sampler filtered");
  }}
 }
}
#[cfg(test)]
mod ca_named_checks {
 use spirv_tools::val::Validator;
 #[test] fn ca_named_modules_validate() {
  for name in ["fixed_vert_lpf_spc","fixed_frag_lpf_cpf"] {
   let b=std::fs::read(format!("$HOME/nvmtl-tests/k7-diagnostic/named-{name}.spv")).unwrap();
   let w:Vec<u32>=b.chunks_exact(4).map(|c|u32::from_le_bytes(c.try_into().unwrap())).collect();
   spirv_tools::val::create(Some(spirv_tools::TargetEnv::Vulkan_1_2)).validate(&w,None).unwrap();
   println!("PASS CoreAnimation {name}: named scalar/vector specialization validates");
  }
 }
}

#[cfg(test)]
mod threadgroup_configuration_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn simultaneous_translations_keep_independent_lengths() {
        nvmtl_translate_set_threadgroup_lengths(std::ptr::null(), 0);
        let rendezvous = Arc::new(Barrier::new(3));
        let handles = [64u32, 8720].map(|bytes| {
            let rendezvous = Arc::clone(&rendezvous);
            std::thread::spawn(move || {
                nvmtl_translate_set_threadgroup_lengths(&bytes, 1);
                rendezvous.wait();
                let actual = threadgroup_lengths();
                rendezvous.wait();
                assert_eq!(actual, vec![(0, bytes)]);
                nvmtl_translate_set_threadgroup_lengths(std::ptr::null(), 0);
                assert!(threadgroup_lengths().is_empty());
            })
        });
        rendezvous.wait();
        let main_lengths = threadgroup_lengths();
        rendezvous.wait();
        for handle in handles { handle.join().expect("worker configuration"); }
        assert!(main_lengths.is_empty());
    }

    // A kernel that binds a sampler takes the sampler path, which used to drop the threadgroup lengths: the Workgroup
    // array kept its 512-element default, so a kernel asking for 16384 bytes of float4 (VideoToolbox's scaler) got half.
    const SAMPLER_AND_THREADGROUP_KERNEL: &str = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %texture, ptr addrspace(2) %sampler, ptr addrspace(3) %temp, ptr addrspace(1) %out, i32 %i) {
entry:
  %sample = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %texture, ptr addrspace(2) %sampler, <2 x float> zeroinitializer, i1 false, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds <4 x float>, ptr addrspace(3) %temp, i64 %idx
  store <4 x float> %color, ptr addrspace(3) %slot, align 16
  %back = load <4 x float>, ptr addrspace(3) %slot, align 16
  store <4 x float> %back, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"texture"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"temp"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!7 = !{i32 4, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;

    fn workgroup_array_elements_after_sampler_translation(tg_bytes: Option<u32>) -> u32 {
        match tg_bytes {
            Some(bytes) => nvmtl_translate_set_threadgroup_lengths(&bytes, 1),
            None => nvmtl_translate_set_threadgroup_lengths(std::ptr::null(), 0),
        }
        let ll = std::ffi::CString::new(SAMPLER_AND_THREADGROUP_KERNEL).unwrap();
        let stage = std::ffi::CString::new("kernel").unwrap();
        let state = serde_json::json!({"min_filter":"Nearest","mag_filter":"Nearest","mip_filter":"None","address_mode_s":"ClampToEdge","address_mode_t":"ClampToEdge","address_mode_r":"ClampToEdge","coordinates":"Normalized","compare_function":"None","max_anisotropy":1,"lod_min_clamp":0.0,"lod_max_clamp":1000.0,"border_color":"TransparentBlack","reduction":"WeightedAverage","lod_bias":0.0});
        let json = std::ffi::CString::new(serde_json::json!({"samplers":{"0":state},"constants":{}}).to_string()).unwrap();
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        let mut err = [0 as c_char; 2048];
        let rc = unsafe { nvmtl_translate_samplers(ll.as_ptr(), stage.as_ptr(), json.as_ptr(), &mut out, &mut len, err.as_mut_ptr(), err.len()) };
        nvmtl_translate_set_threadgroup_lengths(std::ptr::null(), 0);
        assert_eq!(rc, 0, "{}", unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy());
        let spv = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
        nvmtl_translate_free(out, len);
        let module = rspirv::dr::load_bytes(&spv).expect("load spv");
        let constants: std::collections::HashMap<u32, u32> = module.types_global_values.iter()
            .filter(|i| i.class.opcode == rspirv::spirv::Op::Constant)
            .filter_map(|i| match (i.result_id, i.operands.first()) {
                (Some(id), Some(rspirv::dr::Operand::LiteralBit32(v))) => Some((id, *v)),
                _ => None,
            }).collect();
        // the Workgroup variable's pointee array, found through the variable rather than by "the only array"
        let workgroup_pointee = module.types_global_values.iter().find_map(|i| {
            (i.class.opcode == rspirv::spirv::Op::Variable
                && i.operands.first() == Some(&rspirv::dr::Operand::StorageClass(rspirv::spirv::StorageClass::Workgroup)))
                .then(|| i.result_type)
        }).flatten().expect("a Workgroup variable");
        let array = module.types_global_values.iter().find(|i| i.result_id == Some(workgroup_pointee))
            .and_then(|ptr| match ptr.operands.get(1) { Some(rspirv::dr::Operand::IdRef(t)) => Some(*t), _ => None })
            .expect("the pointer's pointee");
        let length_id = module.types_global_values.iter().find(|i| i.result_id == Some(array))
            .and_then(|a| match a.operands.get(1) { Some(rspirv::dr::Operand::IdRef(c)) => Some(*c), _ => None })
            .expect("an array type");
        constants[&length_id]
    }

    #[test]
    fn sampler_specialization_keeps_the_threadgroup_length() {
        assert_eq!(workgroup_array_elements_after_sampler_translation(None), 512, "no length: the default");
        assert_eq!(workgroup_array_elements_after_sampler_translation(Some(16384)), 1024, "16384 bytes of float4");
        assert_eq!(workgroup_array_elements_after_sampler_translation(Some(4096)), 256, "4096 bytes of float4");
    }
}
