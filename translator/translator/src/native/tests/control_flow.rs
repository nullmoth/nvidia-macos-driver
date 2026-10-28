#![allow(unused_imports)]
use super::super::cfg::{
    exceeds_local_structured_plan_budget, id_ref_operand, infer_branch_merges,
    infer_direct_switch_merges, infer_loop_merges, infer_switch_merges,
    infer_switch_merges_bounded, loop_forest_is_empty, lower_unstructured_switches,
    split_body_blocks, BodyBlock, CROSS_ARM_EDGE_MAX_BLOCKS,
};
use super::super::emit_vulkan_spirv;
use super::super::emitter::Emitter;
use super::super::ir::{LlType, LlValue};
use super::super::parse::{parse_type, parse_typed_value};
use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::passes::{self, Stage};
use crate::spirv_module::load_bytes;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Instruction};
use crate::{disassemble, meta, tools};
use spirv::{Capability, Decoration, Op, Scope, SelectionControl, StorageClass, Word};
use std::path::PathBuf;

fn carriered_block(name: &str, lines: &[&str]) -> BodyBlock {
    let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let typed = crate::native::tir::lower_block_carrier(name, &lines, &HashMap::default());
    BodyBlock {
        name: name.to_string(),
        role: crate::native::cfg::BlockRole::Normal,
        typed: typed.map(Into::into),
    }
}

#[test]
fn default_construct_tree_keeps_normal_plan_merges_per_function() {
    let ll = r#"
define void @k() {
entry:
  %cond = icmp eq i32 0, 0
  br i1 %cond, label %left, label %right

left:
  br label %merge

right:
  br label %merge

merge:
  ret void
}
"#;

    let spv = crate::native::emit_vulkan_spirv(ll).expect("default emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpSelectionMerge").count(), 1, "{asm}");
}

#[test]
fn construct_tree_with_symbolic_raw_cursor_selects_raw_relooper() {
    let ll = r#"
%S = type { i32 }

define void @k(ptr addrspace(1) %bytes, i1 %outer, i1 %inner, i1 %done) {
entry:
  br i1 %outer, label %inner_header, label %outer_else
inner_header:
  br i1 %inner, label %inner_then, label %inner_else
inner_then:
  br label %exit
inner_else:
  br label %loop
outer_else:
  br label %loop
loop:
  %i = phi i64 [ 0, %inner_else ], [ 0, %outer_else ], [ %next, %latch ]
  %cursor = getelementptr i8, ptr addrspace(1) %bytes, i64 %i
  %field = getelementptr %S, ptr addrspace(1) %cursor, i64 0, i32 0
  %value = load i32, ptr addrspace(1) %field, align 1
  %next = add i64 %i, 1
  br i1 %done, label %exit, label %latch
latch:
  br label %loop
exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"bytes"}
"#;

    let emitted = crate::native::emit_vulkan_spirv_with_outcome(ll, None, Some("k"), None)
        .expect("construct-tree emission remains an owned intermediate");
    assert_eq!(
        emitted.sidecar.ownership_plan_rejected_functions,
        crate::fxhash::set_from(["k".to_string()])
    );
}

