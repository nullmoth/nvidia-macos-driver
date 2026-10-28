use super::*;
use crate::native::tir::TirOperand;

fn parsed(ll: &str) -> LlModule {
    LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module")
}

fn function<'a>(module: &'a LlModule, name: &str) -> &'a LlFunction {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing function {name}"))
}

fn call_names(function: &LlFunction) -> Vec<&str> {
    function
        .carrier_insts()
        .filter_map(|instruction| instruction.call().as_ref())
        .map(|call| call.callee.as_str())
        .collect()
}

fn inline_bindings(function: &LlFunction) -> Vec<&crate::native::tir::TirInst> {
    function
        .carrier_insts()
        .filter(|instruction| instruction.opcode == "metal2vulkan.inline_parameter")
        .collect()
}

#[test]
fn inlines_the_general_one_block_leaf_axes() {
    let ll = r#"
define internal i32 @constant() {
  ret i32 7
}

define internal float @root(float %value) {
  %result = tail call float @air.sqrt(float %value)
  ret float %result
}

define internal i32 @lane(<2 x i32> %value) {
  %result = extractelement <2 x i32> %value, i64 1
  ret i32 %result
}

define void @main() {
  %slot = alloca i32
  %vector_slot = alloca <2 x i32>
  %vector = load <2 x i32>, ptr %vector_slot
  %constant = call i32 @constant()
  %root = call float @root(float 4.000000e+00)
  %lane = call i32 @lane(<2 x i32> %vector)
  %sum = add i32 %constant, %lane
  ret void
}

declare float @air.sqrt(float)
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 3,
            helper_instances: 3,
        }
    );
    let main = function(&module, "main");
    let calls = call_names(main);
    assert_eq!(calls, vec!["air.sqrt"], "only the declaration call remains");
    assert_eq!(
        module.functions.len(),
        1,
        "fully migrated helper bodies are pruned before emission"
    );

    let root_call = main
        .carrier_insts()
        .find(|instruction| {
            instruction
                .call()
                .as_ref()
                .is_some_and(|call| call.callee == "air.sqrt")
        })
        .expect("cloned declaration call");
    assert!(matches!(
        root_call.operands.first(),
        Some(TirOperand::Value { name, ty: LlType::Float })
            if name.contains(".param.")
    ));
    assert!(inline_bindings(main).iter().any(|binding| matches!(
        binding.operands.first(),
        Some(TirOperand::Const {
            value: LlValue::Float(value),
            ty: LlType::Float,
        }) if *value == 4.0
    )));

    let sum = main
        .carrier_insts()
        .find(|instruction| instruction.result.as_deref() == Some("%sum"))
        .expect("sum");
    assert!(matches!(
        sum.operands.first(),
        Some(TirOperand::Const {
            value: LlValue::Int(7),
            ty: LlType::Int(32),
        })
    ));
}

#[test]
fn records_type_capabilities_from_pruned_functions() {
    let ll = r#"
define internal half @half_leaf(half %value) {
  ret half %value
}

define internal i64 @wide_leaf(i64 %value) {
  ret i64 %value
}

define void @main() {
  %half = call half @half_leaf(half 0xH0000)
  %wide = call i64 @wide_leaf(i64 7)
  ret void
}
"#;
    let mut module = parsed(ll);

    module.inline_ordinary_leaf_helpers();

    assert_eq!(
        module.preinlined_helper_type_capabilities,
        crate::fxhash::set_from([LlTypeCapability::Float16, LlTypeCapability::Int64,])
    );

    let mut pointer_module = parsed(
        r#"
define internal void @pointer_leaf(ptr %pointer) {
  ret void
}

define void @main(ptr %pointer) {
  call void @pointer_leaf(ptr %pointer)
  ret void
}
"#,
    );
    pointer_module.inline_ordinary_leaf_helpers();
    assert_eq!(
        pointer_module.preinlined_helper_type_capabilities,
        crate::fxhash::set_from([LlTypeCapability::Int8])
    );
}

#[test]
fn inlines_pointer_parameter_in_pointer_only_module() {
    let ll = r#"
define internal void @write(ptr %pointer, i32 %value) {
  store i32 %value, ptr %pointer
  ret void
}

define void @main() {
  %slot = alloca i32
  call void @write(ptr %slot, i32 9)
  ret void
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 1,
            helper_instances: 1,
        }
    );
    let main = function(&module, "main");
    assert!(call_names(main).is_empty());
    let store = main
        .carrier_insts()
        .find(|instruction| instruction.opcode == "store")
        .expect("cloned store");
    assert!(matches!(
        store.operands.as_slice(),
        [
            TirOperand::Value {
                name: value,
                ty: LlType::Int(32),
            },
            TirOperand::Value {
                name: pointer,
                ty: LlType::Ptr(0),
            },
        ] if value.contains(".param.") && pointer.contains(".param.")
    ));
}

