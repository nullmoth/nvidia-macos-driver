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
use metal2vulkan::{disassemble, translate_sanitized_native};
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

    // One descriptor array holding both samplers, not two separate bindings.
    let (array, length) = ops
        .iter()
        .find_map(|(id, body)| {
            let mut operands = body.strip_prefix("OpTypeArray ")?.split_whitespace();
            let element = operands.next()?;
            let length = operands.next()?;
            (bare(element) == sampler).then_some((*id, bare(length)))
        })
        .expect("the descriptor is an array of samplers");
    assert_eq!(
        constant_value(&ops, length),
        Some(2),
        "the array must hold both declared samplers:\n{asm}"
    );

    // The descriptor array is the UniformConstant variable the shader binds.
    let var = ops
        .iter()
        .filter_map(|(id, body)| {
            let mut operands = body.strip_prefix("OpVariable ")?.split_whitespace();
            let pointer = operands.next()?;
            let storage = operands.next()?;
            (storage == "UniformConstant").then_some((*id, bare(pointer)))
        })
        .find(|(_, pointer)| {
            operands_of(&ops, pointer, "OpTypePointer ").is_some_and(|operands| {
                operands.split_whitespace().last().map(bare) == Some(array)
            })
        })
        .map(|(id, _)| id)
        .expect("the descriptor array is bound as a UniformConstant variable");

    // The shader samples through element 1, the index its GEP states.
    let chained_index = ops.iter().find_map(|(_, body)| {
        let mut operands = body.strip_prefix("OpAccessChain ")?.split_whitespace();
        let _result_type = operands.next()?;
        if bare(operands.next()?) != var {
            return None;
        }
        let index = operands.next()?;
        operands.next().is_none().then_some(bare(index))
    });
    assert_eq!(
        chained_index.and_then(|index| constant_value(&ops, index)),
        Some(1),
        "the element the constant selects must be the one chained:\n{asm}"
    );

    // And that chain feeds a real sampler load, not a placeholder or a synthesized default.
    assert!(
        ops.iter().any(|(_, body)| {
            body.strip_prefix("OpLoad ")
                .and_then(|operands| operands.split_whitespace().next())
                .map(bare)
                == Some(sampler)
        }),
        "the chained descriptor must be loaded as a sampler value:\n{asm}"
    );

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