#[test]
fn nested_selection_entries_into_one_loop_are_structured_by_construction() {
    let ll = r#"
define void @k() {
entry:
  %outer = icmp eq i32 0, 0
  %inner = icmp eq i32 1, 1
  %done = icmp eq i32 2, 2
  br i1 %outer, label %inner_header, label %outer_else

inner_header:
  br i1 %inner, label %inner_then, label %inner_else

inner_then:
  br label %inner_merge

inner_else:
  br label %loop

inner_merge:
  br label %outer_merge

outer_else:
  br label %loop

loop:
  %i = phi i32 [ 0, %inner_else ], [ 0, %outer_else ], [ %next, %latch ]
  %next = add i32 %i, 1
  br i1 %done, label %loop_exit, label %latch

latch:
  br label %loop

loop_exit:
  br label %outer_merge

outer_merge:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.matches("OpSelectionMerge").count() >= 2, "{asm}");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_selection_loop_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

#[test]
fn loop_exit_into_selection_sibling_is_structured_by_construction() {
    let ll = r#"
define void @k() {
entry:
  %direct = icmp eq i32 0, 1
  %leave = icmp eq i32 1, 1
  %again = icmp eq i32 2, 3
  br label %owner

owner:
  br i1 %direct, label %sibling, label %loop

loop:
  %i = phi i32 [ 0, %owner ], [ %next, %loop_tail ]
  br label %body

body:
  %next = add i32 %i, 1
  br i1 %leave, label %sibling, label %loop_tail

loop_tail:
  br i1 %again, label %loop, label %tail

sibling:
  %value = phi i32 [ 7, %owner ], [ %next, %body ]
  br label %post

tail:
  br label %post

post:
  br label %merge

merge:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpSwitch"), "{asm}");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_loop_exit_sibling_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

#[test]
fn guarded_loop_nest_exiting_into_its_join_stays_four_structured_loops() {
    let ll = r#"
define void @k() {
entry:
  br label %outer

outer:
  %i = phi i32 [ 0, %entry ], [ %i.next, %latch ]
  br label %pre

pre:
  %p = phi i32 [ 0, %outer ], [ %p.next, %pre ]
  %p.next = add i32 %p, 1
  %p.done = icmp eq i32 %p.next, 8
  br i1 %p.done, label %sel, label %pre

sel:
  %guard = icmp ult i32 %i, 2
  br i1 %guard, label %latch, label %mid

mid:
  %j = phi i32 [ 0, %sel ], [ %j.next, %mid.tail ]
  br label %inner

inner:
  %l = phi i32 [ 0, %mid ], [ %l.next, %inner ]
  %l.next = add i32 %l, 1
  %l.done = icmp eq i32 %l.next, 8
  br i1 %l.done, label %mid.tail, label %inner

mid.tail:
  %j.next = add i32 %j, 1
  %j.done = icmp eq i32 %j.next, 16
  br i1 %j.done, label %latch, label %mid

latch:
  %i.next = add i32 %i, 1
  %i.done = icmp eq i32 %i.next, 4
  br i1 %i.done, label %exit, label %outer

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    let loop_merges = asm.matches("OpLoopMerge").count();
    assert_eq!(
        loop_merges, 4,
        "outer, p, j, and l must each own a loop merge; got {loop_merges}\n{asm}"
    );
    assert!(
        !asm.contains("OpSwitch"),
        "no PC-dispatch switch may own the nest\n{asm}"
    );
    assert!(
        !asm.contains("ct.sl"),
        "no regional dispatch wrapper may own the nest\n{asm}"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_guarded_loop_nest_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

#[test]
fn source_unroll_hints_reach_the_header_loop_merge() {
    let ll = r#"
define void @k() {
entry:
  br label %a

a:
  %i = phi i32 [ 0, %entry ], [ %i.next, %a ]
  %i.next = add i32 %i, 1
  %a.done = icmp eq i32 %i.next, 5
  br i1 %a.done, label %mid, label %a, !llvm.loop !3

mid:
  br label %b

b:
  %j = phi i32 [ 0, %mid ], [ %j.next, %b ]
  %j.next = add i32 %j, 1
  %b.done = icmp eq i32 %j.next, 7
  br i1 %b.done, label %done, label %b, !llvm.loop !6

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
!3 = distinct !{!3, !4, !5}
!4 = !{!"llvm.loop.mustprogress"}
!5 = !{!"llvm.loop.unroll.count", i32 3}
!6 = distinct !{!6, !7}
!7 = !{!"llvm.loop.unroll.disable"}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    let merges: Vec<&str> = asm
        .lines()
        .filter(|line| line.contains("OpLoopMerge"))
        .collect();
    assert_eq!(merges.len(), 2, "{asm}");
    assert!(
        merges
            .iter()
            .any(|m| m.contains("PartialCount") && m.trim_end().ends_with('3')),
        "{merges:?}"
    );
    assert!(
        merges.iter().any(|m| m.contains("DontUnroll")),
        "{merges:?}"
    );
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_unroll_hints_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

#[test]
fn break_and_continue_cleanup_switch_threads_into_one_structured_loop() {
    let ll = r#"
define void @k() {
entry:
  %n = mul i32 7, 9
  %z = icmp eq i32 %n, 0
  br i1 %z, label %early, label %pre

early:
  br label %exit

pre:
  br label %head

head:
  %acc = phi i32 [ 0, %pre ], [ %acc.next, %latch ]
  %i = phi i32 [ 0, %pre ], [ %i.next, %latch ]
  %y = phi i32 [ 1, %pre ], [ %y.sel, %latch ]
  %acc3 = mul i32 %acc, 3
  %acc.next = add i32 %acc3, %y
  %x = mul i32 %i, 747796405
  %brk = icmp slt i32 %x, -1900000000
  br i1 %brk, label %cleanup, label %test

test:
  %m = and i32 %x, 3
  %isc = icmp eq i32 %m, 2
  br i1 %isc, label %cont, label %upd

cont:
  br label %cleanup

upd:
  %y7 = mul i32 %y, 7
  %x15 = and i32 %x, 15
  %y.new = add i32 %x15, %y7
  br label %cleanup

cleanup:
  %y.sel = phi i32 [ %y, %cont ], [ %y.new, %upd ], [ %y, %head ]
  %dest = phi i32 [ 4, %cont ], [ 0, %upd ], [ 2, %head ]
  switch i32 %dest, label %exit [
    i32 0, label %latch
    i32 4, label %latch
  ]

latch:
  %via = phi i32 [ %y.sel, %cleanup ], [ %y.sel, %cleanup ]
  %i.step = add nuw i32 %i, 1
  %i.next = add i32 %i.step, %via
  %done = icmp ugt i32 %i.next, %n
  br i1 %done, label %exit, label %head

exit:
  %r = phi i32 [ 0, %early ], [ %acc.next, %latch ], [ %acc.next, %cleanup ]
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    let loop_merges = asm.matches("OpLoopMerge").count();
    assert_eq!(
        loop_merges, 1,
        "the head loop must be the only loop; got {loop_merges}\n{asm}"
    );
    assert!(
        !asm.contains("OpSwitch"),
        "no relooper state switch may own the loop\n{asm}"
    );
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_switch_thread_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

fn function_byte_chains(module: &crate::spirv_module::Module) -> usize {
    let bytes: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| {
            i.class.opcode == Op::TypeInt && i.operands.first() == Some(&Operand::LiteralBit32(8))
        })
        .filter_map(|i| i.result_id)
        .collect();
    let byte_ptrs: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| {
            i.class.opcode == Op::TypePointer
                && i.operands.first() == Some(&Operand::StorageClass(StorageClass::Function))
                && matches!(i.operands.get(1), Some(Operand::IdRef(p)) if bytes.contains(p))
        })
        .filter_map(|i| i.result_id)
        .collect();
    module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| b.instructions.iter()))
        .filter(|i| matches!(i.class.opcode, Op::AccessChain | Op::InBoundsAccessChain))
        .filter(|i| i.result_type.is_some_and(|t| byte_ptrs.contains(&t)))
        .count()
}

#[test]
fn byte_boxes_reached_only_by_byte_chains_become_word_arrays() {
    let ll = r#"
%struct.RS = type { [8 x i8] }

define void @k() {
entry:
  %s = alloca [4 x %struct.RS], align 1
  %t = alloca %struct.RS, align 1
  %n = mul i32 7, 9
  %z0 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 0, i32 0, i64 0
  store i64 0, ptr %z0, align 1
  %z1 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 1, i32 0, i64 0
  store i64 0, ptr %z1, align 1
  %z2 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 2, i32 0, i64 0
  store i64 0, ptr %z2, align 1
  %z3 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 3, i32 0, i64 0
  store i64 0, ptr %z3, align 1
  br label %head

head:
  %i = phi i32 [ 0, %entry ], [ %i.next, %head ]
  %j = and i32 %i, 3
  %j64 = zext i32 %j to i64
  %p0 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 0
  %b0 = load i8, ptr %p0, align 1
  %e0 = zext i8 %b0 to i64
  %p1 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 1
  %b1 = load i8, ptr %p1, align 1
  %e1 = zext i8 %b1 to i64
  %h1 = shl i64 %e1, 8
  %p2 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 2
  %b2 = load i8, ptr %p2, align 1
  %e2 = zext i8 %b2 to i64
  %h2 = shl i64 %e2, 16
  %p3 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 3
  %b3 = load i8, ptr %p3, align 1
  %e3 = zext i8 %b3 to i64
  %h3 = shl i64 %e3, 24
  %p4 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 4
  %b4 = load i8, ptr %p4, align 1
  %e4 = zext i8 %b4 to i64
  %h4 = shl i64 %e4, 32
  %p5 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 5
  %b5 = load i8, ptr %p5, align 1
  %e5 = zext i8 %b5 to i64
  %h5 = shl i64 %e5, 40
  %p6 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 6
  %b6 = load i8, ptr %p6, align 1
  %e6 = zext i8 %b6 to i64
  %h6 = shl i64 %e6, 48
  %p7 = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 %j64, i32 0, i64 7
  %b7 = load i8, ptr %p7, align 1
  %e7 = zext i8 %b7 to i64
  %h7 = shl i64 %e7, 56
  %o1 = or i64 %e0, %h1
  %o2 = or i64 %o1, %h2
  %o3 = or i64 %o2, %h3
  %o4 = or i64 %o3, %h4
  %o5 = or i64 %o4, %h5
  %o6 = or i64 %o5, %h6
  %o7 = or i64 %o6, %h7
  %i64 = zext i32 %i to i64
  %v = add i64 %o7, %i64
  store i64 %v, ptr %p0, align 1
  %i.next = add i32 %i, 1
  %done = icmp eq i32 %i.next, %n
  br i1 %done, label %exit, label %head

exit:
  %q = getelementptr inbounds %struct.RS, ptr %t, i64 0, i32 0, i64 3
  %w = getelementptr inbounds [4 x %struct.RS], ptr %s, i64 0, i64 2, i32 0, i64 5
  %r = load i8, ptr %w, align 1
  store i8 %r, ptr %q, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let module = load_bytes(crate::native::emit_vulkan_spirv(ll).expect("default structured emit"))
        .expect("load native spv");
    let before = function_byte_chains(&module);
    assert!(
        before >= 40,
        "the fixture must reach the byte path it exists for; got {before} uchar chains"
    );
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform");
    let after = function_byte_chains(&out);
    let spv = out
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(
        after, 0,
        "every byte chain must be a word chain after the pass (was {before})\n{asm}"
    );
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_byte_words_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create scratch");
    if let Err(error) = tools::spirv_val_bytes(&spv, &tmp) {
        panic!("producer output must validate: {error}\n{asm}");
    }
    std::fs::remove_dir_all(&tmp).expect("remove scratch");
}

#[test]
fn emission_failure_carries_complete_same_cfg_planner_rejections() {
    let ll = r#"
define void @k() {
entry:
  br label %head
head:
  %conflict = phi i32 [ 0, %entry ], [ 1, %entry ]
  %choose = icmp eq i32 0, 0
  br i1 %choose, label %a, label %b
a:
  br label %head
b:
  br label %head
}
"#;

    let failure = crate::native::emit_vulkan_spirv_with_outcome(ll, None, Some("k"), None)
        .expect_err("conflicting phi must fail after CFG planning");
    assert!(
        failure
            .error
            .contains("phi %conflict has multiple values from predecessor %entry"),
        "{}",
        failure.error
    );
    assert_eq!(
        failure.rejected.ordinary_plan_functions,
        crate::fxhash::set_from(["k".to_string()])
    );
    assert_eq!(
        failure.rejected.ownership_plan_functions,
        crate::fxhash::set_from(["k".to_string()])
    );
}

#[test]
fn native_kernel_drops_all_metadata_void_call_structurally() {
    let ll = r#"
declare void @llvm.experimental.noalias.scope.decl(metadata) #0
declare void @some.unknown.debug.marker(metadata, metadata) #0

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %in) {
entry:
  call void @llvm.experimental.noalias.scope.decl(metadata !10)
  call void @some.unknown.debug.marker(metadata !10, metadata !11)
  %v = load i32, ptr addrspace(1) %in, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

attributes #0 = { nounwind }

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"in"}
!10 = !{!10}
!11 = !{!11}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_metadata_void_call_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn local_array_pointer_induction_phi_compares_by_index() {
    let ll = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  %arr = alloca [2 x i32], align 4
  %first = getelementptr inbounds [2 x i32], ptr %arr, i64 0, i64 0
  store i32 7, ptr %first, align 4
  %end = getelementptr inbounds [2 x i32], ptr %arr, i64 0, i64 1
  br label %loop

loop:
  %p = phi ptr [ %first, %entry ], [ %next, %loop ]
  %v = load i32, ptr %p, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  %next = getelementptr inbounds i32, ptr %p, i64 1
  %done = icmp eq ptr %next, %end
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_pointer_induction_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_fmax3_fmin3_and_fmedian3_lower_as_nested_binary_extinsts() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %mx = tail call fast float @air.fast_fmax3.f32(float 1.000000e+00, float 2.000000e+00, float 3.000000e+00)
  %mn = tail call fast float @air.fast_fmin3.f32(float %mx, float 4.000000e+00, float 5.000000e+00)
  %md = tail call fast float @air.fast_fmedian3.f32(float %mx, float %mn, float 6.000000e+00)
  ret void
}

declare float @air.fast_fmax3.f32(float, float, float)
declare float @air.fast_fmin3.f32(float, float, float)
declare float @air.fast_fmedian3.f32(float, float, float)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_minmax3_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    let fmax_count = asm
        .lines()
        .filter(|line| line.contains("OpExtInst") && line.contains(" FMax "))
        .count();
    let fmin_count = asm
        .lines()
        .filter(|line| line.contains("OpExtInst") && line.contains(" FMin "))
        .count();
    assert_eq!(fmax_count, 4, "{asm}");
    assert_eq!(fmin_count, 4, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

fn pure_latch_trampolines(module: &crate::spirv_module::Module) -> usize {
    let id_of = |operand: Option<&Operand>| match operand {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    };
    let mut count = 0;
    for function in &module.functions {
        let loops = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| instruction.class.opcode == Op::LoopMerge)
            .filter_map(|instruction| {
                Some((
                    id_of(instruction.operands.get(1))?,
                    id_of(instruction.operands.first())?,
                ))
            })
            .collect::<HashMap<Word, Word>>();
        let by_label = function
            .blocks
            .iter()
            .filter_map(|block| Some((block.label.as_ref()?.result_id?, block)))
            .collect::<HashMap<Word, &Block>>();
        for block in &function.blocks {
            let [.., merge, branch] = block.instructions.as_slice() else {
                continue;
            };
            if merge.class.opcode != Op::SelectionMerge
                || branch.class.opcode != Op::BranchConditional
            {
                continue;
            }
            let (Some(s), Some(t), Some(f)) = (
                id_of(merge.operands.first()),
                id_of(branch.operands.get(1)),
                id_of(branch.operands.get(2)),
            ) else {
                continue;
            };
            let other = if t == s {
                f
            } else if f == s {
                t
            } else {
                continue;
            };
            let Some(&m) = loops.get(&other) else {
                continue;
            };
            let pure = by_label.get(&s).is_some_and(|tramp| {
                matches!(
                    tramp.instructions.as_slice(),
                    [only] if only.class.opcode == Op::Branch && only.operands == [Operand::IdRef(m)]
                )
            });
            count += usize::from(pure);
        }
    }
    count
}

#[test]
fn native_do_while_guarded_break_latch_branches_straight_to_loop_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %x) {
entry:
  br label %h

h:
  %i = phi i32 [ 0, %entry ], [ %n, %latch ]
  br label %s

s:
  %c = icmp slt i32 %i, %x
  br i1 %c, label %body, label %m

body:
  %n = add i32 %i, 1
  br label %latch

latch:
  %d = icmp slt i32 %n, 16
  br i1 %d, label %h, label %m

m:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_latch_trampoline_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let emitted = load_bytes(&spv).expect("load native spv");
    let before = pure_latch_trampolines(&emitted);
    assert!(
        before >= 1,
        "precondition: the emitter no longer makes the private latch trampoline, so this test \
         measures nothing:\n{}",
        disassemble(&spv).unwrap_or_default()
    );
    let out = passes::transform(emitted, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    let after = pure_latch_trampolines(&load_bytes(&out).expect("load transformed"));
    assert_eq!(
        after, 0,
        "{before} latch trampoline(s) emitted, {after} still reach the loop merge after \
         transform:\n{asm}"
    );
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_latch_can_precede_phi_header() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

latch:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 4
  br i1 %done, label %exit, label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %latch ]
  br label %latch

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_forward_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_implicit_entry_with_mixed_numeric_params_resolves_phi_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @main(i32 %0, i32 %named, i32 %6) {
  br label %8

8:
  %9 = phi i32 [ %0, %7 ]
  ret i32 %9
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpReturnValue"), "{asm}");
}

#[test]
fn native_loop_continue_branch_does_not_reuse_entry_branch_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %x) {
entry:
  %enter = icmp slt i32 %x, 4
  br i1 %enter, label %loop, label %exit

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %done = icmp eq i32 %i, 4
  br i1 %done, label %exit, label %body

body:
  br label %cont

cont:
  %next = add i32 %i, 1
  %again = icmp slt i32 %i, 3
  br i1 %again, label %loop, label %exit

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_continue_branch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_body_branch_merges_at_continue_target() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %x) {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %skip = icmp eq i32 %i, 3
  br i1 %skip, label %cont, label %body

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 4
  br i1 %done, label %exit, label %loop

body:
  %choose = icmp eq i32 %x, 0
  br i1 %choose, label %right, label %left

left:
  %left_cond = icmp eq i32 %i, 1
  br i1 %left_cond, label %left_set, label %cont

left_set:
  br label %cont

right:
  %right_cond = icmp eq i32 %i, 2
  br i1 %right_cond, label %right_set, label %cont

right_set:
  br label %cont

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_body_continue_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert_eq!(asm.matches("OpBranchConditional").count(), 5, "{asm}");
    assert_eq!(asm.matches("OpSelectionMerge").count(), 4, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_merge_block_can_precede_loop_textually() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

exit:
  ret void

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  br label %body

body:
  br label %cont

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 3
  br i1 %done, label %exit, label %loop
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_early_loop_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_merge_repair_rechecks_single_predecessor_targets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

merge:
  br i1 true, label %target, label %exit

target:
  br label %exit

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  br label %cont

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 3
  br i1 %done, label %merge, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_merge_single_pred_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert_eq!(
        asm.matches("OpSelectionMerge").count(),
        usize::from(crate::env_vars::no_latch_trampoline_fold()),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_switch_loop_header_splits_switch_under_loop_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  switch i32 %i, label %cont [
    i32 0, label %case0
    i32 1, label %case1
  ]

case0:
  br label %cont

case1:
  br label %cont

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 3
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_loop_header_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(!asm.contains("OpSwitch"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_exit_can_precede_value_defining_body() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @main(float %x) {
entry:
  br label %loop

exit:
  %sum = fadd fast float %acc.next, %x
  ret float %sum

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %body ]
  %acc = phi float [ 0.000000e+00, %entry ], [ %acc.next, %body ]
  br label %body

body:
  %acc.next = fadd fast float %acc, %x
  %next = add nuw nsw i32 %i, 1
  %done = icmp eq i32 %next, 4
  br i1 %done, label %exit, label %loop
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_forward_body_value_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_entry_metadata_gep_keeps_member_isomorphic_padding_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Constants = type <{ i32, i32, i32, i32, i32, i32, i32, i32, i32, [4 x i8], <2 x i32> }>

define void @main(ptr addrspace(2) %constants) {
entry:
  %face_dims = getelementptr inbounds %Constants, ptr addrspace(2) %constants, i64 0, i32 10
  %value = load <2 x i32>, ptr addrspace(2) %face_dims, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 48, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 48, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"constants_t", !"air.arg_name", !"constants"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"f0", i32 4, i32 4, i32 0, !"uint", !"f1", i32 8, i32 4, i32 0, !"uint", !"f2", i32 12, i32 4, i32 0, !"uint", !"f3", i32 16, i32 4, i32 0, !"uint", !"f4", i32 20, i32 4, i32 0, !"uint", !"f5", i32 24, i32 4, i32 0, !"uint", !"f6", i32 28, i32 4, i32 0, !"uint", !"f7", i32 32, i32 4, i32 0, !"uint", !"f8", i32 40, i32 8, i32 0, !"uint2", !"face_dims"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_metadata_gep_padding_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let uint_ty = uint32_type_id(&asm);
    assert!(asm.contains(&format!("OpConstant  {uint_ty}  10")), "{asm}");
    assert!(!asm.contains(&format!("OpConstant  {uint_ty}  9")), "{asm}");
    let module = load_bytes(&spv).expect("load spv");
    let eleven_member_struct = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeStruct && inst.operands.len() == 11)
        .and_then(|inst| inst.result_id)
        .unwrap_or_else(|| panic!("eleven-member constants struct\n{asm}"));
    let face_dims_offset = module.annotations.iter().find_map(|inst| {
        if inst.class.opcode != Op::MemberDecorate {
            return None;
        }
        match inst.operands.as_slice() {
            [Operand::IdRef(target), Operand::LiteralBit32(10), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(offset)]
                if *target == eleven_member_struct =>
            {
                Some(*offset)
            }
            _ => None,
        }
    });
    assert_eq!(face_dims_offset, Some(40), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_record_metadata_gep_remaps_matrix_padding_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Top = type { %Conv, %Conv, %Conv, %Conv }
%Conv = type <{ i32, [12 x i8], %Matrix, i8, [15 x i8] }>
%Matrix = type { [3 x <3 x float>] }

define void @main(ptr addrspace(2) %constants, ptr addrspace(1) %out) {
entry:
  %row = getelementptr inbounds %Top, ptr addrspace(2) %constants, i64 0, i32 0, i32 2, i32 0, i64 0
  %value = load <3 x float>, ptr addrspace(2) %row, align 16
  %x = extractelement <3 x float> %value, i64 0
  store float %x, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 320, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 320, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Top", !"air.arg_name", !"constants"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 80, i32 0, !"Conv", !"c0", !"air.struct_type_info", !5, i32 80, i32 80, i32 0, !"Conv", !"c1", !"air.struct_type_info", !5, i32 160, i32 80, i32 0, !"Conv", !"c2", !"air.struct_type_info", !5, i32 240, i32 80, i32 0, !"Conv", !"c3"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"transfer", i32 16, i32 48, i32 0, !"float3x3", !"matrix", i32 64, i32 1, i32 0, !"bool", !"flag"}
!6 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_metadata_matrix_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpInBoundsAccessChain") && line.contains("%uint_2 %uint_0 %uint_0")
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_loop_carried_byte_offset_pointer_phi_stores_to_buffer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(i32 %byte_off, ptr addrspace(1) %out) {
entry:
  %off64 = zext i32 %byte_off to i64
  %row = getelementptr inbounds i8, ptr addrspace(1) %out, i64 %off64
  br label %loop

loop:
  %p = phi ptr addrspace(1) [ %row, %entry ], [ %next, %loop ]
  %i = phi i32 [ 0, %entry ], [ %inc, %loop ]
  store float 1.000000e+00, ptr addrspace(1) %p, align 4
  %next = getelementptr inbounds float, ptr addrspace(1) %p, i64 1
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, 2
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"byte_off"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_byte_phi_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let out_var = module
        .annotations
        .iter()
        .find_map(|inst| {
            let [
                Operand::IdRef(id),
                Operand::Decoration(Decoration::Binding),
                Operand::LiteralBit32(1),
            ] = inst.operands.as_slice()
            else {
                return None;
            };
            (variable_storage_class(&module, *id) == Some(StorageClass::StorageBuffer))
                .then_some(*id)
        })
        .expect("binding 1 storage buffer");
    let storage_ptrs = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            (inst.class.opcode == Op::InBoundsAccessChain
                && inst.operands.first() == Some(&Operand::IdRef(out_var)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|ptr| storage_ptrs.contains(&ptr))),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_memcpy_from_void_buffer_to_nested_struct_alloca_copies_raw_leaves() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Inner = type { [3 x float], float }
%Outer = type { i32, %Inner, i32 }

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %tmp = alloca %Outer, align 4
  %tmpc = bitcast ptr %tmp to ptr
  %srcc = bitcast ptr addrspace(1) %src to ptr addrspace(1)
  call void @llvm.memcpy.p0.p1.i64(ptr %tmpc, ptr addrspace(1) %srcc, i64 24, i1 false)
  %leafp = getelementptr inbounds %Outer, ptr %tmp, i64 0, i32 1, i32 0, i64 1
  %leaf = load float, ptr %leafp, align 4
  %outc = bitcast ptr addrspace(1) %out to ptr addrspace(1)
  store float %leaf, ptr addrspace(1) %outc, align 4
  ret void
}

declare void @llvm.memcpy.p0.p1.i64(ptr, ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"void", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_raw_to_typed_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module =
        load_bytes(super::super::emit_vulkan_spirv_all_buffers_raw(ll).expect("native raw emit"))
            .expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(
        asm.matches("OpStore").count() >= 7,
        "expected the nested copy to lower to per-leaf stores: {asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_i8_buffer_bitcast_vector_load_uses_raw_phi() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %bytes, ptr addrspace(1) %out, i32 %limit) {
entry:
  %start = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 0
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %start, %entry ], [ %next, %loop ]
  %i = phi i32 [ 0, %entry ], [ %inc, %loop ]
  %wide = bitcast ptr addrspace(1) %cursor to ptr addrspace(1)
  %value = load <2 x i32>, ptr addrspace(1) %wide, align 8
  store <2 x i32> %value, ptr addrspace(1) %out, align 8
  %next = getelementptr inbounds i8, ptr addrspace(1) %cursor, i64 8
  %inc = add nuw i32 %i, 1
  %done = icmp eq i32 %inc, %limit
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"bytes"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"uint2", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"limit"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_loop_i8_buffer_bitcast_vector_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let pointer_bitcasts = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Bitcast
                && inst
                    .result_type
                    .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    assert_eq!(pointer_bitcasts, 0, "{asm}");
    let v2u32_loads = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Load
                && inst
                    .result_type
                    .is_some_and(|ty| is_unsigned_int_vector(&module, ty, 32, 2))
        })
        .count();
    assert_eq!(v2u32_loads, 0, "{asm}");
    let v2u32_constructs = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::CompositeConstruct
                && inst
                    .result_type
                    .is_some_and(|ty| is_unsigned_int_vector(&module, ty, 32, 2))
        })
        .count();
    assert!(v2u32_constructs >= 1, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_atomic_float_slot_retypes_for_i32_cas_loop() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %slot, i32 %idx) {
entry:
  %idx64 = zext i32 %idx to i64
  %p = getelementptr inbounds float, ptr addrspace(1) %slot, i64 %idx64
  %bits = tail call i32 @air.atomic.global.load.i32(ptr addrspace(1) %p, i32 0, i32 2, i1 true)
  %cur = bitcast i32 %bits to float
  %next_f = fadd fast float %cur, 1.000000e+00
  %next = bitcast float %next_f to i32
  %compare = alloca i32, align 4
  store i32 %bits, ptr %compare, align 4
  %old = call i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1) %p, ptr %compare, i32 %next, i32 0, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.load.i32(ptr addrspace(1), i32, i32, i1)
declare i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1), ptr, i32, i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"slot"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_device_atomic_float_slot_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(asm.contains("OpAtomicCompareExchange"), "{asm}");
    assert!(
        !asm.contains("OpVariable %_ptr_Workgroup_uint Workgroup"),
        "{asm}"
    );
    assert_no_pointer_bitcasts(&spv);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_scalarized_struct_phi_gep_keeps_scalar_pointer_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Atomic = type { i32 }
@tg = internal addrspace(3) global [128 x i32] undef, align 4

define void @k(ptr addrspace(1) %out) {
entry:
  %slot0 = getelementptr inbounds i32, ptr addrspace(3) @tg, i64 0
  %base = bitcast ptr addrspace(3) %slot0 to ptr addrspace(3)
  br label %loop

loop:
  %p = phi ptr addrspace(3) [ %base, %entry ], [ %next, %loop ]
  %i = phi i32 [ 0, %entry ], [ %inc, %loop ]
  %field = getelementptr inbounds %Atomic, ptr addrspace(3) %p, i64 0, i32 0
  %v = load i32, ptr addrspace(3) %field, align 1
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i32 %i
  store i32 %v, ptr addrspace(1) %dst, align 4
  %next = getelementptr inbounds %Atomic, ptr addrspace(3) %p, i64 1
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, 2
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_scalarized_struct_phi_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.lines().any(|line| line.contains("OpPhi")), "{asm}");
    let scalar_ptr_id = asm.lines().find_map(|line| {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        (parts.len() >= 5
            && parts[2] == "OpTypePointer"
            && parts[3] == "Workgroup"
            && parts[4] == "%2")
            .then_some(parts[0])
    });
    if let Some(scalar_ptr_id) = scalar_ptr_id {
        let scalar_phi_ids = asm
            .lines()
            .filter_map(|line| {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                (parts.len() >= 4 && parts[2] == "OpPhi" && parts[3] == scalar_ptr_id)
                    .then_some(parts[0])
            })
            .collect::<Vec<_>>();
        assert!(
            !asm.lines().any(|line| {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                parts.len() >= 5
                    && parts[2] == "OpInBoundsAccessChain"
                    && parts[3] == scalar_ptr_id
                    && scalar_phi_ids.contains(&parts[4])
            }),
            "{asm}"
        );
    }
    let struct_id = asm.lines().find_map(|line| {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        (parts.len() >= 4 && parts[2] == "OpTypeStruct" && parts[3] == "%2").then_some(parts[0])
    });
    if let Some(struct_id) = struct_id {
        let struct_ptr_id = asm.lines().find_map(|line| {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            (parts.len() >= 5
                && parts[2] == "OpTypePointer"
                && parts[3] == "Workgroup"
                && parts[4] == struct_id)
                .then_some(parts[0])
        });
        if let Some(struct_ptr_id) = struct_ptr_id {
            assert!(
                !asm.lines().any(|line| line.contains("OpPtrAccessChain")
                    && line.split_whitespace().nth(3) == Some(struct_ptr_id)),
                "{asm}"
            );
        }
    }
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_scalarized_struct_phi_rewrites_param_root_access() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Atomic = type { i32 }

define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out) {
entry:
  %slot0 = getelementptr inbounds i32, ptr addrspace(3) %scratch, i64 0
  %base = bitcast ptr addrspace(3) %slot0 to ptr addrspace(3)
  br label %loop

loop:
  %p = phi ptr addrspace(3) [ %base, %entry ], [ %next, %loop ]
  %i = phi i32 [ 0, %entry ], [ %inc, %loop ]
  %field = getelementptr inbounds %Atomic, ptr addrspace(3) %p, i64 0, i32 0
  %v = load i32, ptr addrspace(3) %field, align 1
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i32 %i
  store i32 %v, ptr addrspace(1) %dst, align 4
  %next = getelementptr inbounds %Atomic, ptr addrspace(3) %p, i64 1
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, 2
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_scalarized_struct_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpPtrAccessChain %_ptr_Workgroup_uint")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_pointer_bitcast_load_reads_low_word_from_nested_i64_aggregate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%union.ETCBlock = type { %struct.Bits }
%struct.Bits = type { i64 }
define i32 @load_low_word(ptr %block) {
entry:
  %field = getelementptr inbounds %union.ETCBlock, ptr %block, i64 0, i32 0, i32 0
  %alias = bitcast ptr %block to ptr
  %word = load i32, ptr %alias, align 4
  ret i32 %word
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(!asm.contains("non-bitcastable"), "{asm}");
}

#[test]
fn native_byte_view_multiroot_phi_scalar_and_vector_load_byte_assemble() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
define void @byteview(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, ptr addrspace(2) %ctl) {
entry:
  %c = load i32, ptr addrspace(2) %ctl, align 4
  %o64 = sext i32 %c to i64
  %cmp = icmp eq i32 %c, 0
  br i1 %cmp, label %t, label %e
t:
  br label %m
e:
  br label %m
m:
  %p = phi ptr addrspace(1) [ %a, %t ], [ %b, %e ]
  %byte = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %o64
  %alias = bitcast ptr addrspace(1) %byte to ptr addrspace(1)
  %hp = getelementptr inbounds half, ptr addrspace(1) %alias, i64 %o64
  %h = load half, ptr addrspace(1) %hp, align 2
  %hf = fpext half %h to float
  %fp = getelementptr inbounds float, ptr addrspace(1) %alias, i64 %o64
  %v = load float, ptr addrspace(1) %fp, align 4
  %vp = getelementptr inbounds <4 x float>, ptr addrspace(1) %alias, i64 %o64
  %vv = load <4 x float>, ptr addrspace(1) %vp, align 16
  %e0 = extractelement <4 x float> %vv, i64 0
  %sum0 = fadd float %v, %e0
  %sum = fadd float %sum0, %hf
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @byteview, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"ctl"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_byte_view_multiroot_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains(" OpBitcast ") && line.contains("_ptr_")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_single_predecessor_pointer_alias_preserves_mixed_buffer_views() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
define void @mixed_alias(ptr addrspace(1) %src, ptr addrspace(1) %out, i32 %selector) {
entry:
  switch i32 %selector, label %merge [ i32 0, label %merge ]
merge:
  %p = phi ptr addrspace(1) [ %src, %entry ], [ %src, %entry ]
  %offset = zext i32 %selector to i64
  %byte = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %offset
  %byte_alias = bitcast ptr addrspace(1) %byte to ptr addrspace(1)
  %fp = getelementptr inbounds float, ptr addrspace(1) %byte_alias, i64 0
  %scalar = load float, ptr addrspace(1) %fp, align 4
  %vp = getelementptr inbounds <4 x float>, ptr addrspace(1) %p, i64 %offset
  %vector = load <4 x float>, ptr addrspace(1) %vp, align 16
  %lane = extractelement <4 x float> %vector, i64 0
  %sum = fadd float %scalar, %lane
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @mixed_alias, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"selector"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_mixed_pointer_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let uchar = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands == [Operand::LiteralBit32(8), Operand::LiteralBit32(0)]
        })
        .and_then(|inst| inst.result_id);
    assert!(
        !asm.lines().any(|line| line.contains("OpPhi %_ptr")),
        "{asm}"
    );
    assert!(
        uchar.is_none_or(|uchar| module.functions.iter().all(|function| function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .all(|inst| inst.class.opcode != Op::Load || inst.result_type != Some(uchar)))),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_raw_phi_mixes_word_and_byte_indices_for_device_atomic() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @nested_raw_atomic(ptr addrspace(1) %out, i1 %take_inner) {
entry:
  br i1 %take_inner, label %inner_entry, label %direct

direct:
  br label %join

inner_entry:
  br label %inner

inner:
  %inner_i = phi i32 [ 0, %inner_entry ], [ %next_inner_i, %step ]
  %inner_ptr = phi ptr addrspace(1) [ %out, %inner_entry ], [ %inner_next, %step ]
  br label %step

step:
  %inner_next = getelementptr inbounds float, ptr addrspace(1) %inner_ptr, i64 1
  %next_inner_i = add nuw i32 %inner_i, 1
  %inner_done = icmp eq i32 %next_inner_i, 1
  br i1 %inner_done, label %join, label %inner

join:
  %final_ptr = phi ptr addrspace(1) [ %out, %direct ], [ %inner_next, %step ]
  %old = tail call i32 @air.atomic.global.load.i32(ptr addrspace(1) %final_ptr, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.load.i32(ptr addrspace(1), i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @nested_raw_atomic, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_mixed_phi_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module =
        load_bytes(super::super::emit_vulkan_spirv_all_buffers_raw(ll).expect("native raw emit"))
            .expect("load native spv");
    let spv = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(
        !asm.contains("OpVariable %_ptr_Workgroup_uint Workgroup"),
        "atomic must retain the raw StorageBuffer cursor:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_pointer_phi_accepts_aligned_dynamic_byte_offset() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @walk_raw_buffer(ptr addrspace(2) %p, i1 %wide, i32 %limit) {
entry:
  %tag = load i32, ptr addrspace(2) %p
  %step32 = select i1 %wide, i32 16, i32 36
  %step = zext i32 %step32 to i64
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next_i, %loop ]
  %cursor = phi ptr addrspace(2) [ %p, %entry ], [ %next, %loop ]
  %v = load i32, ptr addrspace(2) %cursor
  %byte = getelementptr inbounds i8, ptr addrspace(2) %cursor, i64 %step
  %next = bitcast ptr addrspace(2) %byte to ptr addrspace(2)
  %next_i = add i32 %i, 1
  %done = icmp eq i32 %next_i, %limit
  br i1 %done, label %exit, label %loop

exit:
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_reinterpret_mix_uses_selective_raw_roots_without_retry() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
define void @k(ptr addrspace(2) %floats, ptr addrspace(2) %halves, ptr addrspace(1) %out, ptr addrspace(2) %control) {
entry:
  %flag = load i32, ptr addrspace(2) %control, align 4
  %cond = icmp eq i32 %flag, 0
  %float_root = getelementptr float, ptr addrspace(2) %floats, i64 0
  %half_root = getelementptr half, ptr addrspace(2) %halves, i64 0
  %float_view = bitcast ptr addrspace(2) %float_root to ptr addrspace(2)
  %half_view = bitcast ptr addrspace(2) %half_root to ptr addrspace(2)
  %selected = select i1 %cond, ptr addrspace(2) %half_view, ptr addrspace(2) %float_view
  br i1 %cond, label %left, label %right

left:
  %left_ptr = getelementptr <4 x float>, ptr addrspace(2) %selected, i64 1
  br label %merge

right:
  %right_ptr = getelementptr <4 x float>, ptr addrspace(2) %selected, i64 2
  br label %merge

merge:
  %source = phi ptr addrspace(2) [ %left_ptr, %left ], [ %right_ptr, %right ]
  %value = load <4 x float>, ptr addrspace(2) %source, align 16
  store <4 x float> %value, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.function_constant", !7, !"air.buffer", !"air.location_index", i32 29, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"floats"}
!4 = !{i32 1, !"air.function_constant", !8, !"air.buffer", !"air.location_index", i32 29, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"halves"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"control"}
!7 = !{ptr addrspace(2) @float_enabled, !"bool", !"float_enabled"}
!8 = !{ptr addrspace(2) @half_enabled, !"bool", !"half_enabled"}
"#;
    let parsed = super::super::ir::LlModule::parse(ll).expect("parse AIR");
    assert_eq!(
        parsed.metadata_fc_buffer_locations,
        crate::fxhash::map_from([
            (("k".to_string(), "%floats".to_string()), 29),
            (("k".to_string(), "%halves".to_string()), 29),
        ])
    );
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(!asm.contains("PhysicalStorageBuffer"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    assert!(!module.all_inst_iter().any(|inst| {
        inst.class.opcode == Op::Bitcast
            && inst
                .result_type
                .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
    }));
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reinterpret_mix_selective_raw_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_modeled_pointer_phi_materializes_gep_backedge_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @walk_modeled_float(ptr addrspace(1) %p, i32 %base, i32 %limit) {
entry:
  %start = getelementptr inbounds float, ptr addrspace(1) %p, i32 %base
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %start, %entry ], [ %next, %body ]
  %i = phi i32 [ 0, %entry ], [ %inc, %body ]
  %v = load float, ptr addrspace(1) %cursor
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, %limit
  br i1 %done, label %exit, label %body

body:
  %next = getelementptr inbounds float, ptr addrspace(1) %cursor, i32 32
  br label %loop

exit:
  ret float %v
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpCopyObject"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let defined = module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let missing = module
        .all_inst_iter()
        .flat_map(|inst| inst.operands.iter())
        .filter_map(|op| match op {
            Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => Some(*id),
            _ => None,
        })
        .filter(|id| !defined.contains(id))
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "missing ids {missing:?}\n{asm}");
}

#[test]
fn native_function_pointer_phi_is_an_index_phi_by_construction() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @walk_private(i32 %limit) {
entry:
  %values = alloca [8 x float], align 16
  %start = getelementptr inbounds [8 x float], ptr %values, i32 0, i32 0
  br label %loop

loop:
  %cursor = phi ptr [ %start, %entry ], [ %next, %body ]
  %i = phi i32 [ 0, %entry ], [ %inc, %body ]
  %value = load float, ptr %cursor, align 4
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, %limit
  br i1 %done, label %exit, label %body

body:
  %next = getelementptr inbounds float, ptr %cursor, i32 1
  br label %loop

exit:
  ret float %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    let function_pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer
                && instruction.operands.first()
                    == Some(&Operand::StorageClass(StorageClass::Function)))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Phi
            && instruction
                .result_type
                .is_some_and(|ty| !function_pointer_types.contains(&ty))
    }));
    assert!(!module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Phi
            && instruction
                .result_type
                .is_some_and(|ty| function_pointer_types.contains(&ty))
    }));
    for block in &module.functions.first().expect("function").blocks {
        let phi_count = block
            .instructions
            .iter()
            .take_while(|instruction| instruction.class.opcode == Op::Phi)
            .count();
        assert!(block.instructions[phi_count..]
            .iter()
            .all(|instruction| instruction.class.opcode != Op::Phi));
    }
}

#[test]
fn native_multi_index_function_pointer_phi_is_built_from_differing_index_phis() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @choose_private(i1 %choose) {
entry:
  %values = alloca [2 x [4 x float]], align 16
  %left = getelementptr inbounds [2 x [4 x float]], ptr %values, i32 0, i32 0, i32 1
  %right = getelementptr inbounds [2 x [4 x float]], ptr %values, i32 0, i32 1, i32 2
  br i1 %choose, label %take_left, label %take_right

take_left:
  br label %merge

take_right:
  br label %merge

merge:
  %cursor = phi ptr [ %left, %take_left ], [ %right, %take_right ]
  %value = load float, ptr %cursor, align 4
  %again = load float, ptr %cursor, align 4
  %sum = fadd float %value, %again
  ret float %sum
}
"#;
    // Two loads through the phi: ptr_phi_load rewrites only a phi whose single use is one load, so this
    // shape still reaches the index-phi construction this test exists for.
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    let function_pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer
                && instruction.operands.first()
                    == Some(&Operand::StorageClass(StorageClass::Function)))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Phi
            && instruction
                .result_type
                .is_some_and(|ty| function_pointer_types.contains(&ty))
    }));
    let integer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypeInt).then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        module
            .all_inst_iter()
            .filter(|instruction| {
                instruction.class.opcode == Op::Phi
                    && instruction
                        .result_type
                        .is_some_and(|ty| integer_types.contains(&ty))
            })
            .count(),
        2
    );
}

#[test]
fn native_workgroup_phi_materializes_incoming_pointers_on_predecessor_edges() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @merge_workgroup(ptr addrspace(3) %p, i1 %choose) {
entry:
  br i1 %choose, label %left, label %right

left:
  br label %merge

right:
  br label %merge

merge:
  %selected = phi ptr addrspace(3) [ %p, %left ], [ %p, %right ]
  store i32 7, ptr addrspace(3) %selected, align 4
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    let function = module.functions.first().expect("function");
    let mut definitions = HashMap::default();
    for block in &function.blocks {
        let label = block
            .label
            .as_ref()
            .and_then(|label| label.result_id)
            .expect("block label");
        let mut saw_non_phi = false;
        for inst in &block.instructions {
            if inst.class.opcode == Op::Phi {
                assert!(!saw_non_phi, "phi after ordinary instruction: {block:?}");
            } else {
                saw_non_phi = true;
            }
            if let Some(result) = inst.result_id {
                definitions.insert(result, (label, inst.class.opcode));
            }
        }
    }

    let mut materialized_incomings = 0;
    for block in &function.blocks {
        for phi in block
            .instructions
            .iter()
            .filter(|inst| inst.class.opcode == Op::Phi)
        {
            for pair in phi.operands.chunks(2) {
                let [Operand::IdRef(value), Operand::IdRef(predecessor)] = pair else {
                    continue;
                };
                let Some((definition_block, opcode)) = definitions.get(value) else {
                    continue;
                };
                assert_eq!(definition_block, predecessor, "{phi:?}");
                if matches!(
                    opcode,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) {
                    materialized_incomings += 1;
                }
            }
        }
    }
    assert_eq!(materialized_incomings, 2);
}

#[test]
fn native_structured_merges_are_constructed_next_to_their_terminators() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @selection_and_loop(i1 %choose, i32 %limit, float %x) {
entry:
  %neg = fneg float %x
  br i1 %choose, label %left, label %right

left:
  br label %join

right:
  br label %join

join:
  %initial = phi float [ %x, %left ], [ %neg, %right ]
  br label %loop

loop:
  %i = phi i32 [ 0, %join ], [ %next, %body ]
  %value = phi float [ %initial, %join ], [ %sum, %body ]
  %done = icmp eq i32 %i, %limit
  br i1 %done, label %exit, label %body

body:
  %next = add i32 %i, 1
  %sum = fadd float %value, %x
  br label %loop

exit:
  ret float %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    let mut merge_count = 0;
    for block in &module.functions.first().expect("function").blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            if !matches!(instruction.class.opcode, Op::SelectionMerge | Op::LoopMerge) {
                continue;
            }
            merge_count += 1;
            assert_eq!(index + 2, block.instructions.len(), "{block:?}");
        }
    }
    assert_eq!(merge_count, 2);
}