#[test]
fn repeated_calls_share_one_helper_instance_and_keep_names_hygienic() {
    let ll = r#"
define internal i32 @add(i32 %left, i32 %right) {
  %sum = add i32 %left, %right
  ret i32 %sum
}

define void @main() {
  %slot = alloca i32
  %sum = load i32, ptr %slot
  %first = call i32 @add(i32 %sum, i32 1)
  %second = call i32 @add(i32 %first, i32 2)
  store i32 %second, ptr %slot
  ret void
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 2,
            helper_instances: 1,
        }
    );
    let main = function(&module, "main");
    assert!(call_names(main).is_empty());
    let additions = main
        .carrier_insts()
        .filter(|instruction| instruction.opcode == "add")
        .collect::<Vec<_>>();
    assert_eq!(additions.len(), 2);
    let first_result = additions[0].result.as_deref().expect("first cloned result");
    let second_result = additions[1]
        .result
        .as_deref()
        .expect("second cloned result");
    assert_ne!(first_result, second_result);
    assert!(matches!(
        additions[0].operands.first(),
        Some(TirOperand::Value { name, .. }) if name.contains(".param.")
    ));
    assert!(matches!(
        additions[1].operands.first(),
        Some(TirOperand::Value { name, .. }) if name.contains(".param.")
    ));
    let bindings = inline_bindings(main);
    assert!(bindings.iter().any(|binding| matches!(
        binding.operands.first(),
        Some(TirOperand::Value { name, .. }) if name == "%sum"
    )));
    assert!(bindings.iter().any(|binding| matches!(
        binding.operands.first(),
        Some(TirOperand::Value { name, .. }) if name == first_result
    )));
    let store = main
        .carrier_insts()
        .find(|instruction| instruction.opcode == "store")
        .expect("store");
    assert!(matches!(
        store.operands.first(),
        Some(TirOperand::Value { name, .. }) if name == second_result
    ));
}

#[test]
fn leaves_multiblock_and_other_mechanisms_residual() {
    let ll = r#"
define internal i32 @leaf(i32 %value) {
  %result = add i32 %value, 1
  ret i32 %result
}

define internal i32 @has_alloca(i32 %value) {
  %slot = alloca i32
  store i32 %value, ptr %slot
  %result = load i32, ptr %slot
  ret i32 %result
}

define internal i32 @has_bodied_callee(i32 %value) {
  %result = call i32 @leaf(i32 %value)
  ret i32 %result
}

define internal void @has_indirect(ptr %function) {
  call void %function()
  ret void
}

define internal i32 @has_cfg(i32 %value) {
entry:
  %condition = icmp eq i32 %value, 0
  br i1 %condition, label %zero, label %other
zero:
  ret i32 0
other:
  ret i32 %value
}

define void @main(ptr %function) {
  %a = call i32 @has_alloca(i32 1)
  %b = call i32 @has_bodied_callee(i32 %a)
  call void @has_indirect(ptr %function)
  %c = call i32 @has_cfg(i32 %b)
  ret void
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    // WAS splices 1 with has_bodied_callee residual: one pass spliced the leaf into it and never came back. Rounds now
    // repeat, so the wrapper is a leaf in round two; alloca, indirect calls and real control flow still stay residual.
    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 2,
            helper_instances: 2,
        },
        "the leaf, then the wrapper that became a leaf"
    );
    assert_eq!(
        call_names(function(&module, "main")),
        vec!["has_alloca", "has_indirect", "has_cfg"]
    );
}

