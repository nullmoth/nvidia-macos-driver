//! Apple's `array<sampler, N>` parameter is one Vulkan descriptor array of `OpTypeSampler`, and the
//! shader picks its element with a constant index. The translator used to refuse these outright
//! ("no descriptor-array lowering for"), which made ANGLE fall back to Apple's GL on the CPU.
//!
//! The emitter does not model a loaded sampler-state pointer, so it records what the load was
//! (`LocalPointerFieldLoad` naming the array and the constant index) and hands the value over as
//! an opaque placeholder. The resource pass turns that into `OpAccessChain` into the descriptor
//! array plus an `OpLoad` of a sampler value, which is what `OpImageSample*` needs.
//!
//! Only a constant index is lowered: Metal never emits anything else for a sampler array (WebGL2
//! rejects "array index for samplers must be constant integral expressions").
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, inline_and_forward_object_fields, translate_native_no_retry, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d =
        std::env::temp_dir().join(format!("m2v_sampler_descriptor_arrays_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn spirv_val(spv: &[u8]) {
    let out = std::process::Command::new("spirv-val")
        .args(["--target-env", "vulkan1.2", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(spv)?;
            c.wait_with_output()
        });
    if let Ok(out) = out {
        assert!(
            out.status.success(),
            "spirv-val: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every `%id = OpSomething operands` line, as `(id without %, body)`.
fn operations(asm: &str) -> Vec<(&str, &str)> {
    asm.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" = ")?;
            Some((left.trim().strip_prefix('%')?, right.trim()))
        })
        .collect()
}

fn bare(token: &str) -> &str {
    token.strip_prefix('%').unwrap_or(token)
}

fn operands_of<'a>(ops: &'a [(&str, &str)], id: &str, opcode: &str) -> Option<&'a str> {
    let wanted = id.strip_prefix('%').unwrap_or(id);
    ops.iter()
        .find(|(candidate, body)| *candidate == wanted && body.starts_with(opcode))?
        .1
        .strip_prefix(opcode)
}

/// The literal an `OpConstant` names, e.g. `OpConstant %uint 2` or `OpConstant %6 2`.
fn constant_value(ops: &[(&str, &str)], id: &str) -> Option<i64> {
    let id = id.strip_prefix('%').unwrap_or(id);
    operands_of(ops, id, "OpConstant ")?
        .split_whitespace()
        .last()?
        .parse()
        .ok()
}

const FIXTURE: &str = include_str!("fixtures/gl/sampler_array_chosen_by_constant.ll");

#[test]
fn a_sampler_descriptor_array_samples_the_element_the_constant_selects() {
    let spv = translate_sanitized_native(FIXTURE, Stage::Vertex, &tmp())
        .expect("a sampler descriptor array must translate");
    let asm = disassemble(&spv).expect("disassemble");
    if std::env::var_os("M2V_SHOW_ASM").is_some() {
        eprintln!("{asm}");
    }
    let ops = operations(&asm);

    let sampler = ops
        .iter()
        .find(|(_, body)| *body == "OpTypeSampler")
        .map(|(id, _)| *id)
        .expect("the module declares a sampler type");

    // One sampler variable per element, at consecutive bindings: the plugin declares every Metal
    // sampler index as its own count-1 binding, so a [2 x sampler] at the first binding read past
    // it on the GPU and sampled element 1 with the wrong sampler.
    assert!(
        !ops.iter().any(|(_, body)| body.starts_with("OpTypeArray ")
            && body.split_whitespace().nth(1).map(bare) == Some(sampler)),
        "no sampler array type may be declared:\n{asm}"
    );
    let pointer = ops
        .iter()
        .find(|(_, body)| {
            body.strip_prefix("OpTypePointer UniformConstant ").map(bare) == Some(sampler)
        })
        .map(|(id, _)| *id)
        .expect("a UniformConstant sampler pointer");
    let binding_of = |var: &str| {
        asm.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("OpDecorate ")?;
            let mut it = rest.split_whitespace();
            (bare(it.next()?) == var && it.next()? == "Binding").then(|| it.next()?.parse::<u32>().ok())?
        })
    };
    // The shader samples through element 1, the index its GEP states: the sampler it loads is a
    // plain sampler variable at the binding after the parameter's own (sampler index 0 + 1).
    // Element 0 is unused here, so its variable may be stripped.
    let loaded: Vec<&str> = ops
        .iter()
        .filter_map(|(_, body)| {
            let mut it = body.strip_prefix("OpLoad ")?.split_whitespace();
            (bare(it.next()?) == sampler).then(|| bare(it.next().unwrap_or("")))
        })
        .collect();
    assert_eq!(loaded.len(), 1, "one sampler load:\n{asm}");
    assert!(
        ops.iter().any(|(id, body)| *id == loaded[0]
            && body.strip_prefix("OpVariable ").and_then(|r| r.split_whitespace().next()).map(bare) == Some(pointer)),
        "the loaded sampler is a plain sampler variable:\n{asm}"
    );
    let first = metal2vulkan::reflect::sampler_resource_binding(0).expect("sampler band");
    assert_eq!(binding_of(loaded[0]), Some(first + 1), "the constant selects element 1:\n{asm}");

    assert!(
        asm.contains("OpImageSampleExplicitLod"),
        "the sample must still happen:\n{asm}"
    );

    spirv_val(&spv);
}