#[test]
fn native_pointer_phi_uses_zero_index_for_root_bypass() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @merge_with_root_bypass(ptr addrspace(1) %p, i1 %advance, i1 %again) {
entry:
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %p, %entry ], [ %next, %body ]
  br i1 %advance, label %body, label %merge

body:
  %next = getelementptr inbounds float, ptr addrspace(1) %cursor, i64 4
  br i1 %again, label %loop, label %merge

merge:
  %chosen = phi ptr addrspace(1) [ %next, %body ], [ %cursor, %loop ]
  %v = load float, ptr addrspace(1) %chosen
  ret float %v
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let defined = module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let missing = module
        .all_inst_iter()
        .flat_map(|inst| inst.operands.iter())
        .filter_map(|op| match op {
            Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => Some(*id),
            _ => None,
        })
        .filter(|id| !defined.contains(id))
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "missing ids {missing:?}\n{asm}");
}

#[test]
fn native_pointer_phi_undef_arm_merges_branch_local_dynamic_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Buffer = type { [64 x float] }

define float @main(ptr addrspace(1) %buffer, i1 %present, i64 %base) {
entry:
  br i1 %present, label %concrete, label %merge

concrete:
  %branch.index = add i64 %base, 1
  %pointer = getelementptr inbounds %Buffer, ptr addrspace(1) %buffer, i64 0, i32 0, i64 %branch.index
  br label %merge

merge:
  %selected = phi ptr addrspace(1) [ %pointer, %concrete ], [ undef, %entry ]
  %value = load float, ptr addrspace(1) %selected, align 4
  ret float %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let mut module = load_bytes(&spv).expect("load native spv");
    module.capabilities.push(Instruction::new(
        Op::Capability,
        None,
        None,
        vec![Operand::Capability(Capability::Linkage)],
    ));
    let failure = super::super::owned_module_failure(&module).map(|failure| match failure {
        super::super::OwnedModuleFailure::Invalid(error)
        | super::super::OwnedModuleFailure::CfgConstruction(error)
        | super::super::OwnedModuleFailure::TypeConstruction(error)
        | super::super::OwnedModuleFailure::RawBufferConstruction(error) => error,
    });
    assert!(
        failure.is_none(),
        "{failure:?}\n{}",
        disassemble(&spv).expect("disassemble")
    );
}

