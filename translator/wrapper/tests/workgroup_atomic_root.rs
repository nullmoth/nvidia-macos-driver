//! Own minimal AIR regressions; no cached application shader or GPU execution.
use metal2vulkan::passes::{Stage, TransformOptions};
use rspirv::dr::{load_bytes, Operand};
use rspirv::spirv::{Op, Scope, StorageClass};
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

fn kernel(body: &str, declarations: &str, local_type: &str) -> String {
    format!(r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out) {{
entry:
  {body}
  ret void
}}
{declarations}
!air.kernel = !{{!0}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3, !4}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"{local_type}", !"air.arg_name", !"scratch"}}
!4 = !{{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"out"}}
"#)
}

fn validate(bytes: &[u8]) {
    // Required tool: absence is a test failure, never an unreported skip.
    let validator=std::env::var_os("METAL2VULKAN_SPIRV_VAL")
        .expect("METAL2VULKAN_SPIRV_VAL must name the independent CPU validator");
    let mut child=Command::new(validator).args(["--target-env","vulkan1.2","-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .expect("spawn independent spirv-val");
    child.stdin.take().unwrap().write_all(bytes).expect("validator input");
    let result=child.wait_with_output().expect("validator result");
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
}

fn translate(ll: &str, whole_groups: bool) -> Vec<u8> {
    let mut options=TransformOptions::default().with_threadgroup_memory_length(0,64).unwrap();
    if whole_groups { options.kernel_dispatch=Some(metal2vulkan::reflect::KernelDispatch::Workgroups); }
    let bytes=metal2vulkan::translate_native_no_retry_constructed_with_options(ll,Stage::Kernel,options)
        .expect("translate direct threadgroup-root atomic");
    validate(&bytes);bytes
}

fn check_atomic_pointers(bytes: &[u8], wanted: Op, scope: Scope) {
    let module=load_bytes(bytes).expect("SPIR-V parse");
    let defs:HashMap<_,_>=module.all_inst_iter().filter_map(|i|i.result_id.map(|id|(id,i))).collect();
    let atomics:Vec<_>=module.all_inst_iter().filter(|i|i.class.opcode==wanted).collect();
    assert!(!atomics.is_empty(),"expected {wanted:?}");
    for atomic in atomics {
        let Operand::IdRef(pointer)=atomic.operands[0] else {panic!("atomic pointer");};
        let pointer_def=defs[&pointer];let pointer_type=defs[&pointer_def.result_type.unwrap()];
        assert_eq!(pointer_type.class.opcode,Op::TypePointer);
        assert_eq!(pointer_type.operands[0],Operand::StorageClass(StorageClass::Workgroup));
        let Operand::IdRef(pointee)=pointer_type.operands[1] else {panic!("pointee");};
        assert_eq!(defs[&pointee].class.opcode,Op::TypeInt);
        assert_eq!(defs[&pointee].operands[0],Operand::LiteralBit32(32));
        let scope_id=match atomic.operands[1] {Operand::IdScope(x)|Operand::IdRef(x)=>x,_=>panic!("scope")};
        assert_eq!(defs[&scope_id].operands[0],Operand::LiteralBit32(scope as u32));
    }
}

#[test]
fn workgroup_atomic_root_signed_unsigned_rmw_keep_scalar_pointers() {
    for (name,opcode) in [("max.s",Op::AtomicSMax),("min.s",Op::AtomicSMin),
        ("max.u",Op::AtomicUMax),("min.u",Op::AtomicUMin),("add.u",Op::AtomicIAdd),
        ("sub.u",Op::AtomicISub),("and.u",Op::AtomicAnd),("or.u",Op::AtomicOr),
        ("xor.u",Op::AtomicXor),("xchg",Op::AtomicExchange)] {
        let callee=format!("air.atomic.local.{name}.i32");
        let ll=kernel(&format!("store i32 0, ptr addrspace(3) %scratch, align 4\n%old = call i32 @{callee}(ptr addrspace(3) %scratch, i32 -7, i32 0, i32 1, i1 true)\nstore i32 %old, ptr addrspace(1) %out, align 4"),
            &format!("declare i32 @{callee}(ptr addrspace(3), i32, i32, i32, i1)"),"int");
        for whole_groups in [false,true] {
            let bytes=translate(&ll,whole_groups);check_atomic_pointers(&bytes,opcode,Scope::Workgroup);
        }
    }
}

#[test]
fn workgroup_atomic_root_preserves_nonzero_element_address() {
    let ll=kernel(r#"
store i32 0, ptr addrspace(3) %scratch, align 4
%slot = getelementptr i32, ptr addrspace(3) %scratch, i64 1
%a = call i32 @air.atomic.local.max.s.i32(ptr addrspace(3) %scratch, i32 -7, i32 0, i32 1, i1 true)
%b = call i32 @air.atomic.local.max.s.i32(ptr addrspace(3) %slot, i32 11, i32 0, i32 1, i1 true)
%sum = add i32 %a, %b
store i32 %sum, ptr addrspace(1) %out, align 4
"#,"declare i32 @air.atomic.local.max.s.i32(ptr addrspace(3), i32, i32, i32, i1)","int");
    let bytes=translate(&ll,true);check_atomic_pointers(&bytes,Op::AtomicSMax,Scope::Workgroup);
    let module=load_bytes(&bytes).unwrap();
    let pointers:Vec<_>=module.all_inst_iter().filter(|i|i.class.opcode==Op::AtomicSMax).map(|i|i.operands[0].clone()).collect();
    assert_eq!(pointers.len(),2);assert_ne!(pointers[0],pointers[1],"element1 must not collapse to root element0");
    let defs:HashMap<_,_>=module.all_inst_iter().filter_map(|i|i.result_id.map(|id|(id,i))).collect();
    let element_indices:Vec<_>=pointers.iter().map(|pointer| {
        let Operand::IdRef(pointer)=pointer else {panic!("pointer");};
        let chain=defs[pointer];
        assert!(matches!(chain.class.opcode,Op::AccessChain|Op::InBoundsAccessChain));
        let Operand::IdRef(index)=chain.operands.last().unwrap() else {panic!("element index");};
        match defs[index].operands[0] {Operand::LiteralBit32(value)=>value,_=>panic!("constant element index")}
    }).collect();
    assert_eq!(element_indices,vec![0,1],"atomic byte offset must survive root decay");
}