#[test]
fn a_sampler_descriptor_array_whose_declared_length_contradicts_its_type_fallbacks() {
    // The metadata claims three descriptors, the LLVM type carries two: sizing the descriptor
    // array from either one would be a guess, so the module must fall back instead.
    let mismatched = FIXTURE.replace(
        "!\"air.location_index\", i32 0, i32 2, !\"air.arg_type_name\", !\"array<sampler, 2>\"",
        "!\"air.location_index\", i32 0, i32 3, !\"air.arg_type_name\", !\"array<sampler, 3>\"",
    );
    assert_ne!(
        mismatched, FIXTURE,
        "the fixture must carry the count to contradict"
    );

    match translate_sanitized_native(&mismatched, Stage::Vertex, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes); a descriptor array \
             sized from an ABI its type contradicts is a silently wrong module",
            spv.len()
        ),
        Err(e) => assert!(
            e.contains("occupies 3 sampler descriptors"),
            "the diagnostic should name the declared count; got: {e}"
        ),
    }
}

#[test]
fn a_sampler_array_past_the_sampler_indices_of_a_stage_fallbacks() {
    // Each element becomes a variable and a binding of its own, and the count comes from the shader: a
    // stage has 16 sampler indices, so 17 must be refused rather than declare bindings past the band.
    let huge = FIXTURE
        .replace("[2 x ptr addrspace(2)]", "[17 x ptr addrspace(2)]")
        .replace(
            "!\"air.location_index\", i32 0, i32 2, !\"air.arg_type_name\", !\"array<sampler, 2>\"",
            "!\"air.location_index\", i32 0, i32 17, !\"air.arg_type_name\", !\"array<sampler, 17>\"",
        );
    assert!(huge.contains("array<sampler, 17>"), "the fixture must carry the count to raise");
    match translate_sanitized_native(&huge, Stage::Vertex, &tmp()) {
        Ok(spv) => panic!("an array of 17 samplers translated ({} bytes)", spv.len()),
        Err(e) => assert!(e.contains("sampler indices a stage has"), "got: {e}"),
    }
}

const TEXTURE_ENV: &str = include_str!("fixtures/gl/sampler_array_through_texture_env.ll");

/// The binding of the sampler variable an `OpSampledImage` combines, through its `OpLoad`.
fn sampler_binding_of(asm: &str, ops: &[(&str, &str)], sampled_image: &str) -> Option<u32> {
    let mut it = operands_of(ops, sampled_image, "OpSampledImage ")?.split_whitespace();
    let (_ty, _image, sampler) = (it.next()?, it.next()?, it.next()?);
    let var = operands_of(ops, sampler, "OpLoad ")?.split_whitespace().nth(1).map(bare)?;
    asm.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("OpDecorate ")?;
        let mut it = rest.split_whitespace();
        (bare(it.next()?) == var && it.next()? == "Binding").then(|| it.next()?.parse::<u32>().ok())?
    })
}

#[test]
fn a_sampler_array_element_reached_through_angles_texture_env_samples_its_own_element() {
    // #449: the shadow map of light 1 is element 1 of both arrays. As written the helper's sampler is a pointer read out
    // of the env object, which has no sampler state; the inline retry must forward it, and must also see the element
    // through the `metal::sampler`/`metal::depth2d` wrappers, or element 1's samples silently fold to zero.
    if let Ok(spv) = translate_native_no_retry(TEXTURE_ENV, Stage::Fragment) {
        let asm = disassemble(&spv).expect("disassemble");
        assert_eq!(
            asm.matches("OpImageSampleDref").count(),
            2,
            "a module that translates as written must still sample both shadow maps:\n{asm}"
        );
    }
    let retry = inline_and_forward_object_fields(TEXTURE_ENV).expect("the helper is inlined");
    let spv = translate_native_no_retry(&retry, Stage::Fragment)
        .unwrap_or_else(|e| panic!("the inline retry must translate: {e}\n{retry}"));
    let asm = disassemble(&spv).expect("disassemble");
    if std::env::var_os("M2V_SHOW_ASM").is_some() {
        eprintln!("{asm}");
    }
    let ops = operations(&asm);
    let dref: Vec<&str> = ops
        .iter()
        .filter_map(|(_, body)| {
            body.strip_prefix("OpImageSampleDref")?.split_whitespace().nth(2).map(bare)
        })
        .collect();
    assert_eq!(dref.len(), 2, "one depth compare per light, none folded away:\n{asm}");
    let first = metal2vulkan::reflect::sampler_resource_binding(0).expect("sampler band");
    let mut bindings: Vec<Option<u32>> =
        dref.iter().map(|si| sampler_binding_of(&asm, &ops, si)).collect();
    bindings.sort();
    assert_eq!(
        bindings,
        vec![Some(first), Some(first + 1)],
        "light 0 samples with element 0 and light 1 with element 1:\n{asm}"
    );
    let images: Vec<&str> = dref
        .iter()
        .filter_map(|si| operands_of(&ops, si, "OpSampledImage ")?.split_whitespace().nth(1).map(bare))
        .collect();
    assert!(images.len() == 2 && images[0] != images[1], "each light reads its own shadow map:\n{asm}");
    spirv_val(&spv);
}