#[test]
fn native_buffer_pointer_phi_root_and_gep_backedge_share_result_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Atomic = type { i32 }

define void @k(ptr addrspace(1) %histogram, i32 %stride) {
entry:
  %warm_index = zext i32 %stride to i64
  %warm_slot = getelementptr inbounds %Atomic, ptr addrspace(1) %histogram, i64 %warm_index, i32 0
  %warm = tail call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %warm_slot, i32 1, i32 0, i32 2, i1 true)
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %histogram, %entry ], [ %next, %body ]
  %i = phi i32 [ 0, %entry ], [ %inc, %body ]
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds %Atomic, ptr addrspace(1) %cursor, i64 %idx, i32 0
  %old = tail call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %slot, i32 1, i32 0, i32 2, i1 true)
  %inc = add nuw i32 %i, 1
  %done = icmp eq i32 %inc, 2
  br i1 %done, label %exit, label %body

body:
  %step = zext i32 %stride to i64
  %next = getelementptr inbounds %Atomic, ptr addrspace(1) %cursor, i64 %step
  br label %loop

exit:
  ret void
}

declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"histogram"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"__s"}
!5 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"stride"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_buffer_pointer_phi_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    assert_phi_operand_types_match(&module, &asm);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_phi_gep_infers_buffer_param_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %wt, i32 %limit) {
entry:
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %wt, %entry ], [ %next, %loop ]
  %i = phi i32 [ 0, %entry ], [ %inc, %loop ]
  %v = load float, ptr addrspace(1) %cursor, align 4
  %next = getelementptr inbounds float, ptr addrspace(1) %cursor, i64 1
  %inc = add i32 %i, 1
  %done = icmp eq i32 %inc, %limit
  br i1 %done, label %exit, label %loop

