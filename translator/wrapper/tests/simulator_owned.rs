//! Original CPU fixtures: no Apple/application shader or GPU execution.
use metal2vulkan::passes::{Stage, TransformOptions};
use rspirv::dr::{load_bytes, Module, Operand};
use rspirv::spirv::{Op, Word};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};

fn translate(ll: &str) -> Vec<u8> {
    let bytes = metal2vulkan::translate_native_no_retry_constructed_with_options(
        ll, Stage::Kernel, TransformOptions::default(),
    ).expect("owned LLVM translates");
    let validator = std::env::var_os("METAL2VULKAN_SPIRV_VAL").expect("required independent validator");
    let mut child = Command::new(validator).args(["--target-env", "vulkan1.2", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    bytes
}

#[test]
fn simulator_union_helper_byte_views_keep_dynamic_element_indices() {
    let inputs = [
        include_str!("fixtures/simulator/byte_array-helper_array.ll"),
        include_str!("fixtures/simulator/byte_array-helper_byte.ll"),
        include_str!("fixtures/simulator/byte_union-helper_array.ll"),
        include_str!("fixtures/simulator/byte_union-helper_byte.ll"),
        include_str!("fixtures/simulator/scalar_union-helper_array.ll"),
        include_str!("fixtures/simulator/scalar_union-helper_byte.ll"),
        include_str!("fixtures/simulator/vec_union-helper_array.ll"),
        include_str!("fixtures/simulator/vec_union-helper_byte.ll"),
        include_str!("fixtures/simulator/word_union-helper_array.ll"),
        include_str!("fixtures/simulator/word_union-helper_byte.ll"),
    ];
    for ll in inputs {
        let module = load_bytes(translate(ll)).unwrap();
        let defs: HashMap<_, _> = module.all_inst_iter().filter_map(|i| i.result_id.map(|id| (id, i))).collect();
        assert!(module.all_inst_iter().any(|i| matches!(i.class.opcode, Op::AccessChain | Op::InBoundsAccessChain)
            && i.operands.iter().skip(1).any(|o| matches!(o, Operand::IdRef(id)
                if defs.get(id).is_some_and(|d| d.class.opcode == Op::UConvert)))),
            "dynamic array element must not become member zero");
    }
}

fn input_roots(module: &Module) -> HashSet<Word> {
    module.annotations.iter().filter_map(|i| {
        if i.class.opcode != Op::Decorate { return None; }
        match i.operands.as_slice() {
            [Operand::IdRef(id), Operand::Decoration(rspirv::spirv::Decoration::Binding), Operand::LiteralBit32(0 | 1)] => Some(*id),
            _ => None,
        }
    }).collect()
}

fn verify_selected_loads_are_guarded(module: &Module) {
    let defs: HashMap<_, _> = module.all_inst_iter().filter_map(|i| i.result_id.map(|id| (id, i))).collect();
    let roots = input_roots(module);
    fn root(id: Word, defs: &HashMap<Word, &rspirv::dr::Instruction>) -> Word {
        let Some(i) = defs.get(&id) else { return id; };
        if matches!(i.class.opcode, Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::InBoundsPtrAccessChain | Op::CopyObject) {
            if let Some(Operand::IdRef(base)) = i.operands.first() { return root(*base, defs); }
        }
        id
    }
    let mut checked = 0;
    for function in &module.functions {
        let mut successors: HashMap<Word, Vec<Word>> = HashMap::new();
        let mut selections = Vec::new();
        for block in &function.blocks {
            let id = block.label.as_ref().unwrap().result_id.unwrap();
            let Some(end) = block.instructions.last() else { continue; };
            match end.class.opcode {
                Op::Branch => { if let Operand::IdRef(next) = end.operands[0] { successors.insert(id, vec![next]); } }
                Op::BranchConditional => {
                    let [_, Operand::IdRef(a), Operand::IdRef(b), ..] = end.operands.as_slice() else { panic!("branch"); };
                    successors.insert(id, vec![*a, *b]);
                    if let Some(merge) = block.instructions.iter().find(|i| i.class.opcode == Op::SelectionMerge) {
                        let Operand::IdRef(merge) = merge.operands[0] else { panic!("merge"); };
                        selections.push((*a, *b, merge));
                    }
                }
                _ => {}
            }
        }
        let reachable = |start: Word, stop: Word| {
            let mut seen = HashSet::new(); let mut pending = vec![start];
            while let Some(id) = pending.pop() {
                if id == stop || !seen.insert(id) { continue; }
                if let Some(next) = successors.get(&id) { pending.extend(next.iter().copied()); }
            }
            seen
        };
        let guarded: Vec<_> = selections.iter().map(|(a,b,merge)| (reachable(*a,*merge), reachable(*b,*merge))).collect();
        for block in &function.blocks {
            let id = block.label.as_ref().unwrap().result_id.unwrap();
            for i in &block.instructions {
                if i.class.opcode != Op::Load { continue; }
                let Some(Operand::IdRef(ptr)) = i.operands.first() else { continue; };
                if !roots.contains(&root(*ptr, &defs)) { continue; }
                assert!(guarded.iter().any(|(a,b)| a.contains(&id) ^ b.contains(&id)),
                    "input load was emitted before selection or after its merge: {i:?}");
                let pointer = defs[ptr];
                if pointer.operands.iter().skip(1).any(|o| matches!(o, Operand::IdRef(index)
                    if defs.get(index).is_some_and(|d| d.class.opcode != Op::Constant))) {
                    let address_block = function.blocks.iter().find(|b| b.instructions.iter().any(|i| i.result_id == Some(*ptr))).unwrap();
                    let address_id = address_block.label.as_ref().unwrap().result_id.unwrap();
                    assert!(guarded.iter().any(|(a,b)| a.contains(&address_id) ^ b.contains(&address_id)),
                        "dynamic candidate address was formed before its selected arm");
                }
                checked += 1;
            }
        }
    }
    assert!(checked >= 2, "both input arms must be checked");
}

#[test]
fn simulator_unused_partition_like_pointer_is_not_loaded_or_materialized() {
    for ll in [
        include_str!("fixtures/simulator/raw-gep-then-select.ll"),
        include_str!("fixtures/simulator/raw-select-then-gep.ll"),
        include_str!("fixtures/simulator/raw-nested-select.ll"),
    ] {
        let module = load_bytes(translate(ll)).unwrap();
        verify_selected_loads_are_guarded(&module);
    }
}

#[test]
fn simulator_local_direct_selection_remains_valid() {
    translate(include_str!("fixtures/simulator/local-direct-select.ll"));
}