#[test]
fn multiblock_entry_loop_remains_residual() {
    let ll = r#"
define internal i32 @count(i32 %limit) {
entry:
  %done = icmp eq i32 %limit, 4
  br i1 %done, label %exit, label %entry
exit:
  ret i32 %limit
}

define i32 @main() {
entry:
  %value = call i32 @count(i32 4)
  br label %merge
merge:
  %result = phi i32 [ %value, %entry ]
  ret i32 %result
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(stats, TypedInlineStats::default());
    let main = function(&module, "main");
    assert_eq!(call_names(main), vec!["count"]);
    assert_eq!(main.blocks.len(), 2);
}

#[test]
fn propagates_cloned_pointer_pointee_facts() {
    let ll = r#"
%Pair = type { i32, i32 }

define internal void @write_second(ptr %pair, i32 %value) {
  %field = getelementptr inbounds %Pair, ptr %pair, i64 0, i32 1
  store i32 %value, ptr %field
  ret void
}

define void @main() {
  %pair = alloca %Pair
  call void @write_second(ptr %pair, i32 5)
  ret void
}
"#;
    let mut module = parsed(ll);
    assert!(module
        .ptr_pointees
        .contains_key(&("write_second".to_string(), "%pair".to_string())));

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(stats.splices, 1);
    assert!(module
        .ptr_pointees
        .iter()
        .any(|((function, local), pointee)| {
            function == "main"
                && local.contains(".param.")
                && pointee == &LlType::Named("%Pair".to_string())
        }));
}

#[test]
fn conflicting_caller_and_helper_pointer_pointees_remain_residual() {
    let ll = r#"
define float @main() {
entry:
  %slot = alloca float
  store float 0.000000e+00, ptr %slot
  %value = call float @load_bits(ptr %slot)
  ret float %value
}

define internal float @load_bits(ptr %pointer) {
entry:
  %bits = load i32, ptr %pointer
  %value = bitcast i32 %bits to float
    ret float %value
}
"#;
    let mut module = parsed(ll);
    assert_eq!(
        module
            .ptr_pointees
            .get(&("load_bits".to_string(), "%pointer".to_string())),
        Some(&LlType::Int(32))
    );

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(stats, TypedInlineStats::default());
    assert_eq!(call_names(function(&module, "main")), vec!["load_bits"]);
}

#[test]
fn mixed_pointer_and_value_helpers_inline_independently() {
    let ll = r#"
define internal void @write(ptr %pointer, i32 %value) {
  store i32 %value, ptr %pointer
  ret void
}

define internal i32 @increment(i32 %value) {
  %result = add i32 %value, 1
  ret i32 %result
}

define void @main() {
  %slot = alloca i32
  %value = call i32 @increment(i32 8)
  call void @write(ptr %slot, i32 %value)
  ret void
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 2,
            helper_instances: 2,
        }
    );
    assert!(call_names(function(&module, "main")).is_empty());
}

#[test]
fn residual_static_initializer_is_a_reachability_root() {
    let ll = r#"
define internal void @leaf(ptr %pointer) {
  store i32 1, ptr %pointer
  ret void
}

define internal void @_GLOBAL__sub_I_residual() section "air.static_init" {
  %slot = alloca i32
  call void @leaf(ptr %slot)
  ret void
}

define void @main() {
  ret void
}
"#;
    let mut module = parsed(ll);

    let stats = module.inline_ordinary_leaf_helpers();

    assert_eq!(
        stats,
        TypedInlineStats {
            splices: 1,
            helper_instances: 1,
        }
    );
    assert!(
        call_names(function(&module, "_GLOBAL__sub_I_residual")).is_empty(),
        "the emitter-injected constructor root must not hide its reachable leaf"
    );
}

#[test]
fn a_helper_that_calls_a_wrapper_is_spliced_once_the_wrapper_is() {
    // Geekbench 6 OpenCL Feature Matching: compare_intensity(ptr image cursor) calls the cos/round wrappers. One pass
    // spliced the wrappers into it but never came back, so the call carrying a buffer pointer stayed and the kernel
    // failed ("byte cursor cannot cross the call"). Every round is a one-block leaf splice; the call graph is gone.
    let ll = r#"
define internal float @wcos(float %x) {
  %r = tail call float @air.cos(float %x)
  ret float %r
}

define internal i32 @compare(ptr addrspace(1) %cur, float %angle) {
  %c = tail call float @wcos(float %angle)
  %v = load i32, ptr addrspace(1) %cur
  %f = fptosi float %c to i32
  %r = add i32 %v, %f
  ret i32 %r
}

define void @main(ptr addrspace(1) %image) {
  %r = call i32 @compare(ptr addrspace(1) %image, float 1.000000e+00)
  ret void
}

declare float @air.cos(float)
"#;
    let mut module = parsed(ll);
    let stats = module.inline_ordinary_leaf_helpers();
    let main = function(&module, "main");
    assert_eq!(call_names(main), vec!["air.cos"], "main keeps only the intrinsic, no helper call");
    assert_eq!(stats.splices, 2, "wcos into compare, then compare into main");
}