exit:
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    assert_eq!(
        ir.ptr_pointees.get(&("k".to_string(), "%wt".to_string())),
        Some(&LlType::Float)
    );
}

#[test]
fn native_scalar_pointer_vector_load_from_phi_root() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @load_vec_from_phi_root(ptr addrspace(1) %p, i1 %advance) {
entry:
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %p, %entry ], [ %next, %body ]
  br i1 %advance, label %body, label %exit

body:
  %next = getelementptr inbounds float, ptr addrspace(1) %cursor, i64 4
  br label %loop

exit:
  %cast = bitcast ptr addrspace(1) %cursor to ptr addrspace(1)
  %v = load <4 x float>, ptr addrspace(1) %cast, align 16
  ret <4 x float> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(
        asm.contains("OpCompositeConstruct") || asm.matches("OpCompositeInsert").count() >= 4,
        "vector not rebuilt from scalar lanes:\n{asm}"
    );
    assert!(
        !asm.contains("reinterpret load bit width mismatch"),
        "{asm}"
    );
}

#[test]
fn native_workgroup_phi_scalar_pointer_vector_load_uses_lane_loads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @load_workgroup_vec_from_phi(ptr addrspace(3) %scratch, i1 %done) {
entry:
  %seed = getelementptr inbounds float, ptr addrspace(3) %scratch, i64 0
  store float 0.000000e+00, ptr addrspace(3) %seed, align 4
  %base = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  br label %loop

loop:
  %cursor = phi ptr addrspace(3) [ %base, %entry ], [ %next, %loop ]
  %v = load <4 x float>, ptr addrspace(3) %cursor, align 16
  %next = getelementptr inbounds <4 x float>, ptr addrspace(3) %cursor, i64 1
  %lane = extractelement <4 x float> %v, i64 0
  br i1 %done, label %exit, label %loop

exit:
  ret float %lane
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.matches("OpLoad").count() >= 4, "{asm}");
    assert!(
        !asm.contains("reinterpret load bit width mismatch"),
        "{asm}"
    );
}

#[test]
fn native_kernel_threadgroup_vector_phi_uses_scalar_backing_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out, i32 %done) {
entry:
  %seed = getelementptr inbounds float, ptr addrspace(3) %scratch, i64 0
  store float 0.000000e+00, ptr addrspace(3) %seed, align 4
  %base = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  br label %loop

loop:
  %cursor = phi ptr addrspace(3) [ %base, %entry ], [ %next, %body ]
  %v = load <4 x float>, ptr addrspace(3) %cursor, align 16
  %lane = extractelement <4 x float> %v, i64 0
  store float %lane, ptr addrspace(1) %out, align 4
  %exit_now = icmp eq i32 %done, 0
  br i1 %exit_now, label %exit, label %body

body:
  %next = getelementptr inbounds <4 x float>, ptr addrspace(3) %cursor, i64 1
  %next_v = load <4 x float>, ptr addrspace(3) %next, align 16
  %next_lane = extractelement <4 x float> %next_v, i64 1
  store float %next_lane, ptr addrspace(1) %out, align 4
  br label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"done"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_vector_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("_ptr_Workgroup_v4float"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_helper_vector_phi_uses_callsite_scalar_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out, i32 %done) {
entry:
  %seed = getelementptr inbounds float, ptr addrspace(3) %scratch, i64 0
  store float 0.000000e+00, ptr addrspace(3) %seed, align 4
  %base = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  tail call void @helper(ptr addrspace(3) %base, ptr addrspace(1) %out, i32 %done)
  ret void
}

define internal void @helper(ptr addrspace(3) %scratch, ptr addrspace(1) %out, i32 %done) {
entry:
  br label %loop

loop:
  %cursor = phi ptr addrspace(3) [ %scratch, %entry ], [ %next, %body ]
  %v = load <4 x float>, ptr addrspace(3) %cursor, align 16
  %lane = extractelement <4 x float> %v, i64 0
  store float %lane, ptr addrspace(1) %out, align 4
  %exit_now = icmp eq i32 %done, 0
  br i1 %exit_now, label %exit, label %body

body:
  %next = getelementptr inbounds <4 x float>, ptr addrspace(3) %cursor, i64 1
  %next_v = load <4 x float>, ptr addrspace(3) %next, align 16
  %next_lane = extractelement <4 x float> %next_v, i64 1
  store float %next_lane, ptr addrspace(1) %out, align 4
  br label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"done"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_helper_vector_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("_ptr_Workgroup_v4float"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_pointer_phi_eq_null_uses_tracked_nullness() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @is_null(i1 %cond, ptr addrspace(1) %p) {
entry:
  br i1 %cond, label %null, label %nonnull

null:
  br label %merge

nonnull:
  %q = getelementptr inbounds i8, ptr addrspace(1) %p, i64 4
  br label %merge

merge:
  %maybe = phi ptr addrspace(1) [ null, %null ], [ %q, %nonnull ]
  %isnull = icmp eq ptr addrspace(1) %maybe, null
  ret i1 %isnull
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn native_guard_branch_with_blank_line_gets_selection_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @guard(float %x) {
entry:
  %c = fcmp oeq float %x, 0.000000e+00
  br i1 %c, label %discard, label %merge

discard:
  tail call void @air.discard_fragment()
  br label %merge

merge:
  ret float %x
}

declare void @air.discard_fragment()
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    assert!(asm.contains("OpBranchConditional"), "{asm}");
}

#[test]
fn native_conditional_branch_ignores_metadata_operands() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @guard(float %x) {
entry:
  %c = fcmp ogt float %x, 0.000000e+00
  br i1 %c, label %exit, label %loop, !llvm.loop !0

loop:
  br label %exit

exit:
  ret float %x
}

!0 = distinct !{!0}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBranchConditional"), "{asm}");
}

#[test]
fn native_multiblock_branch_phi_and_fcmp_lower() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @pick(float %x) {
entry:
  %c = fcmp ogt float %x, 5.000000e-01
  br i1 %c, label %hi, label %lo
hi:
  %h = fadd fast float %x, 1.000000e+00
  br label %merge
lo:
  %l = fadd fast float %x, 2.000000e+00
  br label %merge
merge:
  %res = phi float [ %h, %hi ], [ %l, %lo ]
  ret float %res
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    assert!(asm.contains("OpBranchConditional"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpFOrdGreaterThan"), "{asm}");
}

#[test]
fn native_switch_lowers_to_structured_op_switch() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @pick(i8 %tag, float %x) {
entry:
  switch i8 %tag, label %default [
i8 0, label %merge
i8 1, label %one
  ]

one:
  %onev = fadd fast float %x, 1.000000e+00
  br label %merge

default:
  br label %merge

merge:
  %res = phi float [ %x, %entry ], [ %onev, %one ], [ 0.000000e+00, %default ]
  ret float %res
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    assert!(asm.contains("OpSwitch"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
}

#[test]
fn native_switch_default_unreachable_uses_live_case_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @pick(i32 %tag, half %x) {
entry:
  switch i32 %tag, label %bad [
i32 0, label %a
i32 1, label %a
i32 2, label %b
  ]

a:
  %av = fadd fast half %x, 0xH3C00
  br label %merge

b:
  %bv = fsub fast half %x, 0xH3C00
  br label %merge

bad:
  unreachable

merge:
  %out = phi half [ %av, %a ], [ %bv, %b ]
  ret half %out
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_unreachable_default_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("pick"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpUnreachable"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_switch_bypass_phi_carrier_rewrites_incomings() {
    fn phi_preds(blocks: &[super::super::cfg::BodyBlock], name: &str, phi: &str) -> Vec<String> {
        let block = blocks
            .iter()
            .find(|b| b.name == name)
            .expect("block present");
        let carrier = block.typed.as_ref().expect("carrier populated");
        carrier
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some(phi))
            .and_then(|inst| inst.phi_incoming().as_ref())
            .map(|(_, incoming)| incoming.iter().map(|(_, pred)| pred.clone()).collect())
            .unwrap_or_default()
    }
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @pick(i32 %tag, half %x, i1 %cond) {
entry:
  switch i32 %tag, label %bad [
i32 0, label %a
i32 1, label %b
i32 2, label %c
  ]

a:
  %av = fadd fast half %x, 0xH3C00
  br label %inner

b:
  %bv = fsub fast half %x, 0xH3C00
  br label %inner

c:
  %cv = fmul fast half %x, 0xH4000
  br i1 %cond, label %inner, label %merge

bad:
  unreachable

inner:
  %iv = phi half [ %av, %a ], [ %bv, %b ], [ %cv, %c ]
  br label %merge

merge:
  %out = phi half [ %iv, %inner ], [ 0xH0000, %c ]
  ret half %out
}
"#;
    let blocks = super::super::ir::LlModule::parse(ll)
        .expect("parse")
        .functions
        .into_iter()
        .find(|function| function.name == "pick")
        .expect("pick function")
        .blocks;
    let lowered = lower_unstructured_switches(&blocks);
    assert!(
        lowered
            .iter()
            .any(|block| block.name.contains("switch_bypass")),
        "expected a synthetic switch-bypass block; got {:?}",
        lowered.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
    let iv_preds = phi_preds(&lowered, "%inner", "%iv");
    assert!(
        iv_preds.iter().any(|pred| pred.contains("switch_bypass")),
        "intermediate phi %iv must gain a bypass incoming; preds {iv_preds:?}"
    );
    let out_preds = phi_preds(&lowered, "%merge", "%out");
    assert!(
        !out_preds.iter().any(|pred| pred == "%c"),
        "merge phi %out must drop its direct %c incoming; preds {out_preds:?}"
    );
}

#[test]
fn native_switch_merge_skips_intermediate_phi_when_case_can_bypass_it() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @pick(i32 %tag, half %x, i1 %cond) {
entry:
  switch i32 %tag, label %bad [
i32 0, label %a
i32 1, label %b
i32 2, label %c
  ]

a:
  %av = fadd fast half %x, 0xH3C00
  br label %inner

b:
  %bv = fsub fast half %x, 0xH3C00
  br label %inner

c:
  %cv = fmul fast half %x, 0xH4000
  br i1 %cond, label %inner, label %merge

bad:
  unreachable

inner:
  %iv = phi half [ %av, %a ], [ %bv, %b ], [ %cv, %c ]
  br label %merge

merge:
  %out = phi half [ %iv, %inner ], [ 0xH0000, %c ]
  ret half %out
}
"#;
    let blocks = super::super::ir::LlModule::parse(ll)
        .expect("parse")
        .functions
        .into_iter()
        .find(|function| function.name == "pick")
        .expect("pick function")
        .blocks;
    let switch_merges = infer_switch_merges(&blocks);
    assert_eq!(
        switch_merges.get("%entry"),
        Some(&"%merge".to_string()),
        "{switch_merges:?}"
    );
    let lowered = lower_unstructured_switches(&blocks);
    assert!(
        lowered
            .iter()
            .any(|block| block.typed.as_ref().is_some_and(|t| matches!(
                t.terminator,
                crate::native::tir::TirTerminator::Switch { .. }
            ))),
        "{lowered:#?}"
    );
}

#[test]
fn native_switch_default_branch_to_case_target_is_renested_as_a_valid_switch() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @pick(i32 %tag, i1 %outer, float %x) {
entry:
  br i1 %outer, label %sw, label %outer_merge

sw:
  switch i32 %tag, label %default [
i32 0, label %case0
i32 1, label %case1
i32 2, label %case2
  ]

case1:
  %v1 = fadd fast float %x, 1.000000e+00
  br label %default

case2:
  %v2 = fmul fast float %x, 2.000000e+00
  br label %default

default:
  %dv = phi float [ %x, %sw ], [ %v1, %case1 ], [ %v2, %case2 ]
  %use_case0 = phi i1 [ true, %sw ], [ false, %case1 ], [ false, %case2 ]
  br i1 %use_case0, label %case0, label %merge

case0:
  br label %merge

merge:
  %mv = phi float [ 1.000000e+00, %case0 ], [ %dv, %default ]
  %out = fmul fast float %mv, %x
  br label %outer_merge

outer_merge:
  %res = phi float [ %out, %merge ], [ %x, %entry ]
  ret float %res
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_case_order_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("pick"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSwitch"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_switch_adjacent_case_chain_remains_valid_by_construction() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @pick(i32 %tag) {
entry:
  switch i32 %tag, label %merge [
i32 3, label %a
i32 2, label %b
i32 1, label %c
  ]

a:
  %av = add i32 %tag, 1
  br label %b

b:
  %bin = phi i32 [ 0, %entry ], [ %av, %a ]
  %bv = add i32 %bin, 2
  br label %c

c:
  %cin = phi i32 [ 0, %entry ], [ %bv, %b ]
  %cv = add i32 %cin, 3
  br label %merge

merge:
  %result = phi i32 [ 0, %entry ], [ %cv, %c ]
  ret i32 %result
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_adjacent_switch_cases_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("pick"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpSwitch"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    std::fs::remove_dir_all(&tmp).expect("remove validation scratch");
}

#[test]
fn native_switch_default_nested_switch_to_shared_case_is_renested_as_valid_switches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %outer, i32 %inner) {
entry:
  switch i32 %outer, label %default [
i32 0, label %edge
i32 15, label %edge
  ]

default:
  switch i32 %inner, label %body [
i32 0, label %edge
i32 19, label %edge
  ]

edge:
  br label %merge

body:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_default_nested_case_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpSwitch").count(), 2, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_branch_merge_skips_shared_tail_with_external_case_entry() {
    let blocks = vec![
        carriered_block("entry", &["br i1 %c0, label %a, label %case1"]),
        carriered_block("%case1", &["br i1 %c1, label %a, label %case2"]),
        carriered_block("%case2", &["br i1 %c2, label %b, label %case3"]),
        carriered_block("%case3", &["br i1 %c3, label %c, label %bad"]),
        carriered_block("%a", &["br label %shared"]),
        carriered_block("%b", &["br label %shared"]),
        carriered_block("%c", &["br label %tail"]),
        carriered_block("%bad", &["unreachable"]),
        carriered_block(
            "%shared",
            &["%v = phi half [ %av, %a ], [ %bv, %b ]", "br label %tail"],
        ),
        carriered_block(
            "%tail",
            &[
                "%out = phi half [ %v, %shared ], [ 0xH0000, %c ]",
                "br label %merge",
            ],
        ),
        carriered_block("%merge", &["ret void"]),
    ];
    let merges = infer_branch_merges(&blocks);
    assert_eq!(
        merges.get(&("%b".to_string(), "%case3".to_string())),
        None,
        "{merges:?}"
    );
    assert_eq!(
        merges.get(&("%a".to_string(), "%case1".to_string())),
        Some(&"%tail".to_string()),
        "{merges:?}"
    );
}

#[test]
fn native_nested_switches_do_not_redeclare_shared_merge_block() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  switch i8 1, label %merge [
i8 0, label %inner
  ]

inner:
  switch i8 2, label %merge [
i8 3, label %case
  ]

case:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_switch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpSwitch").count(), 0, "{asm}");
    let merges = asm
        .lines()
        .filter(|line| line.contains("OpSelectionMerge"))
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect::<Vec<_>>();
    assert_eq!(merges.len(), 2, "{asm}");
    assert_ne!(merges[0], merges[1], "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_loop_latch_can_precede_exit_block() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(float %x) {
entry:
  br label %outer

exit:
  %sink = fadd fast float %acc.next, %x
  ret void

outer:
  %i = phi i32 [ 0, %entry ], [ %outer.next, %outer.latch ]
  %scale = fadd fast float %x, 1.000000e+00
  br label %inner

outer.latch:
  %outer.next = add nuw nsw i32 %i, 1
  %outer.done = icmp eq i32 %outer.next, 4
  br i1 %outer.done, label %exit, label %outer

inner:
  %j = phi i32 [ 0, %outer ], [ %inner.next, %inner ]
  %acc = phi float [ 0.000000e+00, %outer ], [ %acc.next, %inner ]
  %acc.next = fadd fast float %acc, %scale
  %inner.next = add nuw nsw i32 %j, 1
  %inner.done = icmp eq i32 %inner.next, 8
  br i1 %inner.done, label %outer.latch, label %inner
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_late_latch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_header_emits_loop_merge_for_latch_backedge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %skip_body = icmp eq i32 %i, 0
  br i1 %skip_body, label %cont, label %body

body:
  br label %cont

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 2
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpLoopMerge").count(), 1, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_loop_latch_to_outer_continue_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %outer

outer:
  %i = phi i32 [ 16, %entry ], [ %next_i, %outer_continue ]
  br label %inner

outer_continue:
  %next_i = add i32 %i, 16
  %outer_done = icmp ugt i32 %next_i, 64
  br i1 %outer_done, label %exit, label %outer, !llvm.loop !0

inner:
  %j = phi i32 [ 0, %outer ], [ %next_j, %inner_continue ]
  br label %self_once

inner_continue:
  %next_j = add i32 %j, 1
  %inner_done = icmp eq i32 %next_j, 4
  br i1 %inner_done, label %outer_continue, label %inner, !llvm.loop !1

self_once:
  %first = phi i1 [ true, %inner ], [ false, %self_once ]
  br i1 %first, label %self_once, label %inner_continue, !llvm.loop !2

exit:
  ret void
}

!0 = distinct !{!0}
!1 = distinct !{!1}
!2 = distinct !{!2}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_latch_outer_continue_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_early_loop_continue_moves_after_late_phi_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %choose = icmp eq i32 %i, 0
  br i1 %choose, label %then, label %else

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 2
  br i1 %done, label %exit, label %loop

then:
  br label %join

else:
  br label %join

join:
  %selected = phi i32 [ 1, %then ], [ 2, %else ]
  %keep_going = icmp ne i32 %selected, 0
  br i1 %keep_going, label %cont, label %exit

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_late_continue_pred_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_early_single_predecessor_target_moves_after_late_phi_join() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %condition = icmp eq i32 0, 0
  br i1 %condition, label %then, label %else

target:
  ret void

then:
  br label %join

else:
  br label %join

join:
  %selected = phi i32 [ 1, %then ], [ 2, %else ]
  %keep = icmp ne i32 %selected, 0
  br i1 %keep, label %target, label %target
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_late_single_pred_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("create validation scratch");
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpPhi"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    std::fs::remove_dir_all(&tmp).expect("remove validation scratch");
}

#[test]
fn native_loop_header_selection_is_split_to_body_block() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %choose = icmp eq i32 %i, 0
  br i1 %choose, label %a, label %b

a:
  br label %cont

b:
  br label %cont

cont:
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 2
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_header_selection_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_header_else_if_selection_uses_shared_join() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %first = icmp eq i32 %i, 0
  br i1 %first, label %if1, label %elseif

if1:
  br label %join

elseif:
  %second = icmp eq i32 %i, 1
  br i1 %second, label %if2, label %join

if2:
  br label %join

join:
  %chosen = phi i32 [ 1, %if1 ], [ 2, %if2 ], [ 0, %elseif ]
  %third = icmp eq i32 %chosen, 1
  br i1 %third, label %if3, label %elseif2

if3:
  br label %join2

elseif2:
  %fourth = icmp eq i32 %chosen, 2
  br i1 %fourth, label %if4, label %join2

if4:
  br label %join2

join2:
  %chosen2 = phi i32 [ 3, %if3 ], [ 4, %if4 ], [ 0, %elseif2 ]
  %next = add i32 %i, %chosen2
  br label %cont

cont:
  %done = icmp eq i32 %next, 3
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_loop_header_else_if_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_latch_selection_does_not_create_spurious_loop_header() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  br label %outer

outer:
  %i = phi i32 [ 0, %entry ], [ %i.next, %outer.cont ]
  %enter = icmp eq i32 %i, 0
  br i1 %enter, label %inner, label %outer.cont

outer.cont:
  %i.next = add i32 %i, 1
  %outer.done = icmp eq i32 %i.next, 2
  br i1 %outer.done, label %exit, label %outer

inner:
  %j = phi i32 [ 0, %outer ], [ %j.next, %inner.latch ]
  %x = add i32 %j, %i
  %c0 = icmp eq i32 %x, 0
  br i1 %c0, label %a, label %b

a:
  br label %join1

b:
  %c1 = icmp eq i32 %x, 1
  br i1 %c1, label %c, label %join1

c:
  br label %join1

join1:
  %p = phi i32 [ 1, %a ], [ 2, %c ], [ 0, %b ]
  %c2 = icmp eq i32 %p, 1
  br i1 %c2, label %d, label %e

d:
  br label %inner.latch

e:
  %c3 = icmp eq i32 %p, 2
  br i1 %c3, label %f, label %inner.latch

f:
  br label %inner.latch

inner.latch:
  %q = phi i32 [ 3, %d ], [ 4, %f ], [ 0, %e ]
  %j.next = add i32 %j, %q
  %inner.done = icmp eq i32 %j.next, 3
  br i1 %inner.done, label %outer.cont, label %inner

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_latch_selection_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_branch_merge_can_start_following_else_if_ladder() {
    let body = r#"
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %first = icmp eq i32 %i, 0
  br i1 %first, label %if1, label %elseif

if1:
  br label %join

elseif:
  %second = icmp eq i32 %i, 1
  br i1 %second, label %if2, label %join

if2:
  br label %join

join:
  %third = icmp eq i32 1, 1
  br i1 %third, label %if3, label %elseif2

if3:
  br label %join2

elseif2:
  %fourth = icmp eq i32 1, 2
  br i1 %fourth, label %if4, label %join2

if4:
  br label %join2

join2:
  %next = add i32 %i, 1
  br label %cont

cont:
  %done = icmp eq i32 %next, 3
  br i1 %done, label %exit, label %loop

exit:
  ret void
"#;
    let lines = body.lines().map(ToString::to_string).collect::<Vec<_>>();
    let blocks = split_body_blocks(
        &lines,
        "entry".to_string(),
        &crate::fxhash::HashMap::default(),
    );
    let merges = infer_branch_merges(&blocks);
    assert_eq!(
        merges.get(&("%if1".to_string(), "%elseif".to_string())),
        Some(&"%join".to_string()),
        "{merges:?}"
    );
    assert_eq!(
        merges.get(&("%if3".to_string(), "%elseif2".to_string())),
        Some(&"%join2".to_string()),
        "{merges:?}"
    );
}

#[test]
fn native_loop_inference_rejects_exit_blocks_with_later_predecessors() {
    let blocks = vec![
        carriered_block("%entry", &["br label %loop"]),
        carriered_block("%exit", &["br label %merge"]),
        carriered_block(
            "%loop",
            &[
                "%done = icmp eq i32 0, 1",
                "br i1 %done, label %exit, label %cont",
            ],
        ),
        carriered_block("%cont", &["br label %loop"]),
        carriered_block("%merge", &["ret void"]),
    ];
    let merges = infer_loop_merges(&blocks);
    assert!(!merges.contains_key("%exit"), "{merges:?}");
    assert!(merges.contains_key("%loop"), "{merges:?}");
}

#[test]
fn native_branch_inference_skips_bypassable_early_exit() {
    let blocks = vec![
        carriered_block(
            "%entry",
            &[
                "%outer = icmp eq i32 0, 0",
                "br i1 %outer, label %early, label %body",
            ],
        ),
        carriered_block(
            "%body",
            &[
                "%inner = icmp eq i32 1, 1",
                "br i1 %inner, label %nested, label %early",
            ],
        ),
        carriered_block(
            "%nested",
            &[
                "%late = icmp eq i32 2, 2",
                "br i1 %late, label %early, label %late_body",
            ],
        ),
        carriered_block("%late_body", &["br label %merge"]),
        carriered_block("%early", &["br label %merge"]),
        carriered_block("%merge", &["ret void"]),
    ];
    let merges = infer_branch_merges(&blocks);
    assert_eq!(
        merges.get(&("%early".to_string(), "%body".to_string())),
        Some(&"%merge".to_string()),
        "{merges:?}"
    );
}

#[test]
fn native_switch_merge_with_bypass_predecessor_is_split() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %skip = icmp eq i32 0, 0
  br i1 %skip, label %merge, label %sw

sw:
  switch i32 1, label %merge [
i32 0, label %case
  ]

case:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_external_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_split_switch_merge_rewrites_nested_branch_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %skip = icmp eq i32 0, 0
  br i1 %skip, label %external, label %sw

external:
  br label %merge

sw:
  switch i32 1, label %merge [
i32 0, label %case0
i32 1, label %case1
  ]

case0:
  br label %merge

case1:
  %inner = icmp eq i32 1, 1
  br i1 %inner, label %case1_true, label %case1_false

case1_true:
  br label %merge

case1_false:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_nested_branch_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selection_with_shared_branch_target_clones_target() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i1 %outer, i1 %inner, float %x) {
entry:
  br i1 %outer, label %shared, label %inner_header

inner_header:
  br i1 %inner, label %case, label %shared

case:
  %a = fadd float %x, 1.000000e+00
  br label %merge

shared:
  %b = fadd float %x, 2.000000e+00
  br label %merge

merge:
  %p = phi float [ %a, %case ], [ %b, %shared ]
  %sink = fadd float %p, 3.000000e+00
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shared_branch_target_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selection_or_chain_shared_target_is_cloned() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i1 %first, i1 %second, float %x) {
entry:
  br i1 %first, label %shared, label %check_second

check_second:
  br i1 %second, label %shared, label %merge

shared:
  %flag = fadd float %x, 4.000000e+00
  br label %merge

merge:
  %p = phi float [ %flag, %shared ], [ 0.000000e+00, %check_second ]
  %sink = fadd float %p, 1.000000e+00
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_or_chain_shared_target_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpFAdd").count(), 3, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selection_with_shared_branch_target_clones_reachable_region() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i1 %outer, i1 %inner, i1 %deep, float %x) {
entry:
  br i1 %outer, label %shared, label %inner_header

inner_header:
  br i1 %inner, label %shared, label %other

shared:
  %a = fadd float %x, 1.000000e+00
  br i1 %deep, label %shared_then, label %shared_else

shared_then:
  %b = fadd float %a, 2.000000e+00
  br label %shared_join

shared_else:
  %c = fadd float %a, 3.000000e+00
  br label %shared_join

shared_join:
  %d = phi float [ %b, %shared_then ], [ %c, %shared_else ]
  br label %merge

other:
  %e = fadd float %x, 4.000000e+00
  br label %merge

merge:
  %p = phi float [ %d, %shared_join ], [ %e, %other ]
  %sink = fadd float %p, 5.000000e+00
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shared_region_clone_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_phi_edge_materializations_attach_before_predecessor_merge() {
    let mut blocks = vec![
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(1), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(4),
                        Operand::SelectionControl(SelectionControl::NONE),
                    ],
                ),
                Instruction::new(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(90), Operand::IdRef(4), Operand::IdRef(2)],
                ),
            ],
        },
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(2), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::LoopMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(3),
                        Operand::IdRef(2),
                        Operand::LoopControl(spirv::LoopControl::NONE),
                    ],
                ),
                Instruction::new(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(91), Operand::IdRef(3), Operand::IdRef(2)],
                ),
            ],
        },
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(3), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::Phi,
                    Some(10),
                    Some(50),
                    vec![Operand::IdRef(51), Operand::IdRef(2)],
                ),
                Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(4)]),
            ],
        },
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(4), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::Phi,
                    Some(10),
                    Some(60),
                    vec![
                        Operand::IdRef(61),
                        Operand::IdRef(1),
                        Operand::IdRef(50),
                        Operand::IdRef(3),
                    ],
                ),
                Instruction::new(
                    Op::IAdd,
                    Some(10),
                    Some(75),
                    vec![Operand::IdRef(61), Operand::IdRef(61)],
                ),
                Instruction::new(
                    Op::InBoundsAccessChain,
                    Some(20),
                    Some(70),
                    vec![Operand::IdRef(100), Operand::IdRef(101)],
                ),
                Instruction::new(
                    Op::Phi,
                    Some(20),
                    Some(71),
                    vec![
                        Operand::IdRef(70),
                        Operand::IdRef(1),
                        Operand::IdRef(72),
                        Operand::IdRef(2),
                    ],
                ),
                Instruction::new(
                    Op::AccessChain,
                    Some(20),
                    Some(80),
                    vec![Operand::IdRef(100), Operand::IdRef(102)],
                ),
                Instruction::new(
                    Op::Phi,
                    Some(20),
                    Some(81),
                    vec![
                        Operand::IdRef(80),
                        Operand::IdRef(1),
                        Operand::IdRef(82),
                        Operand::IdRef(3),
                    ],
                ),
                Instruction::new(Op::Return, None, None, vec![]),
            ],
        },
    ];
    let ir = super::super::ir::LlModule::parse(
        r#"target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  ret void
}
"#,
    )
    .expect("parse empty module");
    let mut emitter = Emitter::new(ir);

    let target = blocks
        .iter_mut()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(4))
        .expect("target block");
    let mut edge_instructions = Vec::new();
    target.instructions.retain(|inst| {
        if matches!(inst.result_id, Some(70 | 80)) {
            edge_instructions.push(inst.clone());
            false
        } else {
            inst.result_id != Some(75)
        }
    });
    emitter.record_phi_edge_instructions(1, edge_instructions);
    emitter
        .attach_phi_edge_instructions(&mut blocks)
        .expect("attach phi edges");

    let pred = blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(1))
        .expect("incoming predecessor");
    let access_idx = pred
        .instructions
        .iter()
        .position(|inst| inst.result_id == Some(70))
        .expect("moved access chain");
    let plain_access_idx = pred
        .instructions
        .iter()
        .position(|inst| inst.result_id == Some(80))
        .expect("moved plain access chain");
    let merge_idx = pred
        .instructions
        .iter()
        .position(|inst| inst.class.opcode == Op::SelectionMerge)
        .expect("selection merge");
    assert!(access_idx < merge_idx, "{pred:?}");
    assert!(plain_access_idx < merge_idx, "{pred:?}");

    let target = blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(4))
        .expect("target block");
    let first_non_phi = target
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Phi)
        .expect("non-phi terminator");
    assert!(
        target.instructions[..first_non_phi]
            .iter()
            .all(|inst| inst.class.opcode == Op::Phi),
        "{target:?}"
    );
}

#[test]
fn native_conditional_continue_backedge_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i1 %skip, i1 %done, i1 %exit, float %x) {
entry:
  br i1 %skip, label %merge, label %loop

loop:
  %v = phi float [ %x, %entry ], [ %next, %cont ]
  %i = phi i32 [ 0, %entry ], [ %inc, %cont ]
  %next = fadd float %v, 1.000000e+00
  br i1 %done, label %merge, label %cont

cont:
  %inc = add i32 %i, 1
  %stop = icmp eq i32 %inc, 4
  %leave = select i1 %exit, i1 true, i1 %stop
  br i1 %leave, label %after, label %loop

after:
  %out = fadd float %next, 2.000000e+00
  br label %merge

merge:
  %p = phi float [ 0.000000e+00, %entry ], [ %next, %loop ], [ %out, %after ]
  %sink = fadd float %p, 3.000000e+00
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_conditional_continue_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_inner_branch_to_switch_merge_gets_own_synthetic_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  switch i32 0, label %merge [
i32 0, label %case
  ]

case:
  %cond = icmp eq i32 1, 1
  br i1 %cond, label %a, label %b

a:
  br label %merge

b:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_switch_inner_branch_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpSelectionMerge").count(), 2, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_branch_reuses_inner_conditional_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %outer = icmp eq i32 0, 0
  br i1 %outer, label %body, label %inner

inner:
  %inner_cond = icmp eq i32 1, 1
  br i1 %inner_cond, label %body, label %inner_merge

body:
  br label %inner_merge

inner_merge:
  br label %exit

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_conditional_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    let selection_merges = asm.matches("OpSelectionMerge").count();
    assert!(selection_merges >= 1, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selection_with_nested_shared_branch_target_clones_target() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i1 %outer, i1 %skip, i1 %inner, float %x) {
entry:
  br i1 %outer, label %shared, label %top

top:
  br i1 %skip, label %done, label %nested

done:
  %d = fadd float %x, 3.000000e+00
  br label %merge

nested:
  br i1 %inner, label %case, label %shared

case:
  %c = fadd float %x, 1.000000e+00
  br label %merge

shared:
  %shared_cond = icmp eq i32 2, 2
  br i1 %shared_cond, label %shared_then, label %shared_else

shared_then:
  %s1 = fadd float %x, 2.000000e+00
  br label %merge

shared_else:
  %s2 = fadd float %x, 5.000000e+00
  br label %merge

merge:
  %p = phi float [ %s1, %shared_then ], [ %s2, %shared_else ], [ %d, %done ], [ %c, %case ]
  %sink = fadd float %p, 4.000000e+00
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_shared_branch_target_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selection_phi_passthrough_target_with_external_pred_is_cloned() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %outer = icmp eq i32 0, 0
  br i1 %outer, label %header, label %external

external:
  br label %body

header:
  %inner = icmp eq i32 1, 1
  br i1 %inner, label %body, label %merge

body:
  %p = phi i32 [ 1, %header ], [ 2, %external ]
  br label %merge

merge:
  %q = phi i32 [ 0, %header ], [ %p, %body ]
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_phi_passthrough_clone_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_branch_merge_can_be_later_reachable_target() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %skip = icmp eq i32 0, 0
  br i1 %skip, label %exit, label %body

body:
  br label %mid

mid:
  br label %exit

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_reachable_branch_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_branch_merge_skips_bypassable_early_exit() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %outer = icmp eq i32 0, 0
  br i1 %outer, label %early, label %body

body:
  %inner = icmp eq i32 1, 1
  br i1 %inner, label %nested, label %early

nested:
  %late = icmp eq i32 2, 2
  br i1 %late, label %early, label %late_body

late_body:
  br label %merge

early:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_bypassable_early_exit_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_branch_merge_can_be_shared_later_target() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %outer = icmp eq i32 0, 0
  br i1 %outer, label %nested, label %straight

straight:
  br label %merge

nested:
  %inner = icmp eq i32 1, 1
  br i1 %inner, label %left, label %right

left:
  %left_cond = icmp eq i32 2, 2
  br i1 %left_cond, label %left_body, label %left_merge

left_body:
  br label %left_merge

left_merge:
  br label %merge

right:
  %right_cond = icmp eq i32 3, 3
  br i1 %right_cond, label %right_body, label %right_merge

right_body:
  br label %right_merge

right_merge:
  br label %merge

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_shared_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpBranchConditional").count(), 4, "{asm}");
    assert_eq!(asm.matches("OpSelectionMerge").count(), 4, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_loop_merge_with_bypass_predecessor_is_split() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %skip = icmp eq i32 0, 0
  br i1 %skip, label %merge, label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %done = icmp eq i32 %i, 2
  br i1 %done, label %merge, label %body

body:
  br label %cont

cont:
  %next = add i32 %i, 1
  br label %loop

merge:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_external_loop_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_backward_layout_loop_merge_with_bypass_is_split() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %skip = icmp eq i32 0, 0
  br i1 %skip, label %merge, label %loop

merge:
  ret void

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %cont ]
  %done = icmp eq i32 %i, 2
  br i1 %done, label %merge, label %body

body:
  br label %cont

cont:
  %next = add i32 %i, 1
  br label %loop
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_backward_external_loop_merge_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_switch_accepts_signed_case_literals() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @signed_pick(i8 %tag) {
entry:
  switch i8 %tag, label %zero [
i8 -1, label %neg
  ]

neg:
  br label %merge

zero:
  br label %merge

merge:
  %res = phi i32 [ 1, %neg ], [ 0, %zero ]
  ret i32 %res
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpSwitch"), "{asm}");
    assert!(asm.contains("255"), "{asm}");
}

#[test]
fn native_switch_rejects_out_of_range_negative_case_literals() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @signed_pick(i8 %tag) {
entry:
  switch i8 %tag, label %zero [
i8 -129, label %neg
  ]

neg:
  br label %merge

zero:
  br label %merge

merge:
  %res = phi i32 [ 1, %neg ], [ 0, %zero ]
  ret i32 %res
}
"#;
    let err = emit_vulkan_spirv(ll).expect_err("expected range error");
    assert!(err.contains("overflows i8"), "{err}");
}

#[test]
fn native_phi_splat_constant_uses_constant_composite() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @phi_splat(float %s, <4 x float> %x) {
entry:
  %c = fcmp ogt float %s, 0.000000e+00
  br i1 %c, label %keep, label %nan

keep:
  br label %merge

nan:
  br label %merge

merge:
  %res = phi <4 x float> [ %x, %keep ], [ splat (float 0x7FF8000000000000), %nan ]
  ret <4 x float> %res
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
}

#[test]
fn native_phi_vector_literal_with_undef_lane_uses_constant_composite() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @phi_vector_undef_lane(i1 %c) {
entry:
  br i1 %c, label %a, label %b

a:
  br label %merge

b:
  br label %merge

merge:
  %res = phi <4 x float> [ <float 0.000000e+00, float 1.000000e+00, float 2.000000e+00, float undef>, %a ], [ zeroinitializer, %b ]
  ret <4 x float> %res
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(!asm.contains("OpCompositeConstruct"), "{asm}");
}

#[test]
fn native_phi_accepts_forward_backedge_values() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @loop_phi(<4 x float> %x) {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %body ]
  %acc = phi <4 x float> [ %x, %entry ], [ %out, %body ]
  %done = icmp eq i32 %i, 1
  br i1 %done, label %exit, label %body

body:
  %out = insertelement <4 x float> %acc, float 1.000000e+00, i64 0
  %next = add i32 %i, 1
  br label %loop

exit:
  ret <4 x float> %acc
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpCompositeInsert"), "{asm}");
}

#[test]
fn native_branch_condition_accepts_forward_predecessor_value() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @forward_branch_condition(i1 %c) {
entry:
  br label %producer

consumer:
  br i1 %flag, label %yes, label %no

producer:
  %flag = and i1 %c, true
  br label %consumer

yes:
  ret void

no:
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBranchConditional"), "{asm}");
    assert!(asm.contains("OpLogicalAnd"), "{asm}");
}

#[test]
fn native_internal_call_skips_extra_air_abi_operands_by_formal_type() {
    let ll = r#"
define <4 x half> @frag(<4 x float> %pos, <3 x float> %a, <3 x float> %b) {
entry:
  %tmp = alloca <4 x float>, align 16
  %x = extractelement <4 x float> %pos, i64 0
  %enabled = fcmp fast olt float %x, 0.000000e+00
  call void @helper(ptr %tmp, <4 x float> %pos, <3 x float> %a, <3 x float> %b, <3 x float> %a, <2 x float> zeroinitializer, <3 x float> %b, i32 7, [12 x i8] undef, <3 x float> %a, <3 x half> zeroinitializer, i1 %enabled)
  ret <4 x half> zeroinitializer
}

define internal void @helper(ptr %out, <4 x float> %pos, <3 x float> %a, <3 x float> %b, <3 x float> %c, <2 x float> %uv, <3 x float> %d, <3 x float> %e, <3 x half> %normal, i1 %enabled) {
entry:
  br i1 %enabled, label %yes, label %no

yes:
  ret void

no:
  ret void
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{!4, !5, !6}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!4 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos"}
!5 = !{i32 1, !"air.fragment_input", !"air.arg_type_name", !"float3", !"air.arg_name", !"a"}
!6 = !{i32 2, !"air.fragment_input", !"air.arg_type_name", !"float3", !"air.arg_name", !"b"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_extra_air_abi_call_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpFOrdLessThan"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_phi_canonicalizes_identical_incoming_from_same_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @same_pred_phi(i8 %tag) {
entry:
  switch i8 %tag, label %merge [
i8 1, label %merge
  ]

merge:
  %out = phi i32 [ 0, %entry ], [ 0, %entry ]
  ret i32 %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(!asm.contains("OpPhi"), "{asm}");
    assert!(asm.contains("OpReturnValue"), "{asm}");
}

#[test]
fn native_bool_phi_dedupes_identical_incoming_from_same_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @same_pred_bool_phi(i8 %tag) {
entry:
  switch i8 %tag, label %default [
i8 0, label %merge
i8 1, label %merge
  ]

default:
  br label %merge

merge:
  %out = phi i1 [ true, %entry ], [ true, %entry ], [ false, %default ]
  ret i1 %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let phi = asm
        .lines()
        .find(|line| line.contains("OpPhi"))
        .unwrap_or_else(|| panic!("missing OpPhi in {asm}"));
    assert_eq!(phi.split_whitespace().count(), 8, "{asm}");
}

#[test]
fn native_small_int_phi_dedupes_identical_incoming_from_same_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i8 @same_pred_i8_phi(i8 %tag) {
entry:
  switch i8 %tag, label %default [
i8 0, label %merge
i8 1, label %merge
  ]

default:
  br label %merge

merge:
  %out = phi i8 [ 1, %entry ], [ 1, %entry ], [ 0, %default ]
  ret i8 %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let phi = asm
        .lines()
        .find(|line| line.contains("OpPhi"))
        .unwrap_or_else(|| panic!("missing OpPhi in {asm}"));
    assert_eq!(phi.split_whitespace().count(), 8, "{asm}");
}

#[test]
fn native_vector_zero_phi_dedupes_identical_incoming_from_same_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x i32> @same_pred_vec_phi(i8 %tag) {
entry:
  switch i8 %tag, label %default [
i8 0, label %merge
i8 1, label %merge
  ]

default:
  br label %merge

merge:
  %out = phi <4 x i32> [ zeroinitializer, %entry ], [ zeroinitializer, %entry ], [ <i32 1, i32 1, i32 1, i32 1>, %default ]
  ret <4 x i32> %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let phi = asm
        .lines()
        .find(|line| line.contains("OpPhi"))
        .unwrap_or_else(|| panic!("missing OpPhi in {asm}"));
    assert_eq!(phi.split_whitespace().count(), 8, "{asm}");
}

#[test]
fn native_nested_quoted_named_types_resolve_for_gep() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.RB::Shader::Globals.23" = type { %"struct.RB::Shader::PrimitiveGlobals" }
%"struct.RB::Shader::PrimitiveGlobals" = type { [2 x float], half }
define half @primitive_field(ptr addrspace(2) %g) {
entry:
  %p = getelementptr inbounds %"struct.RB::Shader::Globals.23", ptr addrspace(2) %g, i64 0, i32 0, i32 1
  %v = load half, ptr addrspace(2) %p
  ret half %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_parser_accepts_nested_byte_string_arrays() {
    let value = parse_typed_value(r#"[2 x [2 x i8]] [[2 x i8] c"\02\08", [2 x i8] c"!\B7"]"#)
        .expect("parse nested byte string array");
    assert_eq!(
        value.ty,
        LlType::Array(Box::new(LlType::Array(Box::new(LlType::Int(8)), 2)), 2)
    );
    let LlValue::Array(rows) = value.value else {
        panic!("expected outer array");
    };
    let bytes = rows
        .into_iter()
        .flat_map(|row| match row.value {
            LlValue::Array(lanes) => lanes
                .into_iter()
                .map(|lane| match lane.value {
                    LlValue::Int(byte) => byte as u8,
                    other => panic!("expected byte lane, got {other:?}"),
                })
                .collect::<Vec<_>>(),
            other => panic!("expected inner byte array, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(bytes, [2, 8, 33, 183]);
}

#[test]
fn native_parser_accepts_nested_packed_struct_constants() {
    let value = parse_typed_value(
        "[2 x <{ i8, i8, [2 x i8] }>] [<{ i8, i8, [2 x i8] }> <{ i8 0, i8 -1, [2 x i8] zeroinitializer }>, <{ i8, i8, [2 x i8] }> <{ i8 2, i8 3, [2 x i8] zeroinitializer } >]",
    )
    .expect("parse nested packed struct constants");
    assert_eq!(
        value.ty,
        LlType::Array(
            Box::new(LlType::Struct(vec![
                LlType::Int(8),
                LlType::Int(8),
                LlType::Array(Box::new(LlType::Int(8)), 2)
            ])),
            2
        )
    );
    let LlValue::Array(rows) = value.value else {
        panic!("expected outer array");
    };
    assert_eq!(rows.len(), 2);
    for row in rows {
        let LlValue::Struct(fields) = row.value else {
            panic!("expected packed struct row");
        };
        assert_eq!(fields.len(), 3);
        assert!(matches!(fields[2].value, LlValue::Zero));
    }
}

#[test]
fn native_helper_keeps_nested_array_pointee_over_flat_metadata_alias() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%StaticArray2D = type { [3 x [7 x float]] }

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %weights, i32 %row) {
entry:
  call fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(2) %weights, i32 %row)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(2) %weights, i32 %row) {
entry:
  %wide = zext i32 %row to i64
  %slot = getelementptr inbounds %StaticArray2D, ptr addrspace(2) %weights, i64 0, i32 0, i64 %wide, i64 1
  %v = load float, ptr addrspace(2) %slot, align 4
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 84, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 84, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"StaticArray2D", !"air.arg_name", !"weights"}
!5 = !{i32 0, i32 4, i32 21, !"float", !"data"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"row"}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    assert_eq!(
        ir.ptr_pointees
            .get(&("helper".to_string(), "%weights".to_string())),
        Some(&parse_type("%StaticArray2D").expect("parse type"))
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_array_call_metadata_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("ArrayStride 28"), "{asm}");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_i2_trunc_switch_legalizes_and_validates() {
    let ll = r#"
target triple = "air64_v28-apple-macosx12.0.0"

define void @k(ptr addrspace(1) %out, i16 %gid) {
entry:
  %t = trunc i16 %gid to i2
  switch i2 %t, label %def [
    i2 -1, label %c3
    i2 -2, label %c2
    i2 1, label %c1
  ]

c1:
  store i32 1, ptr addrspace(1) %out, align 4
  ret void
c2:
  store i32 2, ptr addrspace(1) %out, align 4
  ret void
c3:
  store i32 3, ptr addrspace(1) %out, align 4
  ret void
def:
  store i32 0, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"device int*", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_i2_switch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let raw = emit_vulkan_spirv(ll).expect("native emit i2 switch");
    load_bytes(&raw).expect("SPIR-V load of primary emit");
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    for bad in [2u32, 3, 4, 5, 6, 7, 9, 12, 24] {
        assert!(
            !asm.contains(&format!("OpTypeInt {bad} ")),
            "legalized module must not declare OpTypeInt {bad}:\n{asm}"
        );
    }
    assert!(
        asm.contains("OpTypeInt 8 0") || asm.contains("OpTypeInt 8 1"),
        "i2 should legalize into an i8 container:\n{asm}"
    );
    assert!(
        asm.lines().any(|l| l.contains("OpSwitch")
            && l.contains(" 3 ")
            && l.contains(" 2 ")
            && l.contains(" 1 ")),
        "switch cases must be low-bit encodings 3/2/1, not sign-extended -1/-2:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_i24_constant_legalizes_and_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %x) {
entry:
  %n = trunc i32 %x to i24
  %a = add i24 %n, 1
  %z = zext i24 %a to i32
  ret void
}

"#;
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_native_i24_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let raw = emit_vulkan_spirv(ll).expect("native emit i24");
    load_bytes(&raw).expect("SPIR-V load of primary emit");
    let module = load_bytes(&raw).expect("load");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble");
    assert!(
        !asm.contains("OpTypeInt 24 "),
        "i24 must legalize away:\n{asm}"
    );
    assert!(
        !asm.contains("OpUConvert"),
        "i24 and i32 share one emitted storage type, so conversion must be valid at construction:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_i24_signed_extension_restores_logical_sign_bit() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i32 %x) {
entry:
  %n = trunc i32 %x to i24
  %s = sext i24 %n to i32
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_i24_sext_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let raw = emit_vulkan_spirv(ll).expect("native emit i24 sext");
    let module = load_bytes(&raw).expect("load");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpShiftRightArithmetic"), "{asm}");
    assert!(
        !asm.contains("OpSConvert"),
        "i24 and i32 share one emitted storage type:\n{asm}"
    );
    tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
}

#[test]
fn dense_small_cfg_keeps_complete_switch_merge_inference() {
    let mut ll = String::from(
        "define void @dense(i1 %c, i32 %sel) {\n\
         entry:\n\
         \x20 switch i32 %sel, label %d [\n\
         \x20   i32 0, label %a\n\
         \x20   i32 1, label %b\n\
         \x20 ]\n\
         a:\n\
         \x20 br label %a2\n\
         a2:\n\
         \x20 br label %merge\n\
         b:\n\
         \x20 br label %b2\n\
         b2:\n\
         \x20 br label %merge\n\
         d:\n\
         \x20 br label %merge\n\
         merge:\n\
         \x20 br label %n0\n",
    );
    const DIAMONDS: usize = 45;
    for i in 0..DIAMONDS {
        ll.push_str(&format!(
            "n{i}:\n  br i1 %c, label %t{i}, label %n{next}\nt{i}:\n  br label %n{next}\n",
            next = i + 1
        ));
    }
    ll.push_str(&format!("n{DIAMONDS}:\n  ret void\n}}\n"));

    let blocks = super::super::ir::LlModule::parse(&ll)
        .expect("parse")
        .functions
        .into_iter()
        .find(|function| function.name == "dense")
        .expect("dense function")
        .blocks;

    assert!(
        exceeds_local_structured_plan_budget(&blocks),
        "the density term must be what declines this graph: {} blocks",
        blocks.len()
    );
    assert!(blocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS);
    assert_eq!(
        infer_direct_switch_merges(&blocks).get("%entry"),
        None,
        "arms reconverge through an intermediate block, so the direct subset proves nothing",
    );
    assert_eq!(
        infer_switch_merges_bounded(&blocks).get("%entry"),
        Some(&"%merge".to_string()),
    );
}

#[test]
fn oversized_cfg_keeps_the_direct_switch_merge_subset() {
    let mut ll = String::from(
        "define void @wide(i32 %sel) {\n\
         entry:\n\
         \x20 switch i32 %sel, label %d [\n\
         \x20   i32 0, label %a\n\
         \x20   i32 1, label %b\n\
         \x20 ]\n\
         a:\n\
         \x20 br label %a2\n\
         a2:\n\
         \x20 br label %merge\n\
         b:\n\
         \x20 br label %b2\n\
         b2:\n\
         \x20 br label %merge\n\
         d:\n\
         \x20 br label %merge\n\
         merge:\n\
         \x20 br label %p0\n",
    );
    let pad = CROSS_ARM_EDGE_MAX_BLOCKS + 1;
    for i in 0..pad {
        ll.push_str(&format!("p{i}:\n  br label %p{next}\n", next = i + 1));
    }
    ll.push_str(&format!("p{pad}:\n  ret void\n}}\n"));

    let blocks = super::super::ir::LlModule::parse(&ll)
        .expect("parse")
        .functions
        .into_iter()
        .find(|function| function.name == "wide")
        .expect("wide function")
        .blocks;

    assert!(blocks.len() > CROSS_ARM_EDGE_MAX_BLOCKS);
    assert_eq!(
        infer_switch_merges_bounded(&blocks),
        infer_direct_switch_merges(&blocks),
    );
    assert_eq!(infer_switch_merges_bounded(&blocks).get("%entry"), None);
}

#[cfg(test)]
fn dense_diamond_chain(back_edge: bool) -> Vec<BodyBlock> {
    const DIAMONDS: usize = 45;
    let mut ll = String::from(
        "define void @dense(i1 %c) {\n\
         entry:\n\
         \x20 br i1 %c, label %a, label %b\n\
         a:\n\
         \x20 br label %a2\n\
         a2:\n\
         \x20 br label %join\n\
         b:\n\
         \x20 br label %b2\n\
         b2:\n\
         \x20 br label %join\n\
         join:\n\
         \x20 br label %n0\n",
    );
    for i in 0..DIAMONDS {
        ll.push_str(&format!(
            "n{i}:\n  br i1 %c, label %t{i}, label %n{next}\nt{i}:\n  br label %n{next}\n",
            next = i + 1
        ));
    }
    ll.push_str(&if back_edge {
        format!("n{DIAMONDS}:\n  br i1 %c, label %n0, label %exit\nexit:\n  ret void\n}}\n")
    } else {
        format!("n{DIAMONDS}:\n  ret void\n}}\n")
    });
    super::super::ir::LlModule::parse(&ll)
        .expect("parse")
        .functions
        .into_iter()
        .find(|function| function.name == "dense")
        .expect("dense function")
        .blocks
}

#[test]
fn loop_free_dense_cfg_keeps_complete_branch_merge_inference() {
    let blocks = dense_diamond_chain(false);
    assert!(exceeds_local_structured_plan_budget(&blocks));
    assert!(blocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS);
    assert!(loop_forest_is_empty(&blocks));
    assert_eq!(
        infer_branch_merges(&blocks).get(&("%a".to_string(), "%b".to_string())),
        Some(&"%join".to_string()),
        "arms reconverge two hops apart, so only the complete inference proves the merge",
    );
}

#[test]
fn a_loop_in_a_dense_cfg_declines_the_complete_branch_merge_inference() {
    let blocks = dense_diamond_chain(true);
    assert!(exceeds_local_structured_plan_budget(&blocks));
    assert!(blocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS);
    assert!(!loop_forest_is_empty(&blocks));
}
