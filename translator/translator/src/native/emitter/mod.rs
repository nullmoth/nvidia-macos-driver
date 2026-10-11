use super::cfg::{
    funnel_shared_branch_dispatches, index_branch_merges_by_header,
    infer_bounded_branch_merges_by_header, infer_branch_merges, infer_direct_branch_merges,
    infer_loop_merges, infer_switch_merges, infer_switch_merges_bounded,
    lower_unstructured_switches, privatize_reused_emitted_merge_targets,
    refunnel_one_deep_shared_arm, LoopMergeInfo,
};
use super::ir::{
    LlDeclaration, LlFunction, LlGep, LlGlobal, LlModule, LlType, LlTypeCapability, LlValue,
    TypedValue,
};
use super::parse::{
    fcmp_predicate, float_compare_result_type, icmp_predicate, int_compare_result_type,
    is_ignored_call_line, matching_paren, parse_type, parse_typed_value, parse_value,
    split_top_level, split_top_level_whitespace, strip_call_prefix, switch_literal_operand, LlCall,
    LlLoad,
};
use crate::spirv_module::Operand;
use crate::spirv_module::{
    is_block_terminator, Block, Function, Instruction, Module, ModuleHeader,
};
use crate::types::TypeInterner;
use spirv::{
    AddressingModel, Capability, FunctionControl, GlslStd450Op, LoopControl, MemoryAccess,
    MemoryModel, MemorySemantics, Op, Scope, SelectionControl, SourceLanguage, StorageClass, Word,
};
use std::collections::{HashMap, HashSet};

mod air_struct_offsets;
mod body;
mod control;
pub(super) mod functions;
mod helpers;
mod memory;
mod ops;
mod pointer_network;
mod pointers;
mod staged;
mod types;

#[cfg(test)]
mod layout_tests;

use helpers::*;
use staged::staged_emit;

pub(super) struct Emitter {
    module: Module,
    ir: LlModule,
    emit_sidecar: crate::emit_sidecar::EmitSidecar,
    air_data_layout: Option<crate::layout::AirDataLayout>,
    interner: TypeInterner,
    glsl_ext: Option<Word>,
    values: HashMap<String, (Word, LlType)>,
    fast_float_products: HashMap<String, (TypedValue, TypedValue)>,
    fast_contract_adds: HashSet<String>,
    fast_uncontracted_sums: HashSet<String>,
    global_values: HashMap<String, (Word, LlType)>,
    int_atomic_reinterpret_globals: HashSet<String>,
    byte_view_reinterpret_globals: HashSet<String>,
    flat_scalar_reinterpret_globals: HashMap<String, LlType>,
    gep_provenance: HashMap<String, GepProvenance>,
    workgroup_padding_byte_pointers: HashMap<String, WorkgroupPaddingBytePointer>,
    selected_pointers: HashMap<String, SelectedPointer>,
    selected_load_pointers: HashMap<String, SelectedLoadPointer>,
    selected_access_trees: HashMap<String, SelectedAccessTree>,
    vector_word_roots: HashMap<Word, VectorWordRoot>,
    vector_word_pointers: HashMap<String, VectorWordPointer>,
    local_pointer_fields: HashMap<LocalPointerField, TypedValue>,
    pointer_forward_values: HashMap<String, TypedValue>,
    raw_memcpy_shadows: HashMap<String, Vec<(u64, Word)>>,
    imageblock_data_scratch: Option<(Word, LlType)>,
    dynamic_pointer_tables: HashMap<String, DynamicPointerTable>,
    forward_geps: HashMap<String, LlGep>,
    forward_pointer_selects: HashMap<String, (TypedValue, TypedValue)>,
    forward_pointer_select_conditions: HashMap<String, TypedValue>,
    pointer_storage: HashMap<String, StorageClass>,
    pointer_pointees: HashMap<String, LlType>,
    local_alloca_pointees: HashMap<String, LlType>,
    pointer_nullness: HashMap<String, Word>,
    bda_inttoptr_sources: HashMap<String, TypedValue>,
    opaque_resource_payload_loads: HashMap<String, String>,
    bda_forward_sources: HashMap<String, TypedValue>,
    bda_address_loads: HashSet<String>,
    bda_forward_addresses: HashSet<String>,
    bda_pointer_tables: HashSet<String>,
    bda_direct_addresses: HashMap<String, Word>,
    bda_address_values: HashSet<Word>,
    bda_pointer_addresses: HashMap<Word, Word>,
    bda_aggregate_addresses: HashMap<String, HashMap<Vec<u32>, Word>>,
    aggregate_pointer_values: HashMap<String, HashMap<Vec<u32>, TypedValue>>,
    opaque_resource_pointer_values: HashMap<String, HashSet<String>>,
    opaque_resource_pointers: HashSet<String>,
    opaque_sampler_pointers: HashSet<String>,
    opaque_resource_ids: HashSet<Word>,
    pointer_payload_words: HashMap<String, (Word, Word)>,
    pointer_payload_values: HashSet<String>,
    pointer_phi_values: HashSet<String>,
    pointer_phi_incoming_values: HashSet<String>,
    tir_phi_incomings: HashMap<String, Vec<(LlValue, String)>>,
    function_param_pointees: HashMap<(String, usize), LlType>,
    function_param_nonnull: HashSet<(String, usize)>,
    function_param_nullness: HashSet<(String, usize)>,
    direct_param_values: HashSet<String>,
    direct_param_indices: HashMap<String, u32>,
    param_values: HashSet<String>,
    current_parameter_types: HashMap<Word, Word>,
    raw_buffer_layout_required: bool,
    inline_parameter_substitutions: Vec<(Word, Word)>,
    raw_buffer_params: HashSet<String>,
    raw_call_param_offsets: HashMap<(String, String), RawBufferOffset>,
    cursor_call_sites: HashSet<(String, String)>,
    agreeing_constant_call_cursors: Option<HashSet<(String, usize)>>,
    data_buffer_params: HashSet<String>,
    raw_offsets: HashMap<String, RawBufferOffset>,
    symbolic_buffer_addresses: HashMap<String, RawBufferOffset>,
    int_alignments: HashMap<String, u64>,
    unmodeled_pointers: HashSet<String>,
    workgroup_i32_addresses: HashMap<(u64, u32), Word>,
    null_texture_values: HashSet<String>,
    function_ids: HashMap<String, Word>,
    block_labels: HashMap<String, Word>,
    branch_merges: HashMap<(String, String), String>,
    branch_merges_by_header: HashMap<String, String>,
    branch_merges_header_only: bool,
    loop_merges: HashMap<String, LoopMergeInfo>,
    switch_merges: HashMap<String, String>,
    current_block: Option<String>,
    loop_controls: HashMap<String, (spirv::LoopControl, Option<u32>)>,
    phi_edge_instructions: HashMap<Word, Vec<Instruction>>,
    phi_result_instructions: Vec<Instruction>,
    tir_result_types: HashMap<String, crate::native::ir::LlType>,
    tir_predicates: HashMap<String, String>,
    tir_aligns: HashMap<String, Option<u64>>,
    tir_gep_source_types: HashMap<String, crate::native::ir::LlType>,
    tir_use_pointees: HashMap<String, crate::native::ir::LlType>,
    tir_direct_load_pointers: HashSet<String>,
    byte_view_pointers: std::collections::HashSet<String>,
    network_pointees: HashMap<String, crate::native::ir::LlType>,
    null_rooted_pointer_values: HashSet<String>,
    null_rooted_pointer_peers: HashMap<String, Vec<String>>,
    construct_tree_active: bool,
    known_ordinary_plan_rejections: HashSet<String>,
    ordinary_plan_rejected_shapes: HashSet<String>,
    known_ownership_plan_rejections: HashSet<String>,
    relooper_feed: bool,
    capture_storage: bool,
    storage_snapshots: Vec<(String, HashMap<String, StorageClass>)>,
    capture_pointees: bool,
    pointee_snapshots: Vec<(String, HashMap<String, LlType>)>,
    bda_device_pointers: bool,
    constant_space_load: bool,
    nonwritable_psb_wrappers: HashMap<LlType, Word>,
    null_bda_load_helpers: HashMap<(LlType, bool), Word>,
    used_device_address: bool,
}

#[derive(Clone, Debug)]
struct GepProvenance {
    root: Word,
    addrspace: u32,
    source_ty: LlType,
    indices: Vec<TypedValue>,
    root_indices: Option<Vec<TypedValue>>,
    root_is_indexed_container: bool,
}

fn bda_address_name(name: &str) -> String {
    format!("{name}.metal2vulkan.bda_address")
}

#[derive(Clone, Debug)]
struct WorkgroupPaddingBytePointer {
    struct_ty: LlType,
    byte_offset: u64,
}

#[derive(Clone, Debug)]
struct SelectedPointer {
    cond: Word,
    true_value: LlValue,
    false_value: LlValue,
    ty: LlType,
}

#[derive(Clone, Debug)]
struct SelectedLoadPointer {
    cond: Word,
    true_ptr: Option<Word>,
    false_ptr: Option<Word>,
    true_storage: StorageClass,
    false_storage: StorageClass,
    pointee: LlType,
    true_raw: Option<RawBufferOffset>,
    false_raw: Option<RawBufferOffset>,
}

#[derive(Clone, Debug)]
struct SelectedAccessTree {
    cond: Word,
    true_arm: SelectedAccessArm,
    false_arm: SelectedAccessArm,
    pointee: LlType,
}

#[derive(Clone, Debug)]
enum SelectedAccessArm {
    Typed { ptr: Word, storage: StorageClass },
    Raw(RawBufferOffset),
    Nested(Box<SelectedAccessTree>),
    Null,
}

struct SelectedGepShape<'a> {
    source_ty: &'a LlType,
    pointee: &'a LlType,
    indices: &'a [TypedValue],
}

#[derive(Clone, Debug)]
struct DynamicPointerTable {
    selector: Word,
    selector_bits: u32,
    entries: Vec<(u32, TypedValue)>,
}

#[derive(Clone, Debug)]
struct VectorWordRoot {
    storage: StorageClass,
    vector_ty: LlType,
    lanes: u32,
    lanes_per_word: u32,
    words_per_vector: u32,
    base_is_vector_pointer: bool,
}

#[derive(Clone, Debug)]
struct VectorWordPointer {
    base: Word,
    storage: StorageClass,
    vector_ty: LlType,
    lanes: u32,
    lanes_per_word: u32,
    words_per_vector: u32,
    base_is_vector_pointer: bool,
    word_index: Word,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LocalPointerField {
    root: Word,
    indices: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PointerMeta {
    storage: StorageClass,
    pointee: Option<LlType>,
}

#[derive(Clone, Debug)]
struct RawBufferOffset {
    const_off: i64,
    dyn_terms: Vec<(TypedValue, i64)>,
    root: String,
    addrspace: u32,
    unmodelable: bool,
    device_addr_base: Option<Word>,
}

impl RawBufferOffset {
    fn root(root: String, addrspace: u32) -> Self {
        Self {
            const_off: 0,
            dyn_terms: vec![],
            root,
            addrspace,
            unmodelable: false,
            device_addr_base: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RawSubwordLane {
    Static(u32),
    Dynamic(Word),
}

impl Emitter {
    pub(super) fn require_capability(&mut self, capability: Capability) {
        if self.module.capabilities.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [Operand::Capability(existing)] if *existing == capability
            )
        }) {
            return;
        }
        self.module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(capability)],
        ));
    }

    pub(super) fn require_extension(&mut self, extension: &str) {
        if self.module.extensions.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [Operand::LiteralString(existing)] if existing == extension
            )
        }) {
            return;
        }
        self.module.extensions.push(Instruction::new(
            Op::Extension,
            None,
            None,
            vec![Operand::LiteralString(extension.to_string())],
        ));
    }

    pub(super) fn new(mut ir: LlModule) -> Self {
        ir.inline_simple_static_initializers();
        ir.fold_static_initializer_constants();
        ir.prune_unreachable_function_bodies();
        ir.inline_ordinary_leaf_helpers();
        let opaque_resource_pointer_values =
            functions::opaque_resource_pointer_values_by_function(&ir);
        let air_data_layout = ir.air_data_layout.clone();
        let mut module = Module::new();
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.memory_model = Some(Instruction::new(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.debug_string_source.push(Instruction::new(
            Op::Source,
            None,
            None,
            vec![
                Operand::SourceLanguage(SourceLanguage::Unknown),
                Operand::LiteralBit32(0),
            ],
        ));
        Self {
            module,
            ir,
            emit_sidecar: crate::emit_sidecar::EmitSidecar::default(),
            air_data_layout,
            interner: TypeInterner::new(),
            glsl_ext: None,
            values: HashMap::new(),
            fast_float_products: HashMap::new(),
            fast_contract_adds: HashSet::new(),
            fast_uncontracted_sums: HashSet::new(),
            global_values: HashMap::new(),
            int_atomic_reinterpret_globals: HashSet::new(),
            byte_view_reinterpret_globals: HashSet::new(),
            flat_scalar_reinterpret_globals: HashMap::new(),
            gep_provenance: HashMap::new(),
            workgroup_padding_byte_pointers: HashMap::new(),
            selected_pointers: HashMap::new(),
            selected_load_pointers: HashMap::new(),
            selected_access_trees: HashMap::new(),
            vector_word_roots: HashMap::new(),
            vector_word_pointers: HashMap::new(),
            local_pointer_fields: HashMap::new(),
            pointer_forward_values: HashMap::new(),
            raw_memcpy_shadows: HashMap::new(),
            imageblock_data_scratch: None,
            dynamic_pointer_tables: HashMap::new(),
            forward_geps: HashMap::new(),
            forward_pointer_selects: HashMap::new(),
            forward_pointer_select_conditions: HashMap::new(),
            pointer_storage: HashMap::new(),
            pointer_pointees: HashMap::new(),
            local_alloca_pointees: HashMap::new(),
            pointer_nullness: HashMap::new(),
            bda_inttoptr_sources: HashMap::new(),
            opaque_resource_payload_loads: HashMap::new(),
            bda_forward_sources: HashMap::new(),
            bda_address_loads: HashSet::new(),
            bda_forward_addresses: HashSet::new(),
            bda_pointer_tables: HashSet::new(),
            bda_direct_addresses: HashMap::new(),
            bda_address_values: HashSet::new(),
            bda_pointer_addresses: HashMap::new(),
            bda_aggregate_addresses: HashMap::new(),
            aggregate_pointer_values: HashMap::new(),
            opaque_resource_pointer_values,
            opaque_resource_pointers: HashSet::new(),
            opaque_sampler_pointers: HashSet::new(),
            opaque_resource_ids: HashSet::new(),
            pointer_payload_words: HashMap::new(),
            pointer_payload_values: HashSet::new(),
            pointer_phi_values: HashSet::new(),
            pointer_phi_incoming_values: HashSet::new(),
            tir_phi_incomings: HashMap::new(),
            function_param_pointees: HashMap::new(),
            function_param_nonnull: HashSet::new(),
            function_param_nullness: HashSet::new(),
            direct_param_values: HashSet::new(),
            direct_param_indices: HashMap::new(),
            param_values: HashSet::new(),
            current_parameter_types: HashMap::new(),
            raw_buffer_layout_required: false,
            inline_parameter_substitutions: Vec::new(),
            raw_buffer_params: HashSet::new(),
            raw_call_param_offsets: HashMap::new(),
            cursor_call_sites: HashSet::new(),
            agreeing_constant_call_cursors: None,
            data_buffer_params: HashSet::new(),
            raw_offsets: HashMap::new(),
            symbolic_buffer_addresses: HashMap::new(),
            int_alignments: HashMap::new(),
            unmodeled_pointers: HashSet::new(),
            workgroup_i32_addresses: HashMap::new(),
            null_texture_values: HashSet::new(),
            function_ids: HashMap::new(),
            block_labels: HashMap::new(),
            branch_merges: HashMap::new(),
            branch_merges_by_header: HashMap::new(),
            branch_merges_header_only: false,
            loop_merges: HashMap::new(),
            switch_merges: HashMap::new(),
            current_block: None,
            loop_controls: HashMap::new(),
            phi_edge_instructions: HashMap::new(),
            phi_result_instructions: Vec::new(),
            tir_result_types: HashMap::new(),
            tir_predicates: HashMap::new(),
            tir_aligns: HashMap::new(),
            tir_gep_source_types: HashMap::new(),
            tir_use_pointees: HashMap::new(),
            tir_direct_load_pointers: HashSet::new(),
            network_pointees: HashMap::new(),
            null_rooted_pointer_values: HashSet::new(),
            null_rooted_pointer_peers: HashMap::new(),
            byte_view_pointers: std::collections::HashSet::new(),
            construct_tree_active: false,
            known_ordinary_plan_rejections: HashSet::new(),
            ordinary_plan_rejected_shapes: HashSet::new(),
            known_ownership_plan_rejections: HashSet::new(),
            relooper_feed: false,
            capture_storage: false,
            storage_snapshots: Vec::new(),
            capture_pointees: false,
            pointee_snapshots: Vec::new(),
            bda_device_pointers: false,
            constant_space_load: false,
            nonwritable_psb_wrappers: HashMap::new(),
            null_bda_load_helpers: HashMap::new(),
            used_device_address: false,
        }
    }

    pub(super) fn with_bda_device_pointers(mut self) -> Self {
        self.bda_device_pointers = true;
        self
    }

    pub(super) fn with_known_plan_rejections(
        mut self,
        ordinary: &HashSet<String>,
        ownership: &HashSet<String>,
    ) -> Self {
        self.known_ordinary_plan_rejections.clone_from(ordinary);
        self.known_ownership_plan_rejections.clone_from(ownership);
        self
    }

    pub(super) fn with_relooper_feed(mut self) -> Self {
        self.relooper_feed = true;
        self
    }

    pub(super) fn glsl_ext_inst_import(&mut self) -> Word {
        if let Some(id) = self.glsl_ext {
            return id;
        }
        for inst in &self.module.ext_inst_imports {
            if let Some(Operand::LiteralString(s)) = inst.operands.first() {
                if s == "GLSL.std.450" {
                    let id = inst.result_id.expect("GLSL import result id");
                    self.glsl_ext = Some(id);
                    return id;
                }
            }
        }
        let id = self.fresh();
        self.module.ext_inst_imports.push(Instruction::new(
            Op::ExtInstImport,
            None,
            Some(id),
            vec![Operand::LiteralString("GLSL.std.450".into())],
        ));
        self.glsl_ext = Some(id);
        id
    }

    fn infer_function_param_pointees(&mut self, functions: &[LlFunction]) -> Result<(), String> {
        let functions_by_name: HashMap<String, &LlFunction> = functions
            .iter()
            .map(|function| (function.name.clone(), function))
            .collect();
        let mut inferred = HashMap::new();
        let mut ambiguous = HashSet::new();
        for function in functions {
            let local_pointees = self.local_call_arg_pointees(function)?;
            for inst in function.carrier_insts() {
                let Some(call_result) = inst.emit_scan_call() else {
                    continue;
                };
                let call = call_result?;
                let Some(callee) = functions_by_name.get(&call.callee) else {
                    continue;
                };
                for (index, arg) in call.args.iter().enumerate() {
                    if index >= callee.params.len() {
                        break;
                    }
                    if !matches!(self.resolve_type(&callee.params[index].1)?, LlType::Ptr(_)) {
                        continue;
                    }
                    let Some(pointee) = self.call_arg_pointee(&arg.value, &local_pointees) else {
                        continue;
                    };
                    let callee_param = &callee.params[index].0;
                    let callee_param_ty = &callee.params[index].1;
                    if !self.callee_param_accepts_call_pointee(
                        &callee.name,
                        callee_param,
                        callee_param_ty,
                        &pointee,
                    ) {
                        continue;
                    }
                    let pointee = self.byte_image_leaf_array(&pointee).unwrap_or(pointee);
                    let key = (callee.name.clone(), index);
                    if ambiguous.contains(&key) {
                        continue;
                    }
                    if let Some(existing) = inferred.get(&key) {
                        if !types_compatible(existing, &pointee) {
                            inferred.remove(&key);
                            ambiguous.insert(key);
                        }
                    } else {
                        inferred.insert(key, pointee);
                    }
                }
            }
        }
        self.function_param_pointees = inferred;
        Ok(())
    }

    pub(super) fn infer_function_param_nonnull(
        &self,
        functions: &[LlFunction],
    ) -> Result<HashSet<(String, usize)>, String> {
        let functions_by_name: HashMap<String, &LlFunction> = functions
            .iter()
            .map(|function| (function.name.clone(), function))
            .collect();
        let mut all_nonnull = HashSet::new();
        let mut seen = HashSet::new();
        let mut rejected = HashSet::new();
        for function in functions {
            let local_nonnull = self.local_call_arg_nonnull_values(function)?;
            for inst in function.carrier_insts() {
                let Some(call_result) = inst.emit_scan_call() else {
                    continue;
                };
                let call = call_result?;
                let Some(callee) = functions_by_name.get(&call.callee) else {
                    continue;
                };
                for (index, arg) in call.args.iter().enumerate() {
                    if index >= callee.params.len() {
                        break;
                    }
                    if !matches!(self.resolve_type(&callee.params[index].1)?, LlType::Ptr(_)) {
                        continue;
                    }
                    let key = (callee.name.clone(), index);
                    if rejected.contains(&key) {
                        continue;
                    }
                    seen.insert(key.clone());
                    if self.call_arg_known_nonnull(&arg.value, &local_nonnull) {
                        all_nonnull.insert(key);
                    } else {
                        all_nonnull.remove(&key);
                        rejected.insert(key);
                    }
                }
            }
        }
        all_nonnull.retain(|key| seen.contains(key) && !rejected.contains(key));
        Ok(all_nonnull)
    }

    pub(super) fn infer_function_param_nullness(
        &self,
        functions: &[LlFunction],
    ) -> Result<HashSet<(String, usize)>, String> {
        let function_param_nonnull = self.infer_function_param_nonnull(functions)?;
        let mut required = HashSet::new();
        for function in functions {
            for inst in function.carrier_insts() {
                if !matches!(inst.cmp_predicate().as_deref(), Some("eq" | "ne")) {
                    continue;
                }
                let operands = inst
                    .operands
                    .iter()
                    .map(crate::native::tir::TirOperand::as_typed_value)
                    .collect::<Option<Vec<_>>>();
                let Some(operands) = operands else {
                    continue;
                };
                let [lhs, rhs] = operands.as_slice() else {
                    continue;
                };
                if !matches!(self.resolve_type(&lhs.ty)?, LlType::Ptr(_)) {
                    continue;
                }
                let observed = match (&lhs.value, &rhs.value) {
                    (LlValue::Zero, LlValue::Local(name))
                    | (LlValue::Local(name), LlValue::Zero) => name,
                    _ => continue,
                };
                if let Some(index) = function
                    .params
                    .iter()
                    .position(|(name, _)| name == observed)
                {
                    required.insert((function.name.clone(), index));
                }
            }
        }

        let mut changed = true;
        while changed {
            changed = false;
            for function in functions {
                for inst in function.carrier_insts() {
                    let Some(call) = inst.call().as_ref() else {
                        continue;
                    };
                    for (callee_index, argument) in call.args.iter().enumerate() {
                        if !required.contains(&(call.callee.clone(), callee_index)) {
                            continue;
                        }
                        let LlValue::Local(argument_name) = &argument.value else {
                            continue;
                        };
                        let Some(caller_index) = function
                            .params
                            .iter()
                            .position(|(name, _)| name == argument_name)
                        else {
                            continue;
                        };
                        changed |= required.insert((function.name.clone(), caller_index));
                    }
                }
            }
        }
        required.retain(|key| {
            !self.ir.entry_functions.contains(&key.0) && !function_param_nonnull.contains(key)
        });
        Ok(required)
    }

    fn local_call_arg_nonnull_values(
        &self,
        function: &LlFunction,
    ) -> Result<HashSet<String>, String> {
        let mut nonnull = HashSet::new();
        if self.ir.entry_functions.contains(&function.name) {
            for (param, ty) in &function.params {
                if matches!(self.resolve_type(ty)?, LlType::Ptr(_)) {
                    nonnull.insert(param.clone());
                }
            }
        }
        for inst in function.carrier_insts() {
            if inst.opcode == "alloca" {
                if let Some(name) = &inst.result {
                    nonnull.insert(name.clone());
                }
            }
        }

        let mut changed = true;
        while changed {
            changed = false;
            for inst in function.carrier_insts() {
                let Some(name) = &inst.result else {
                    continue;
                };
                if nonnull.contains(name) {
                    continue;
                }
                if let Some((src, dst_text)) = inst.bitcast() {
                    if matches!(self.resolve_type(&parse_type(dst_text)?)?, LlType::Ptr(_))
                        && self.call_arg_known_nonnull(&src.value, &nonnull)
                    {
                        nonnull.insert(name.clone());
                        changed = true;
                    }
                    continue;
                }
                if let Some(gep) = &inst.gep() {
                    if self.call_arg_known_nonnull(&gep.base.value, &nonnull)
                        || self.inbounds_gep_has_nonzero_constant_offset(gep)?
                    {
                        nonnull.insert(name.clone());
                        changed = true;
                    }
                }
            }
        }
        Ok(nonnull)
    }

    fn inbounds_gep_has_nonzero_constant_offset(&self, gep: &LlGep) -> Result<bool, String> {
        if !gep.inbounds {
            return Ok(false);
        }
        let Some(first) = const_index(gep.indices.first()) else {
            return Ok(false);
        };
        let source_ty = self.resolve_type(&gep.source_ty)?;
        if first != 0 && self.raw_type_size_align(&source_ty)?.0 != 0 {
            return Ok(true);
        }
        Ok(self
            .constant_aggregate_gep_offset(&source_ty, &gep.indices[1..])?
            .is_some_and(|(offset, _)| offset != 0))
    }

    fn call_arg_known_nonnull(&self, value: &LlValue, local_nonnull: &HashSet<String>) -> bool {
        match value {
            LlValue::Local(name) => local_nonnull.contains(name),
            LlValue::Global(_) => true,
            LlValue::Gep(gep) => self.call_arg_known_nonnull(&gep.base.value, local_nonnull),
            _ => false,
        }
    }

    fn local_call_arg_pointees(
        &self,
        function: &LlFunction,
    ) -> Result<HashMap<String, LlType>, String> {
        let mut pointees = HashMap::new();
        for (param, ty) in &function.params {
            if !matches!(ty, LlType::Ptr(_)) {
                continue;
            }
            if let Some(pointee) = self
                .ir
                .ptr_pointees
                .get(&(function.name.clone(), param.clone()))
                .cloned()
            {
                pointees.insert(param.clone(), pointee);
            }
        }

        let mut changed = true;
        while changed {
            changed = false;
            for inst in function.carrier_insts() {
                let Some(name) = &inst.result else {
                    continue;
                };
                if pointees.contains_key(name) {
                    continue;
                }
                if let Some((src, dst_text)) = inst.bitcast() {
                    let dst_ty = self.resolve_type(&parse_type(dst_text)?)?;
                    if matches!(dst_ty, LlType::Ptr(_)) {
                        if let LlValue::Local(src_name) = &src.value {
                            if let Some(pointee) = pointees.get(src_name).cloned() {
                                pointees.insert(name.clone(), pointee);
                                changed = true;
                            }
                        }
                    }
                    continue;
                }
                if let Some(gep) = &inst.gep() {
                    let LlValue::Local(base_name) = &gep.base.value else {
                        continue;
                    };
                    if pointees.contains_key(base_name) {
                        let pointee =
                            gep_pointee(&self.resolve_type(&gep.source_ty)?, &gep.indices)?;
                        pointees.insert(name.clone(), pointee);
                        changed = true;
                    }
                }
            }
        }
        Ok(pointees)
    }

    fn call_arg_pointee(
        &self,
        value: &LlValue,
        local_pointees: &HashMap<String, LlType>,
    ) -> Option<LlType> {
        match value {
            LlValue::Local(name) => local_pointees.get(name).cloned(),
            LlValue::Global(name) => self.pointer_pointees.get(name).cloned(),
            _ => None,
        }
    }

    fn callee_param_accepts_call_pointee(
        &self,
        callee_name: &str,
        callee_param: &str,
        callee_param_ty: &LlType,
        call_pointee: &LlType,
    ) -> bool {
        if matches!(callee_param_ty, LlType::Ptr(3)) {
            return true;
        }
        self.ir
            .ptr_pointees
            .get(&(callee_name.to_string(), callee_param.to_string()))
            .is_none_or(|local_pointee| {
                types_compatible(local_pointee, call_pointee)
                    || padding_widened_vector(local_pointee, call_pointee)
                    || self.byte_image_receives_param_view(local_pointee, call_pointee)
            })
    }

    pub(in crate::native::emitter) fn byte_image_leaf_array(&self, ty: &LlType) -> Option<LlType> {
        let mut current = self.ir.resolve_known_type(ty);
        loop {
            match current {
                LlType::Struct(fields) if fields.len() == 1 => {
                    current = self.ir.resolve_known_type(&fields[0]);
                }
                LlType::Array(elem, len) if *elem == LlType::Int(8) && len > 0 => {
                    return Some(LlType::Array(elem, len));
                }
                _ => return None,
            }
        }
    }

    fn byte_image_receives_param_view(
        &self,
        local_pointee: &LlType,
        call_pointee: &LlType,
    ) -> bool {
        if !self.ir.is_pure_byte_image(call_pointee) || self.ir.is_pure_byte_image(local_pointee) {
            return false;
        }
        let Some((image_bytes, _)) = self.ir.native_memcpy_type_size_align(call_pointee) else {
            return false;
        };
        match self.byte_value_lanes(local_pointee) {
            Ok(Some((lanes, lane_bytes, _))) => {
                u64::from(lanes) * u64::from(lane_bytes) <= image_bytes
            }
            _ => false,
        }
    }

    fn remove_dead_empty_functions(&mut self) {
        let mut called: HashSet<Word> = HashSet::new();
        for f in &self.module.functions {
            for b in &f.blocks {
                for inst in &b.instructions {
                    if inst.class.opcode == Op::FunctionCall {
                        if let Some(Operand::IdRef(callee)) = inst.operands.first() {
                            called.insert(*callee);
                        }
                    }
                }
            }
        }
        let mut removed: HashSet<Word> = HashSet::new();
        self.module.functions.retain(|f| {
            if !f.blocks.is_empty() {
                return true;
            }
            match f.def.as_ref().and_then(|d| d.result_id) {
                Some(id) if !called.contains(&id) => {
                    removed.insert(id);
                    false
                }
                _ => true,
            }
        });
        if !removed.is_empty() {
            self.module.debug_names.retain(|inst| {
                !(inst.class.opcode == Op::Name
                    && matches!(inst.operands.first(), Some(Operand::IdRef(id)) if removed.contains(id)))
            });
        }
    }

    fn emission_failure(&self, error: String) -> crate::emit_sidecar::EmissionFailure {
        crate::emit_sidecar::EmissionFailure {
            error,
            rejected: Box::new(self.emission_rejections()),
        }
    }

    fn emission_rejections(&self) -> crate::emit_sidecar::EmissionRejections {
        crate::emit_sidecar::EmissionRejections {
            ordinary_plan_functions: self.emit_sidecar.ordinary_plan_rejected_functions.clone(),
            ownership_plan_functions: self.emit_sidecar.ownership_plan_rejected_functions.clone(),
            cursor_call_sites: self.cursor_call_sites.clone(),
            raw_buffer_layout_required: self.raw_buffer_layout_required,
        }
    }

    pub(super) fn emit_with_sidecar(
        mut self,
        buffer_layouts: Option<&HashMap<u32, crate::meta::AirType>>,
        air_data_layout: Option<&crate::layout::AirDataLayout>,
    ) -> Result<(Module, crate::emit_sidecar::EmitSidecar), crate::emit_sidecar::EmissionFailure>
    {
        if let Some(air_data_layout) = air_data_layout {
            self.air_data_layout = Some(air_data_layout.clone());
        }
        let mut this = self.emit_inner()?;
        this.module.sync_id_bound_from_instructions();
        this.remove_unreachable_functions();
        let air_data_layout = this.air_data_layout.clone();
        this.record_air_struct_offsets(buffer_layouts, air_data_layout.as_ref());
        this.emit_sidecar.air_data_layout = air_data_layout;
        if this.used_device_address {
            if let Err(error) = this.lower_bda_null_aggregate_pointers() {
                return Err(this.emission_failure(error));
            }
            if let Err(error) = this.lower_bda_address_operations_module() {
                return Err(this.emission_failure(error));
            }
            this.switch_to_physical_storage_buffer64();
        }
        this.align_physical_memory_accesses();
        Ok((this.module, this.emit_sidecar))
    }

    fn align_physical_memory_accesses(&mut self) {
        fn scalar_alignment(types: &HashMap<Word, &Instruction>, id: Word) -> u32 {
            let Some(inst) = types.get(&id) else {
                return 4;
            };
            match inst.class.opcode {
                Op::TypeInt | Op::TypeFloat => match inst.operands.first() {
                    Some(Operand::LiteralBit32(bits)) => (bits / 8).clamp(1, 8),
                    _ => 4,
                },
                Op::TypeVector | Op::TypeMatrix | Op::TypeArray => match inst.operands.first() {
                    Some(Operand::IdRef(element)) => scalar_alignment(types, *element),
                    _ => 4,
                },
                Op::TypePointer => 8,
                _ => 4,
            }
        }

        let types: HashMap<Word, &Instruction> = self
            .module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id.map(|id| (id, inst)))
            .collect();
        let physical_pointer_alignment: HashMap<Word, u32> = self
            .module
            .types_global_values
            .iter()
            .filter(|inst| inst.class.opcode == Op::TypePointer)
            .filter(|inst| {
                matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(StorageClass::PhysicalStorageBuffer))
                )
            })
            .filter_map(|inst| {
                let id = inst.result_id?;
                let Some(Operand::IdRef(pointee)) = inst.operands.get(1) else {
                    return None;
                };
                Some((id, scalar_alignment(&types, *pointee)))
            })
            .collect();
        if physical_pointer_alignment.is_empty() {
            return;
        }

        let mut pointer_alignment: HashMap<Word, u32> = HashMap::new();
        for function in &self.module.functions {
            for parameter in &function.parameters {
                if let (Some(result), Some(ty)) = (parameter.result_id, parameter.result_type) {
                    if let Some(align) = physical_pointer_alignment.get(&ty) {
                        pointer_alignment.insert(result, *align);
                    }
                }
            }
            for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
                if let (Some(result), Some(ty)) = (instruction.result_id, instruction.result_type) {
                    if let Some(align) = physical_pointer_alignment.get(&ty) {
                        pointer_alignment.insert(result, *align);
                    }
                }
            }
        }

        for instruction in self
            .module
            .functions
            .iter_mut()
            .flat_map(|function| function.blocks.iter_mut())
            .flat_map(|block| block.instructions.iter_mut())
        {
            let operands_before_access = match instruction.class.opcode {
                Op::Load => 1,
                Op::Store => 2,
                _ => continue,
            };
            if instruction.operands.len() != operands_before_access {
                continue;
            }
            let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
                continue;
            };
            let Some(alignment) = pointer_alignment.get(pointer).copied() else {
                continue;
            };
            instruction
                .operands
                .push(Operand::MemoryAccess(MemoryAccess::ALIGNED));
            instruction.operands.push(Operand::LiteralBit32(alignment));
        }
    }

    fn lower_bda_address_operations_module(&mut self) -> Result<(), String> {
        loop {
            let loads_changed = self.lower_bda_integer_pointer_loads_module()?;
            let chains_changed = self.lower_bda_integer_address_chains_module()?;
            if !loads_changed && !chains_changed {
                return Ok(());
            }
        }
    }

    fn lower_bda_integer_pointer_loads_module(&mut self) -> Result<bool, String> {
        let address_ty = self.type_id(&LlType::Int(64))?;
        let physical_address_ptr =
            self.ptr_type_id(StorageClass::PhysicalStorageBuffer, &LlType::Int(64))?;
        let pointer_types = self
            .module
            .types_global_values
            .iter()
            .filter_map(|instruction| {
                (instruction.class.opcode == Op::TypePointer).then_some(instruction.result_id?)
            })
            .collect::<HashSet<_>>();
        let physical_pointer_types = self
            .module
            .types_global_values
            .iter()
            .filter_map(|instruction| {
                (instruction.class.opcode == Op::TypePointer
                    && matches!(
                        instruction.operands.first(),
                        Some(Operand::StorageClass(StorageClass::PhysicalStorageBuffer))
                    ))
                .then_some(instruction.result_id?)
            })
            .collect::<HashSet<_>>();
        let pointer_pointees = self
            .module
            .types_global_values
            .iter()
            .filter_map(|instruction| {
                if instruction.class.opcode != Op::TypePointer {
                    return None;
                }
                let pointee = match instruction.operands.get(1) {
                    Some(Operand::IdRef(id)) => *id,
                    _ => return None,
                };
                Some((instruction.result_id?, pointee))
            })
            .collect::<HashMap<_, _>>();
        let opaque_resource_loads = self
            .emit_sidecar
            .buffer_pointer_field_loads
            .iter()
            .filter(|fact| self.opaque_resource_ids.contains(&fact.id))
            .map(|fact| fact.id)
            .chain(
                self.emit_sidecar
                    .buffer_pointer_dynamic_field_loads
                    .iter()
                    .filter(|fact| self.opaque_resource_ids.contains(&fact.id))
                    .map(|fact| fact.id),
            )
            .collect::<HashSet<_>>();
        let memory_pointer_values = self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                )
            })
            .filter_map(|instruction| match instruction.operands.first() {
                Some(Operand::IdRef(id)) => Some(*id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut value_types = self
            .module
            .types_global_values
            .iter()
            .chain(self.module.functions.iter().flat_map(|function| {
                function.parameters.iter().chain(
                    function
                        .blocks
                        .iter()
                        .flat_map(|block| block.instructions.iter()),
                )
            }))
            .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
            .collect::<HashMap<_, _>>();
        let definitions = self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
            .collect::<HashMap<_, _>>();
        let zero = self.const_uint(0)?;
        let mut candidates = Vec::new();
        for (function_index, function) in self.module.functions.iter().enumerate() {
            for (block_index, block) in function.blocks.iter().enumerate() {
                for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                    if instruction.class.opcode != Op::Load
                        || !instruction
                            .result_type
                            .is_some_and(|ty| pointer_types.contains(&ty))
                        || instruction.result_id.is_some_and(|result| {
                            opaque_resource_loads.contains(&result)
                                && !memory_pointer_values.contains(&result)
                        })
                    {
                        continue;
                    }
                    let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
                        continue;
                    };
                    let pointer_type = value_types.get(pointer).copied();
                    if pointer_type == Some(address_ty)
                        || pointer_type.is_some_and(|ty| physical_pointer_types.contains(&ty))
                    {
                        candidates.push((
                            function_index,
                            block_index,
                            instruction_index,
                            *pointer,
                            pointer_type == Some(address_ty),
                            None,
                            pointer_type.is_some_and(|ty| {
                                physical_pointer_types.contains(&ty)
                                    && pointer_pointees.get(&ty) != Some(&address_ty)
                            }),
                        ));
                    } else if let Some(definition) = definitions.get(pointer) {
                        let linear = match definition.operands.as_slice() {
                            [Operand::IdRef(base), Operand::IdRef(first), Operand::IdRef(index)]
                                if matches!(
                                    definition.class.opcode,
                                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                                ) && *first == zero
                                    && value_types.get(base) == Some(&address_ty) =>
                            {
                                Some((*base, *index))
                            }
                            _ => None,
                        };
                        if let Some((base, index)) = linear {
                            candidates.push((
                                function_index,
                                block_index,
                                instruction_index,
                                base,
                                true,
                                Some(index),
                                false,
                            ));
                        }
                    }
                }
            }
        }
        candidates.reverse();
        let changed = !candidates.is_empty();
        for (
            function_index,
            block_index,
            instruction_index,
            address,
            needs_conversion,
            linear_index,
            needs_reinterpretation,
        ) in candidates
        {
            let physical_base = if needs_conversion || needs_reinterpretation {
                self.fresh()
            } else {
                address
            };
            let physical_pointer = if linear_index.is_some() {
                self.fresh()
            } else {
                physical_base
            };
            let slot_address = if needs_reinterpretation {
                Some(self.fresh())
            } else {
                None
            };
            let block = &mut self.module.functions[function_index].blocks[block_index];
            let result = block.instructions[instruction_index]
                .result_id
                .ok_or_else(|| "native emitter: pointer load has no result id".to_string())?;
            block.instructions[instruction_index] = Self::inst(
                Op::Load,
                Some(address_ty),
                Some(result),
                vec![
                    Operand::IdRef(physical_pointer),
                    Operand::MemoryAccess(MemoryAccess::ALIGNED),
                    Operand::LiteralBit32(8),
                ],
            );
            if needs_conversion {
                block.instructions.insert(
                    instruction_index,
                    Self::inst(
                        Op::ConvertUToPtr,
                        Some(physical_address_ptr),
                        Some(physical_base),
                        vec![Operand::IdRef(address)],
                    ),
                );
            }
            if needs_reinterpretation {
                let slot_address = slot_address.expect("allocated for reinterpretation");
                block.instructions.insert(
                    instruction_index,
                    Self::inst(
                        Op::ConvertPtrToU,
                        Some(address_ty),
                        Some(slot_address),
                        vec![Operand::IdRef(address)],
                    ),
                );
                block.instructions.insert(
                    instruction_index + 1,
                    Self::inst(
                        Op::ConvertUToPtr,
                        Some(physical_address_ptr),
                        Some(physical_base),
                        vec![Operand::IdRef(slot_address)],
                    ),
                );
            }
            if let Some(index) = linear_index {
                let insertion_index = instruction_index
                    + usize::from(needs_conversion)
                    + 2 * usize::from(needs_reinterpretation);
                block.instructions.insert(
                    insertion_index,
                    Self::inst(
                        Op::PtrAccessChain,
                        Some(physical_address_ptr),
                        Some(physical_pointer),
                        vec![Operand::IdRef(physical_base), Operand::IdRef(index)],
                    ),
                );
            }
            value_types.insert(result, address_ty);
            self.bda_address_values.insert(result);
        }
        Ok(changed)
    }

    fn remove_unreachable_functions(&mut self) {
        let function_calls = self
            .module
            .functions
            .iter()
            .filter_map(|function| {
                let function_id = function.def.as_ref()?.result_id?;
                let calls = function
                    .blocks
                    .iter()
                    .flat_map(|block| block.instructions.iter())
                    .filter_map(|instruction| {
                        (instruction.class.opcode == Op::FunctionCall)
                            .then(|| instruction.operands.first())
                            .flatten()
                            .and_then(|operand| match operand {
                                Operand::IdRef(id) => Some(*id),
                                _ => None,
                            })
                    })
                    .collect::<Vec<_>>();
                Some((function_id, calls))
            })
            .collect::<HashMap<_, _>>();
        let mut reachable = self
            .module
            .entry_points
            .iter()
            .filter_map(|entry| match entry.operands.get(1) {
                Some(Operand::IdRef(id)) => Some(*id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut pending = reachable.iter().copied().collect::<Vec<_>>();
        while let Some(function) = pending.pop() {
            if let Some(calls) = function_calls.get(&function) {
                for callee in calls {
                    if reachable.insert(*callee) {
                        pending.push(*callee);
                    }
                }
            }
        }
        if reachable.is_empty() {
            return;
        }
        let mut removed = HashSet::new();
        self.module.functions.retain(|function| {
            let Some(id) = function
                .def
                .as_ref()
                .and_then(|definition| definition.result_id)
            else {
                return true;
            };
            if reachable.contains(&id) {
                true
            } else {
                removed.insert(id);
                false
            }
        });
        self.module.debug_names.retain(|instruction| {
            !matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if removed.contains(id))
        });
    }

    fn lower_bda_integer_address_chains_module(&mut self) -> Result<bool, String> {
        if !self.bda_device_pointers {
            return Ok(false);
        }
        let mut value_types = HashMap::new();
        for instruction in
            self.module
                .types_global_values
                .iter()
                .chain(self.module.functions.iter().flat_map(|function| {
                    function.parameters.iter().chain(
                        function
                            .blocks
                            .iter()
                            .flat_map(|block| block.instructions.iter()),
                    )
                }))
        {
            if let (Some(result), Some(result_type)) =
                (instruction.result_id, instruction.result_type)
            {
                value_types.insert(result, result_type);
            }
        }
        let int64_types = self
            .interner
            .types
            .iter()
            .filter_map(|(ty, id)| matches!(ty, LlType::Int(64)).then_some(*id))
            .chain(
                self.interner
                    .signed_int_types
                    .iter()
                    .filter_map(|(ty, id)| matches!(ty, LlType::Int(64)).then_some(*id)),
            )
            .collect::<HashSet<_>>();
        let pointer_pointees = self
            .interner
            .ptr_types
            .iter()
            .map(|((_, pointee), id)| (*id, pointee.clone()))
            .collect::<HashMap<_, _>>();
        let zero = self.const_uint(0)?;
        let opaque_resource_loads = self
            .emit_sidecar
            .buffer_pointer_field_loads
            .iter()
            .filter(|fact| self.opaque_resource_ids.contains(&fact.id))
            .map(|fact| fact.id)
            .chain(
                self.emit_sidecar
                    .buffer_pointer_dynamic_field_loads
                    .iter()
                    .filter(|fact| self.opaque_resource_ids.contains(&fact.id))
                    .map(|fact| fact.id),
            )
            .collect::<HashSet<_>>();
        let opaque_resource_pointers = self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| {
                (instruction.class.opcode == Op::Load
                    && instruction
                        .result_id
                        .is_some_and(|result| opaque_resource_loads.contains(&result)))
                .then(|| instruction.operands.first())
                .flatten()
                .and_then(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
            })
            .collect::<HashSet<_>>();
        let mut candidates = Vec::new();
        for (function_index, function) in self.module.functions.iter().enumerate() {
            for (block_index, block) in function.blocks.iter().enumerate() {
                for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                    if !matches!(
                        instruction.class.opcode,
                        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                    ) {
                        continue;
                    }
                    if instruction
                        .result_id
                        .is_some_and(|result| opaque_resource_pointers.contains(&result))
                    {
                        continue;
                    }
                    let base_and_index = match instruction.operands.as_slice() {
                        [Operand::IdRef(base), Operand::IdRef(first), Operand::IdRef(index)]
                            if *first == zero =>
                        {
                            Some((*base, *index))
                        }
                        [Operand::IdRef(base), Operand::IdRef(index)] => Some((*base, *index)),
                        _ => None,
                    };
                    let Some((base, index)) = base_and_index else {
                        continue;
                    };
                    if !value_types
                        .get(&base)
                        .is_some_and(|ty| int64_types.contains(ty))
                    {
                        continue;
                    }
                    let Some(pointee) = instruction
                        .result_type
                        .and_then(|ty| pointer_pointees.get(&ty))
                        .cloned()
                    else {
                        continue;
                    };
                    candidates.push((
                        function_index,
                        block_index,
                        instruction_index,
                        base,
                        index,
                        pointee,
                    ));
                }
            }
        }
        candidates.reverse();
        let changed = !candidates.is_empty();
        let mut physical_access_alignments = HashMap::new();
        for (function_index, block_index, instruction_index, base, index, pointee) in candidates {
            let storage_pointee = if matches!(pointee, LlType::Ptr(_)) {
                LlType::Int(64)
            } else {
                pointee
            };
            let physical_type =
                self.ptr_type_id(StorageClass::PhysicalStorageBuffer, &storage_pointee)?;
            let physical_base = self.fresh();
            let alignment = self.device_addr_align(&storage_pointee);
            let block = &mut self.module.functions[function_index].blocks[block_index];
            let result = block.instructions[instruction_index].result_id;
            if let Some(result) = result {
                physical_access_alignments.insert(result, alignment);
            }
            block.instructions[instruction_index] = Self::inst(
                Op::PtrAccessChain,
                Some(physical_type),
                result,
                vec![Operand::IdRef(physical_base), Operand::IdRef(index)],
            );
            block.instructions.insert(
                instruction_index,
                Self::inst(
                    Op::ConvertUToPtr,
                    Some(physical_type),
                    Some(physical_base),
                    vec![Operand::IdRef(base)],
                ),
            );
            self.used_device_address = true;
        }
        for instruction in self
            .module
            .functions
            .iter_mut()
            .flat_map(|function| function.blocks.iter_mut())
            .flat_map(|block| block.instructions.iter_mut())
        {
            let pointer_position = match instruction.class.opcode {
                Op::Load | Op::Store => 0,
                _ => continue,
            };
            let Some(Operand::IdRef(pointer)) = instruction.operands.get(pointer_position) else {
                continue;
            };
            let Some(alignment) = physical_access_alignments.get(pointer).copied() else {
                continue;
            };
            let memory_operand_position = match instruction.class.opcode {
                Op::Load => 1,
                Op::Store => 2,
                _ => unreachable!(),
            };
            if instruction.operands.len() == memory_operand_position {
                instruction
                    .operands
                    .push(Operand::MemoryAccess(MemoryAccess::ALIGNED));
                instruction.operands.push(Operand::LiteralBit32(alignment));
            }
        }
        Ok(changed)
    }

    fn switch_to_physical_storage_buffer64(&mut self) {
        if let Some(mm) = self.module.memory_model.as_mut() {
            if let Some(op) = mm.operands.get_mut(0) {
                *op = Operand::AddressingModel(AddressingModel::PhysicalStorageBuffer64);
            }
        }
        self.require_capability(Capability::PhysicalStorageBufferAddresses);
        self.require_capability(Capability::Int64);
        self.require_extension("SPV_KHR_physical_storage_buffer");
    }

    fn lower_bda_null_aggregate_pointers(&mut self) -> Result<(), String> {
        for instruction in self
            .module
            .functions
            .iter_mut()
            .flat_map(|function| function.blocks.iter_mut())
            .flat_map(|block| block.instructions.iter_mut())
        {
            if instruction.class.opcode != Op::CompositeInsert {
                continue;
            }
            let Some(Operand::IdRef(object)) = instruction.operands.first_mut() else {
                continue;
            };
            if let Some(address) = self.bda_pointer_addresses.get(object).copied() {
                *object = address;
            }
        }
        let null_pointers =
            self.module
                .types_global_values
                .iter()
                .filter_map(|inst| {
                    (inst.class.opcode == Op::ConstantNull)
                        .then_some((inst.result_id?, inst.result_type?))
                })
                .filter(|(_, ty)| {
                    self.module.types_global_values.iter().any(|inst| {
                        inst.class.opcode == Op::TypePointer && inst.result_id == Some(*ty)
                    })
                })
                .collect::<HashMap<_, _>>();
        if null_pointers.is_empty()
            && self.bda_address_values.is_empty()
            && self.bda_pointer_addresses.is_empty()
        {
            return Ok(());
        }

        let type_defs = self
            .module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
            .collect::<HashMap<_, _>>();
        let mut fields_by_null: HashMap<Word, Vec<(Word, usize)>> = HashMap::new();
        let mut unsupported_use = HashSet::new();
        for function in &self.module.functions {
            for inst in function
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
            {
                for (operand_index, operand) in inst.operands.iter().enumerate() {
                    let Operand::IdRef(id) = operand else {
                        continue;
                    };
                    let Some(pointer_ty) = null_pointers.get(id).copied() else {
                        continue;
                    };
                    if inst.class.opcode != Op::CompositeInsert || operand_index != 0 {
                        unsupported_use.insert(*id);
                        continue;
                    }
                    let Some(mut aggregate_ty) = inst.result_type else {
                        unsupported_use.insert(*id);
                        continue;
                    };
                    let mut target = None;
                    for index in inst.operands.iter().skip(2) {
                        let Operand::LiteralBit32(member) = index else {
                            target = None;
                            break;
                        };
                        let Some(def) = type_defs.get(&aggregate_ty) else {
                            target = None;
                            break;
                        };
                        let Some(Operand::IdRef(member_ty)) = def.operands.get(*member as usize)
                        else {
                            target = None;
                            break;
                        };
                        target = Some((aggregate_ty, *member as usize, *member_ty));
                        aggregate_ty = *member_ty;
                    }
                    match target {
                        Some((owner, member, leaf_ty)) if leaf_ty == pointer_ty => {
                            fields_by_null.entry(*id).or_default().push((owner, member));
                        }
                        _ => {
                            unsupported_use.insert(*id);
                        }
                    }
                }
            }
        }
        fields_by_null.retain(|id, fields| !fields.is_empty() && !unsupported_use.contains(id));
        let address_ty = self.type_id(&LlType::Int(64))?;
        let zero = self.const_signed_int(64, 0)?;
        let mut lowered_nulls = fields_by_null.keys().copied().collect::<HashSet<_>>();
        let mut lowered_fields = fields_by_null
            .values()
            .flatten()
            .copied()
            .collect::<HashSet<_>>();
        let result_types = self
            .module
            .types_global_values
            .iter()
            .chain(self.module.functions.iter().flat_map(|function| {
                function.parameters.iter().chain(
                    function
                        .blocks
                        .iter()
                        .flat_map(|block| block.instructions.iter()),
                )
            }))
            .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
            .collect::<HashMap<_, _>>();
        for instruction in self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
        {
            if instruction.class.opcode != Op::CompositeInsert
                || !matches!(instruction.operands.first(), Some(Operand::IdRef(object)) if self.bda_address_values.contains(object))
            {
                continue;
            }
            let Some(mut aggregate_ty) = instruction.result_type else {
                continue;
            };
            let mut target = None;
            for index in instruction.operands.iter().skip(2) {
                let Operand::LiteralBit32(member) = index else {
                    target = None;
                    break;
                };
                let Some(definition) = type_defs.get(&aggregate_ty) else {
                    target = None;
                    break;
                };
                let Some(Operand::IdRef(member_ty)) = definition.operands.get(*member as usize)
                else {
                    target = None;
                    break;
                };
                target = Some((aggregate_ty, *member as usize));
                aggregate_ty = *member_ty;
            }
            if let Some(target) = target {
                lowered_fields.insert(target);
            }
        }
        let mut mixed_pointer_fields = HashSet::new();
        for instruction in self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
        {
            if instruction.class.opcode != Op::CompositeInsert {
                continue;
            }
            let Some(Operand::IdRef(object)) = instruction.operands.first() else {
                continue;
            };
            let Some(mut aggregate_ty) = instruction.result_type else {
                continue;
            };
            let mut target = None;
            for index in instruction.operands.iter().skip(2) {
                let Operand::LiteralBit32(member) = index else {
                    target = None;
                    break;
                };
                let Some(definition) = type_defs.get(&aggregate_ty) else {
                    target = None;
                    break;
                };
                let Some(Operand::IdRef(member_ty)) = definition.operands.get(*member as usize)
                else {
                    target = None;
                    break;
                };
                target = Some((aggregate_ty, *member as usize));
                aggregate_ty = *member_ty;
            }
            if target.is_some_and(|field| lowered_fields.contains(&field))
                && !lowered_nulls.contains(object)
                && result_types.get(object).is_some_and(|object_type| {
                    type_defs.get(object_type).is_some_and(|definition| {
                        matches!(
                            definition.class.opcode,
                            Op::TypePointer
                                | Op::TypeImage
                                | Op::TypeSampler
                                | Op::TypeSampledImage
                                | Op::TypeAccelerationStructureKHR
                        )
                    })
                })
            {
                mixed_pointer_fields.extend(target);
            }
        }
        lowered_fields.retain(|field| !mixed_pointer_fields.contains(field));
        lowered_nulls.retain(|null| {
            fields_by_null
                .get(null)
                .is_some_and(|fields| fields.iter().all(|field| lowered_fields.contains(field)))
        });
        let mut address_values = lowered_nulls.clone();
        address_values.extend(self.bda_address_values.iter().copied());
        loop {
            let mut changed = false;
            for function in &self.module.functions {
                for inst in function
                    .blocks
                    .iter()
                    .flat_map(|block| block.instructions.iter())
                {
                    match inst.class.opcode {
                        Op::CompositeExtract => {
                            let Some(Operand::IdRef(composite)) = inst.operands.first() else {
                                continue;
                            };
                            let Some(mut aggregate_ty) = result_types.get(composite).copied() else {
                                continue;
                            };
                            let mut target = None;
                            for index in inst.operands.iter().skip(1) {
                                let Operand::LiteralBit32(member) = index else {
                                    target = None;
                                    break;
                                };
                                let Some(def) = type_defs.get(&aggregate_ty) else {
                                    target = None;
                                    break;
                                };
                                let Some(Operand::IdRef(member_ty)) =
                                    def.operands.get(*member as usize)
                                else {
                                    target = None;
                                    break;
                                };
                                target = Some((aggregate_ty, *member as usize));
                                aggregate_ty = *member_ty;
                            }
                            if target.is_some_and(|field| lowered_fields.contains(&field)) {
                                if let Some(result) = inst.result_id {
                                    changed |= address_values.insert(result);
                                }
                            }
                        }
                        Op::CompositeInsert => {
                            let Some(Operand::IdRef(object)) = inst.operands.first() else {
                                continue;
                            };
                            if !address_values.contains(object) {
                                continue;
                            }
                            let Some(mut aggregate_ty) = inst.result_type else {
                                continue;
                            };
                            let mut target = None;
                            for index in inst.operands.iter().skip(2) {
                                let Operand::LiteralBit32(member) = index else {
                                    target = None;
                                    break;
                                };
                                let Some(def) = type_defs.get(&aggregate_ty) else {
                                    target = None;
                                    break;
                                };
                                let Some(Operand::IdRef(member_ty)) =
                                    def.operands.get(*member as usize)
                                else {
                                    target = None;
                                    break;
                                };
                                target = Some((aggregate_ty, *member as usize));
                                aggregate_ty = *member_ty;
                            }
                            if let Some(field) = target {
                                changed |= lowered_fields.insert(field);
                            }
                        }
                        Op::CopyObject
                            if inst.operands.iter().any(|operand| {
                                matches!(operand, Operand::IdRef(id) if address_values.contains(id))
                            }) =>
                        {
                            if let Some(result) = inst.result_id {
                                changed |= address_values.insert(result);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut late_mixed_fields = HashSet::new();
        for instruction in self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
        {
            if instruction.class.opcode != Op::CompositeInsert {
                continue;
            }
            let Some(Operand::IdRef(object)) = instruction.operands.first() else {
                continue;
            };
            let Some(mut aggregate_ty) = instruction.result_type else {
                continue;
            };
            let mut target = None;
            for index in instruction.operands.iter().skip(2) {
                let Operand::LiteralBit32(member) = index else {
                    target = None;
                    break;
                };
                let Some(definition) = type_defs.get(&aggregate_ty) else {
                    target = None;
                    break;
                };
                let Some(Operand::IdRef(member_ty)) = definition.operands.get(*member as usize)
                else {
                    target = None;
                    break;
                };
                target = Some((aggregate_ty, *member as usize));
                aggregate_ty = *member_ty;
            }
            if target.is_some_and(|field| lowered_fields.contains(&field))
                && !address_values.contains(object)
                && result_types.get(object).is_some_and(|object_type| {
                    type_defs
                        .get(object_type)
                        .is_some_and(|definition| definition.class.opcode == Op::TypePointer)
                })
            {
                late_mixed_fields.extend(target);
            }
        }
        lowered_fields.retain(|field| !late_mixed_fields.contains(field));
        self.specialize_mixed_bda_aggregate_chains(
            &late_mixed_fields,
            &address_values,
            &type_defs,
            address_ty,
            zero,
        )?;
        lowered_nulls.retain(|null| {
            fields_by_null
                .get(null)
                .is_some_and(|fields| fields.iter().all(|field| lowered_fields.contains(field)))
        });
        for inst in &mut self.module.types_global_values {
            let Some(owner) = inst.result_id else {
                continue;
            };
            for (member, operand) in inst.operands.iter_mut().enumerate() {
                if lowered_fields.contains(&(owner, member)) {
                    *operand = Operand::IdRef(address_ty);
                }
            }
        }
        for function in &mut self.module.functions {
            for inst in function
                .blocks
                .iter_mut()
                .flat_map(|block| block.instructions.iter_mut())
            {
                if inst
                    .result_id
                    .is_some_and(|id| address_values.contains(&id))
                {
                    inst.result_type = Some(address_ty);
                }
                for operand in &mut inst.operands {
                    if matches!(operand, Operand::IdRef(id) if lowered_nulls.contains(id)) {
                        *operand = Operand::IdRef(zero);
                    }
                }
            }
        }
        self.module
            .types_global_values
            .retain(|inst| !inst.result_id.is_some_and(|id| lowered_nulls.contains(&id)));
        Ok(())
    }

    fn specialize_mixed_bda_aggregate_chains(
        &mut self,
        mixed_fields: &HashSet<(Word, usize)>,
        address_values: &HashSet<Word>,
        type_defs: &HashMap<Word, Instruction>,
        address_ty: Word,
        zero: Word,
    ) -> Result<(), String> {
        #[derive(Clone)]
        struct InsertSite {
            function: usize,
            block: usize,
            instruction: usize,
            result: Word,
            object: Word,
            composite: Word,
            result_type: Word,
            path: Vec<u32>,
            target: (Word, usize),
        }

        let mut sites = Vec::new();
        for (function, body) in self.module.functions.iter().enumerate() {
            for (block, basic_block) in body.blocks.iter().enumerate() {
                for (instruction, inst) in basic_block.instructions.iter().enumerate() {
                    if inst.class.opcode != Op::CompositeInsert {
                        continue;
                    }
                    let (
                        Some(result),
                        Some(result_type),
                        Some(Operand::IdRef(object)),
                        Some(Operand::IdRef(composite)),
                    ) = (
                        inst.result_id,
                        inst.result_type,
                        inst.operands.first(),
                        inst.operands.get(1),
                    )
                    else {
                        continue;
                    };
                    let path = inst
                        .operands
                        .iter()
                        .skip(2)
                        .map(|operand| match operand {
                            Operand::LiteralBit32(index) => Some(*index),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>();
                    let Some(path) = path else { continue };
                    let Some(target) = aggregate_path_target(result_type, &path, type_defs) else {
                        continue;
                    };
                    sites.push(InsertSite {
                        function,
                        block,
                        instruction,
                        result,
                        object: *object,
                        composite: *composite,
                        result_type,
                        path,
                        target,
                    });
                }
            }
        }
        let site_by_result = sites
            .iter()
            .enumerate()
            .map(|(index, site)| (site.result, index))
            .collect::<HashMap<_, _>>();
        let module_value_types = self
            .module
            .types_global_values
            .iter()
            .chain(self.module.functions.iter().flat_map(|function| {
                function.parameters.iter().chain(
                    function
                        .blocks
                        .iter()
                        .flat_map(|block| block.instructions.iter()),
                )
            }))
            .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
            .collect::<HashMap<_, _>>();
        let mut handled = HashSet::new();
        let mut type_cache = HashMap::new();
        for seed in sites.iter().filter(|site| {
            address_values.contains(&site.object) && mixed_fields.contains(&site.target)
        }) {
            if handled.contains(&seed.result) {
                continue;
            }
            let mut component = HashSet::from([seed.result]);
            let mut cursor = seed.composite;
            while let Some(index) = site_by_result.get(&cursor).copied() {
                let site = &sites[index];
                component.insert(site.result);
                cursor = site.composite;
            }
            loop {
                let mut changed = false;
                for site in &sites {
                    if component.contains(&site.composite) {
                        changed |= component.insert(site.result);
                    }
                    if component.contains(&site.result)
                        && !type_defs.contains_key(&site.composite)
                        && module_value_types.contains_key(&site.composite)
                    {
                        changed |= component.insert(site.composite);
                    }
                }
                if !changed {
                    break;
                }
            }
            loop {
                let mut changed = false;
                for instruction in self
                    .module
                    .functions
                    .iter()
                    .flat_map(|function| function.blocks.iter())
                    .flat_map(|block| block.instructions.iter())
                {
                    if !matches!(
                        instruction.class.opcode,
                        Op::Phi | Op::Select | Op::CopyObject
                    ) || instruction.result_type != Some(seed.result_type)
                    {
                        continue;
                    }
                    let Some(result) = instruction.result_id else {
                        continue;
                    };
                    let operands = instruction
                        .operands
                        .iter()
                        .filter_map(|operand| match operand {
                            Operand::IdRef(id)
                                if module_value_types.get(id) == Some(&seed.result_type) =>
                            {
                                Some(*id)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if component.contains(&result)
                        || operands.iter().any(|operand| component.contains(operand))
                    {
                        changed |= component.insert(result);
                        for operand in operands {
                            changed |= component.insert(operand);
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            loop {
                let mut changed = false;
                for site in &sites {
                    if component.contains(&site.composite) {
                        changed |= component.insert(site.result);
                    }
                    if component.contains(&site.result)
                        && !type_defs.contains_key(&site.composite)
                        && module_value_types.contains_key(&site.composite)
                    {
                        changed |= component.insert(site.composite);
                    }
                }
                if !changed {
                    break;
                }
            }
            handled.extend(component.iter().copied());
            let component_sites = sites
                .iter()
                .filter(|site| component.contains(&site.result))
                .collect::<Vec<_>>();
            let mut address_paths = component_sites
                .iter()
                .filter(|site| address_values.contains(&site.object))
                .map(|site| site.path.clone())
                .collect::<HashSet<_>>();
            let extracted_paths = self
                .module
                .functions
                .iter()
                .flat_map(|function| function.blocks.iter())
                .flat_map(|block| block.instructions.iter())
                .filter(|instruction| instruction.class.opcode == Op::CompositeExtract)
                .filter_map(|instruction| {
                    let Operand::IdRef(composite) = instruction.operands.first()? else {
                        return None;
                    };
                    (module_value_types.get(composite) == Some(&seed.result_type)).then(|| {
                        instruction
                            .operands
                            .iter()
                            .skip(1)
                            .map(|operand| match operand {
                                Operand::LiteralBit32(index) => Some(*index),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                    })?
                })
                .collect::<HashSet<_>>();
            let mut unobserved_pointer_paths = HashSet::new();
            for pointer_path in aggregate_pointer_leaf_paths(seed.result_type, type_defs) {
                let observed = extracted_paths
                    .iter()
                    .any(|extracted| pointer_path.starts_with(extracted));
                if !observed {
                    address_paths.insert(pointer_path.clone());
                    unobserved_pointer_paths.insert(pointer_path);
                }
            }
            if component_sites.iter().any(|site| {
                address_paths.contains(&site.path)
                    && !unobserved_pointer_paths.contains(&site.path)
                    && !address_values.contains(&site.object)
            }) {
                return Err(
                    "native emitter: one aggregate value chain mixes address and logical-pointer values at the same field"
                        .to_string(),
                );
            }
            let original_type = seed.result_type;
            let specialized_type = self.specialize_bda_aggregate_type(
                original_type,
                &address_paths,
                address_ty,
                type_defs,
                &mut type_cache,
            )?;
            let component_results = component.clone();
            let mut aggregate_values = component_results.clone();
            let mut storage_pointers = HashSet::new();
            let mut storage_pointer_order: Vec<Word> = Vec::new();
            loop {
                let mut changed = false;
                for instruction in self
                    .module
                    .functions
                    .iter()
                    .flat_map(|function| function.blocks.iter())
                    .flat_map(|block| block.instructions.iter())
                {
                    match instruction.class.opcode {
                        Op::Store => {
                            let [Operand::IdRef(pointer), Operand::IdRef(object), ..] =
                                instruction.operands.as_slice()
                            else {
                                continue;
                            };
                            if aggregate_values.contains(object)
                                && storage_pointers.insert(*pointer)
                            {
                                storage_pointer_order.push(*pointer);
                                changed = true;
                            }
                        }
                        Op::Load => {
                            let (Some(result), Some(Operand::IdRef(pointer))) =
                                (instruction.result_id, instruction.operands.first())
                            else {
                                continue;
                            };
                            if aggregate_values.contains(&result)
                                && storage_pointers.insert(*pointer)
                            {
                                storage_pointer_order.push(*pointer);
                                changed = true;
                            }
                            if storage_pointers.contains(pointer) {
                                changed |= aggregate_values.insert(result);
                            }
                        }
                        Op::CopyObject => {
                            let (Some(result), Some(Operand::IdRef(source))) =
                                (instruction.result_id, instruction.operands.first())
                            else {
                                continue;
                            };
                            if aggregate_values.contains(source) {
                                changed |= aggregate_values.insert(result);
                            }
                        }
                        _ => {}
                    }
                }
                if !changed {
                    break;
                }
            }
            let value_types = self
                .module
                .types_global_values
                .iter()
                .chain(self.module.functions.iter().flat_map(|function| {
                    function.parameters.iter().chain(
                        function
                            .blocks
                            .iter()
                            .flat_map(|block| block.instructions.iter()),
                    )
                }))
                .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
                .collect::<HashMap<_, _>>();
            let mut specialized_pointer_types = HashMap::new();
            for pointer in &storage_pointer_order {
                let Some(pointer_type) = value_types.get(pointer).copied() else {
                    continue;
                };
                let Some(definition) = type_defs.get(&pointer_type) else {
                    continue;
                };
                let Some(Operand::StorageClass(storage)) = definition.operands.first() else {
                    continue;
                };
                let specialized_pointer =
                    if let Some(existing) = specialized_pointer_types.get(storage).copied() {
                        existing
                    } else {
                        let id = self.fresh();
                        self.module.types_global_values.push(Self::inst(
                            Op::TypePointer,
                            None,
                            Some(id),
                            vec![
                                Operand::StorageClass(*storage),
                                Operand::IdRef(specialized_type),
                            ],
                        ));
                        specialized_pointer_types.insert(*storage, id);
                        id
                    };
                for instruction in self
                    .module
                    .functions
                    .iter_mut()
                    .flat_map(|function| function.blocks.iter_mut())
                    .flat_map(|block| block.instructions.iter_mut())
                {
                    if instruction.result_id == Some(*pointer) {
                        instruction.result_type = Some(specialized_pointer);
                    }
                }
            }
            for instruction in self
                .module
                .functions
                .iter_mut()
                .flat_map(|function| function.blocks.iter_mut())
                .flat_map(|block| block.instructions.iter_mut())
            {
                match instruction.class.opcode {
                    Op::Load if matches!(instruction.operands.first(), Some(Operand::IdRef(pointer)) if storage_pointers.contains(pointer)) =>
                    {
                        instruction.result_type = Some(specialized_type);
                    }
                    Op::CopyObject
                        if instruction
                            .result_id
                            .is_some_and(|result| aggregate_values.contains(&result)) =>
                    {
                        instruction.result_type = Some(specialized_type);
                    }
                    Op::Phi | Op::Select
                        if instruction
                            .result_id
                            .is_some_and(|result| aggregate_values.contains(&result)) =>
                    {
                        instruction.result_type = Some(specialized_type);
                    }
                    _ => {}
                }
            }
            let mut external_composites = HashMap::new();
            for site in &component_sites {
                if component_results.contains(&site.composite) {
                    continue;
                }
                let Some(definition) = type_defs.get(&site.composite) else {
                    return Err(format!(
                        "native emitter: address aggregate chain has an untyped external root %{} with type {:?}",
                        site.composite,
                        module_value_types.get(&site.composite)
                    ));
                };
                if definition.class.opcode != Op::Undef {
                    return Err(
                        "native emitter: address aggregate chain requires a non-undef external root"
                            .to_string(),
                    );
                }
                let replacement = self.fresh();
                self.module.types_global_values.push(Self::inst(
                    Op::Undef,
                    Some(specialized_type),
                    Some(replacement),
                    vec![],
                ));
                external_composites.insert(site.composite, replacement);
            }
            for site in &component_sites {
                let instruction = &mut self.module.functions[site.function].blocks[site.block]
                    .instructions[site.instruction];
                instruction.result_type = Some(specialized_type);
                if unobserved_pointer_paths.contains(&site.path)
                    && !address_values.contains(&site.object)
                {
                    instruction.operands[0] = Operand::IdRef(zero);
                }
                if let Some(replacement) = external_composites.get(&site.composite).copied() {
                    instruction.operands[1] = Operand::IdRef(replacement);
                }
            }

            let specialized_defs = self
                .module
                .types_global_values
                .iter()
                .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
                .collect::<HashMap<_, _>>();
            for function in &mut self.module.functions {
                for instruction in function
                    .blocks
                    .iter_mut()
                    .flat_map(|block| block.instructions.iter_mut())
                {
                    if instruction.class.opcode != Op::CompositeExtract {
                        continue;
                    }
                    let Some(Operand::IdRef(composite)) = instruction.operands.first() else {
                        continue;
                    };
                    if !aggregate_values.contains(composite) {
                        continue;
                    }
                    let path = instruction
                        .operands
                        .iter()
                        .skip(1)
                        .map(|operand| match operand {
                            Operand::LiteralBit32(index) => Some(*index),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>();
                    let Some(path) = path else { continue };
                    let selected =
                        aggregate_path_leaf_type(specialized_type, &path, &specialized_defs)
                            .ok_or_else(|| {
                                "native emitter: specialized aggregate extract has an invalid path"
                                    .to_string()
                            })?;
                    instruction.result_type = Some(selected);
                }
            }
        }
        Ok(())
    }

    fn specialize_bda_aggregate_type(
        &mut self,
        original: Word,
        paths: &HashSet<Vec<u32>>,
        address_ty: Word,
        type_defs: &HashMap<Word, Instruction>,
        cache: &mut HashMap<(Word, Vec<Vec<u32>>), Word>,
    ) -> Result<Word, String> {
        if paths.iter().any(Vec::is_empty) {
            return Ok(address_ty);
        }
        let mut normalized = paths.iter().cloned().collect::<Vec<_>>();
        normalized.sort();
        let key = (original, normalized.clone());
        if let Some(existing) = cache.get(&key).copied() {
            return Ok(existing);
        }
        let definition = type_defs.get(&original).ok_or_else(|| {
            "native emitter: missing aggregate type for BDA specialization".to_string()
        })?;
        if !matches!(definition.class.opcode, Op::TypeStruct | Op::TypeArray) {
            return Err(
                "native emitter: BDA aggregate specialization reached a non-aggregate type"
                    .to_string(),
            );
        }
        let mut operands = definition.operands.clone();
        for member in 0..operands.len() {
            let child_paths = normalized
                .iter()
                .filter(|path| path.first().copied() == Some(member as u32))
                .map(|path| path[1..].to_vec())
                .collect::<HashSet<_>>();
            if child_paths.is_empty() {
                continue;
            }
            let Some(Operand::IdRef(child)) = operands.get(member).cloned() else {
                return Err(
                    "native emitter: malformed aggregate type during BDA specialization"
                        .to_string(),
                );
            };
            let specialized = self.specialize_bda_aggregate_type(
                child,
                &child_paths,
                address_ty,
                type_defs,
                cache,
            )?;
            operands[member] = Operand::IdRef(specialized);
        }
        let result = self.fresh();
        self.module.types_global_values.push(Self::inst(
            definition.class.opcode,
            None,
            Some(result),
            operands,
        ));
        cache.insert(key, result);
        Ok(result)
    }

    pub(super) fn emit_collecting_storage(
        mut self,
    ) -> Result<Vec<(String, HashMap<String, StorageClass>)>, String> {
        self.capture_storage = true;
        let this = self.emit_inner().map_err(|failure| failure.error)?;
        Ok(this.storage_snapshots)
    }

    pub(super) fn emit_collecting_pointees(
        mut self,
    ) -> Result<Vec<(String, HashMap<String, LlType>)>, String> {
        self.capture_pointees = true;
        let this = self.emit_inner().map_err(|failure| failure.error)?;
        Ok(this.pointee_snapshots)
    }

    pub(super) fn emit_collecting_param_pointees(
        self,
    ) -> Result<HashMap<(String, usize), LlType>, String> {
        let this = self.emit_inner().map_err(|failure| failure.error)?;
        Ok(this.function_param_pointees)
    }

    fn emit_inner(mut self) -> Result<Self, crate::emit_sidecar::EmissionFailure> {
        macro_rules! attempt {
            ($expression:expr) => {
                match $expression {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(self.emission_failure(error));
                    }
                }
            };
        }
        let globals = self.ir.globals.clone();
        let functions = self.ir.functions.clone();
        let declarations = self.ir.declarations.clone();
        self.int_atomic_reinterpret_globals =
            Self::scan_int_atomic_reinterpret_globals(&globals, &functions);
        self.byte_view_reinterpret_globals = attempt!(self.scan_byte_view_reinterpret_globals());
        self.flat_scalar_reinterpret_globals =
            attempt!(self.scan_flat_scalar_reinterpret_globals());
        for global in &globals {
            attempt!(self.emit_global(global));
        }
        for f in &functions {
            let id = self.fresh();
            self.function_ids.insert(f.name.clone(), id);
        }
        for decl in &declarations {
            let id = self.fresh();
            self.function_ids.insert(decl.name.clone(), id);
        }
        for decl in &declarations {
            attempt!(self.emit_declaration(decl));
        }
        attempt!(self.infer_function_param_pointees(&functions));
        self.function_param_nonnull = attempt!(self.infer_function_param_nonnull(&functions));
        self.function_param_nullness = attempt!(self.infer_function_param_nullness(&functions));
        let entry_name = self
            .ir
            .entry_name
            .clone()
            .or_else(|| functions.first().map(|function| function.name.clone()));
        let initializer_names = functions
            .iter()
            .filter(|function| {
                Some(function.name.as_str()) != entry_name.as_deref()
                    && function.is_static_initializer
                    && !self
                        .ir
                        .preinlined_static_initializers
                        .contains(&function.name)
            })
            .map(|function| function.name.clone())
            .collect::<Vec<_>>();
        for f in raw_cursor_callees_after_callers(&self.ir, functions) {
            attempt!(self.emit_function(&f));
        }
        let mut retained_type_capabilities = self
            .ir
            .preinlined_helper_type_capabilities
            .iter()
            .copied()
            .collect::<Vec<_>>();
        retained_type_capabilities.sort_unstable();
        for capability in retained_type_capabilities {
            self.require_capability(match capability {
                LlTypeCapability::Float16 => Capability::Float16,
                LlTypeCapability::Int8 => Capability::Int8,
                LlTypeCapability::Int16 => Capability::Int16,
                LlTypeCapability::Int64 => Capability::Int64,
            });
        }
        self.rewrite_scalar_pointer_arithmetic_access_chains();
        self.remove_dead_empty_functions();
        attempt!(self.inject_static_initializer_calls(entry_name.as_deref(), &initializer_names));
        let mut header = ModuleHeader::new(self.module.id_bound());
        header.set_version(1, 4);
        self.module.header = Some(header);
        let rejected = Box::new(self.emission_rejections());
        let preserved_helpers = self.null_bda_load_helpers.values().copied().collect();
        let module = self.module;
        let emit_sidecar = self.emit_sidecar;
        let inlined = crate::passes::inline_all_emitted_helpers(
            module,
            emit_sidecar,
            self.ir.entry_name.as_deref(),
            &preserved_helpers,
        );
        (self.module, self.emit_sidecar) = match inlined {
            Ok(inlined) => inlined,
            Err(error) => {
                return Err(crate::emit_sidecar::EmissionFailure { error, rejected });
            }
        };
        self.rewrite_private_scalar_offset_access_chains();
        Ok(self)
    }

    fn rewrite_scalar_pointer_arithmetic_access_chains(&mut self) {
        let mut pointer_storage = HashMap::new();
        let aggregate_types = self
            .module
            .types_global_values
            .iter()
            .filter(|inst| {
                matches!(
                    inst.class.opcode,
                    Op::TypeStruct | Op::TypeArray | Op::TypeRuntimeArray | Op::TypeMatrix
                )
            })
            .filter_map(|inst| inst.result_id)
            .collect::<HashSet<_>>();
        let mut pointer_pointees = HashMap::new();
        for inst in &self.module.types_global_values {
            if inst.class.opcode == Op::TypePointer {
                if let (
                    Some(result),
                    Some(Operand::StorageClass(storage)),
                    Some(Operand::IdRef(pointee)),
                ) = (inst.result_id, inst.operands.first(), inst.operands.get(1))
                {
                    pointer_storage.insert(result, *storage);
                    pointer_pointees.insert(result, *pointee);
                }
            }
        }

        let mut id_types = HashMap::new();
        for inst in &self.module.types_global_values {
            if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                id_types.insert(result, result_type);
            }
        }
        for function in &self.module.functions {
            for inst in &function.parameters {
                if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                    id_types.insert(result, result_type);
                }
            }
            for block in &function.blocks {
                if let Some(label) = &block.label {
                    if let (Some(result), Some(result_type)) = (label.result_id, label.result_type)
                    {
                        id_types.insert(result, result_type);
                    }
                }
                for inst in &block.instructions {
                    if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                        id_types.insert(result, result_type);
                    }
                }
            }
        }

        for function in &mut self.module.functions {
            for block in &mut function.blocks {
                for inst in &mut block.instructions {
                    if inst.class.opcode != Op::InBoundsAccessChain {
                        continue;
                    }
                    let Some(result_type) = inst.result_type else {
                        continue;
                    };
                    if !pointer_storage
                        .get(&result_type)
                        .is_some_and(|storage| ptr_access_chain_allowed_storage(*storage))
                    {
                        continue;
                    }
                    if pointer_pointees
                        .get(&result_type)
                        .is_some_and(|pointee| aggregate_types.contains(pointee))
                    {
                        continue;
                    }
                    let Some(Operand::IdRef(base)) = inst.operands.first() else {
                        continue;
                    };
                    if id_types.get(base) != Some(&result_type) {
                        continue;
                    }
                    *inst = Self::inst(
                        Op::PtrAccessChain,
                        inst.result_type,
                        inst.result_id,
                        inst.operands.clone(),
                    );
                }
            }
        }
    }

    fn rewrite_private_scalar_offset_access_chains(&mut self) {
        let mut defs = HashMap::new();
        for inst in &self.module.types_global_values {
            if let Some(id) = inst.result_id {
                defs.insert(id, inst.clone());
            }
        }
        for function in &self.module.functions {
            for param in &function.parameters {
                if let Some(id) = param.result_id {
                    defs.insert(id, param.clone());
                }
            }
            for block in &function.blocks {
                if let Some(label) = &block.label {
                    if let Some(id) = label.result_id {
                        defs.insert(id, label.clone());
                    }
                }
                for inst in &block.instructions {
                    if let Some(id) = inst.result_id {
                        defs.insert(id, inst.clone());
                    }
                }
            }
        }

        let mut pointer_info = HashMap::new();
        for inst in &self.module.types_global_values {
            if inst.class.opcode == Op::TypePointer {
                let (
                    Some(ptr),
                    Some(Operand::StorageClass(storage)),
                    Some(Operand::IdRef(pointee)),
                ) = (inst.result_id, inst.operands.first(), inst.operands.get(1))
                else {
                    continue;
                };
                pointer_info.insert(ptr, (*storage, *pointee));
            }
        }

        let mut insertions = Vec::new();
        let mut next_defs = defs.clone();
        for fi in 0..self.module.functions.len() {
            for bi in 0..self.module.functions[fi].blocks.len() {
                let mut ii = 0usize;
                while ii < self.module.functions[fi].blocks[bi].instructions.len() {
                    let inst = self.module.functions[fi].blocks[bi].instructions[ii].clone();
                    if inst.class.opcode != Op::InBoundsAccessChain || inst.operands.len() != 2 {
                        ii += 1;
                        continue;
                    }
                    let Some(result_type) = inst.result_type else {
                        ii += 1;
                        continue;
                    };
                    let Some((StorageClass::Private, pointee_ty)) =
                        pointer_info.get(&result_type).copied()
                    else {
                        ii += 1;
                        continue;
                    };
                    if !spirv_type_is_scalar(&defs, pointee_ty) {
                        ii += 1;
                        continue;
                    }
                    let Some(Operand::IdRef(base)) = inst.operands.first() else {
                        ii += 1;
                        continue;
                    };
                    let Some(base_def) = next_defs.get(base).cloned() else {
                        ii += 1;
                        continue;
                    };
                    if !matches!(
                        base_def.class.opcode,
                        Op::AccessChain | Op::InBoundsAccessChain
                    ) || base_def.result_type != Some(result_type)
                        || base_def.operands.len() < 2
                    {
                        ii += 1;
                        continue;
                    }
                    let Some(Operand::IdRef(root)) = base_def.operands.first() else {
                        ii += 1;
                        continue;
                    };
                    let Some(root_ptr_ty) = result_type_of_id(&defs, *root) else {
                        ii += 1;
                        continue;
                    };
                    let Some((StorageClass::Private, mut cur_ty)) =
                        pointer_info.get(&root_ptr_ty).copied()
                    else {
                        ii += 1;
                        continue;
                    };
                    for index in &base_def.operands[1..base_def.operands.len() - 1] {
                        let Some(next) = access_chain_step_type(&defs, cur_ty, index) else {
                            cur_ty = 0;
                            break;
                        };
                        cur_ty = next;
                    }
                    if cur_ty == 0
                        || access_chain_step_type(&defs, cur_ty, base_def.operands.last().unwrap())
                            != Some(pointee_ty)
                    {
                        ii += 1;
                        continue;
                    }
                    let Some(Operand::IdRef(offset)) = inst.operands.get(1) else {
                        ii += 1;
                        continue;
                    };
                    let Some(Operand::IdRef(base_last)) = base_def.operands.last() else {
                        ii += 1;
                        continue;
                    };
                    let Some(merged) =
                        self.merge_access_indices(*base_last, *offset, &defs, &mut insertions)
                    else {
                        ii += 1;
                        continue;
                    };
                    let mut operands = base_def.operands.clone();
                    *operands
                        .last_mut()
                        .expect("base access-chain operand length checked") =
                        Operand::IdRef(merged);
                    self.module.functions[fi].blocks[bi].instructions[ii].operands = operands;
                    if let Some(id) = inst.result_id {
                        let mut updated = inst;
                        updated.operands = self.module.functions[fi].blocks[bi].instructions[ii]
                            .operands
                            .clone();
                        next_defs.insert(id, updated);
                    }
                    if !insertions.is_empty() {
                        self.module.functions[fi].blocks[bi]
                            .instructions
                            .splice(ii..ii, insertions.iter().cloned());
                        insertions.clear();
                        ii += 1;
                    }
                    ii += 1;
                }
            }
        }
    }

    fn merge_access_indices(
        &mut self,
        lhs: Word,
        rhs: Word,
        defs: &HashMap<Word, Instruction>,
        insertions: &mut Vec<Instruction>,
    ) -> Option<Word> {
        match (const_int_value(defs, lhs), const_int_value(defs, rhs)) {
            (Some(a), Some(b)) => {
                let ty = result_type_of_id(defs, lhs).or_else(|| result_type_of_id(defs, rhs))?;
                return Some(self.get_or_create_index_const(ty, a.checked_add(b)?));
            }
            (Some(0), _) => return Some(rhs),
            (_, Some(0)) => return Some(lhs),
            _ => {}
        }
        let lhs_ty = result_type_of_id(defs, lhs);
        let rhs_ty = result_type_of_id(defs, rhs);
        let (ty, lhs_id, rhs_id) = match (
            lhs_ty,
            rhs_ty,
            const_int_value(defs, lhs),
            const_int_value(defs, rhs),
        ) {
            (Some(a), Some(b), _, _) if a == b => (a, lhs, rhs),
            (_, Some(ty), Some(value), _) => (ty, self.get_or_create_index_const(ty, value), rhs),
            (Some(ty), _, _, Some(value)) => (ty, lhs, self.get_or_create_index_const(ty, value)),
            _ => return None,
        };
        let id = self.fresh();
        insertions.push(Self::inst(
            Op::IAdd,
            Some(ty),
            Some(id),
            vec![Operand::IdRef(lhs_id), Operand::IdRef(rhs_id)],
        ));
        Some(id)
    }

    fn get_or_create_index_const(&mut self, ty: Word, value: u64) -> Word {
        let operands = index_const_operands(&self.module.types_global_values, ty, value)
            .unwrap_or_else(|| vec![Operand::LiteralBit32(value as u32)]);
        for inst in &self.module.types_global_values {
            if inst.class.opcode == Op::Constant
                && inst.result_type == Some(ty)
                && inst.operands == operands
            {
                return inst.result_id.expect("constant has result id");
            }
        }
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Constant,
            Some(ty),
            Some(id),
            operands,
        ));
        id
    }

    pub(super) fn fresh(&mut self) -> Word {
        self.module.fresh_id()
    }

    pub(super) fn inst(
        op: Op,
        result_type: Option<Word>,
        result_id: Option<Word>,
        operands: Vec<Operand>,
    ) -> Instruction {
        Instruction::new(op, result_type, result_id, operands)
    }
}

fn aggregate_path_target(
    mut aggregate_type: Word,
    path: &[u32],
    definitions: &HashMap<Word, Instruction>,
) -> Option<(Word, usize)> {
    let mut target = None;
    for index in path {
        let definition = definitions.get(&aggregate_type)?;
        let member = match definition.class.opcode {
            Op::TypeStruct => *index as usize,
            Op::TypeArray => 0,
            _ => return None,
        };
        let Operand::IdRef(child) = definition.operands.get(member)? else {
            return None;
        };
        target = Some((aggregate_type, member));
        aggregate_type = *child;
    }
    target
}

fn aggregate_path_leaf_type(
    mut aggregate_type: Word,
    path: &[u32],
    definitions: &HashMap<Word, Instruction>,
) -> Option<Word> {
    for index in path {
        let definition = definitions.get(&aggregate_type)?;
        let member = match definition.class.opcode {
            Op::TypeStruct => *index as usize,
            Op::TypeArray => 0,
            _ => return None,
        };
        let Operand::IdRef(child) = definition.operands.get(member)? else {
            return None;
        };
        aggregate_type = *child;
    }
    Some(aggregate_type)
}

fn aggregate_pointer_leaf_paths(
    aggregate_type: Word,
    definitions: &HashMap<Word, Instruction>,
) -> Vec<Vec<u32>> {
    fn visit(
        ty: Word,
        prefix: &mut Vec<u32>,
        definitions: &HashMap<Word, Instruction>,
        paths: &mut Vec<Vec<u32>>,
    ) {
        let Some(definition) = definitions.get(&ty) else {
            return;
        };
        match definition.class.opcode {
            Op::TypePointer => paths.push(prefix.clone()),
            Op::TypeStruct => {
                for (member, operand) in definition.operands.iter().enumerate() {
                    let Operand::IdRef(child) = operand else {
                        continue;
                    };
                    prefix.push(member as u32);
                    visit(*child, prefix, definitions, paths);
                    prefix.pop();
                }
            }
            Op::TypeArray => {
                if let Some(Operand::IdRef(child)) = definition.operands.first() {
                    prefix.push(0);
                    visit(*child, prefix, definitions, paths);
                    prefix.pop();
                }
            }
            _ => {}
        }
    }

    let mut paths = Vec::new();
    visit(aggregate_type, &mut Vec::new(), definitions, &mut paths);
    paths
}

use crate::spirv_module::ptr_access_chain_allowed_storage;

fn result_type_of_id(defs: &HashMap<Word, Instruction>, id: Word) -> Option<Word> {
    defs.get(&id).and_then(|inst| inst.result_type)
}

fn const_int_value(defs: &HashMap<Word, Instruction>, id: Word) -> Option<u64> {
    let inst = defs.get(&id)?;
    if inst.class.opcode != Op::Constant {
        return None;
    }
    match inst.operands.as_slice() {
        [Operand::LiteralBit32(value)] => Some(u64::from(*value)),
        [Operand::LiteralBit32(lo), Operand::LiteralBit32(hi)] => {
            Some(u64::from(*lo) | (u64::from(*hi) << 32))
        }
        _ => None,
    }
}

fn index_const_operands(types: &[Instruction], ty: Word, value: u64) -> Option<Vec<Operand>> {
    let ty = types
        .iter()
        .find(|inst| inst.result_id == Some(ty) && inst.class.opcode == Op::TypeInt)?;
    let bits = match ty.operands.first()? {
        Operand::LiteralBit32(bits) => *bits,
        _ => return None,
    };
    match bits {
        64 => Some(vec![
            Operand::LiteralBit32(value as u32),
            Operand::LiteralBit32((value >> 32) as u32),
        ]),
        _ => Some(vec![Operand::LiteralBit32(value as u32)]),
    }
}

fn spirv_type_is_scalar(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    defs.get(&ty).is_some_and(|inst| {
        matches!(
            inst.class.opcode,
            Op::TypeBool | Op::TypeFloat | Op::TypeInt
        )
    })
}

fn access_chain_step_type(
    defs: &HashMap<Word, Instruction>,
    composite_ty: Word,
    index: &Operand,
) -> Option<Word> {
    let def = defs.get(&composite_ty)?;
    match def.class.opcode {
        Op::TypeStruct => {
            let Operand::IdRef(index) = index else {
                return None;
            };
            let member = const_int_value(defs, *index)? as usize;
            match def.operands.get(member)? {
                Operand::IdRef(member_ty) => Some(*member_ty),
                _ => None,
            }
        }
        Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
            match def.operands.first()? {
                Operand::IdRef(elem) => Some(*elem),
                _ => None,
            }
        }
        _ => None,
    }
}

fn padding_widened_vector(declared: &LlType, call: &LlType) -> bool {
    match (declared, call) {
        (LlType::Vector(d_elem, d_lanes), LlType::Vector(c_elem, c_lanes)) => {
            types_compatible(d_elem, c_elem) && c_lanes > d_lanes
        }
        _ => false,
    }
}

/// A helper whose pointer parameter is a raw buffer cursor learns the caller's constant byte offset
/// only when the call is emitted (`raw_device_call_arg_id` records it in `raw_call_param_offsets`,
/// and the parameter set-up in `emit_function` reads it). Emitted first, the helper reads that
/// parameter at offset 0. CoreImage's stitched libraries define their helpers before the kernel, so
/// a multi-block helper handed `buffer0 + 80` (the sampler transform of a `coreimage::Sampler`)
/// loaded its matrix from bytes 0..31: `ci_lanczosDownH` / `ci_lanczosDownV` came out empty or
/// smeared, and with them the small graphic icons IconServices shrinks through
/// CILanczosScaleTransform (System Settings' sidebar). Single-block helpers were spared only
/// because they are inlined as text first. Such helpers are now emitted after every function that
/// calls them; the relative order of all other functions is unchanged, and a call cycle falls back
/// to source order.
fn raw_cursor_callees_after_callers(ir: &LlModule, functions: Vec<LlFunction>) -> Vec<LlFunction> {
    let late: HashSet<&str> = ir
        .raw_buffer_params
        .iter()
        .map(|(function, _)| function.as_str())
        .collect();
    if late.is_empty() {
        return functions;
    }
    let count = functions.len();
    let mut callers: Vec<HashSet<usize>> = vec![HashSet::new(); count];
    for (caller, function) in functions.iter().enumerate() {
        for inst in function.carrier_insts() {
            let Some(call) = inst.call().as_deref() else {
                continue;
            };
            if !late.contains(call.callee.as_str()) {
                continue;
            }
            if let Some(callee) = functions.iter().position(|f| f.name == call.callee) {
                if callee != caller {
                    callers[callee].insert(caller);
                }
            }
        }
    }
    if callers
        .iter()
        .enumerate()
        .all(|(callee, set)| set.iter().all(|&caller| caller < callee))
    {
        return functions;
    }
    let mut emitted = vec![false; count];
    let mut order = Vec::with_capacity(count);
    while order.len() < count {
        let next = (0..count)
            .find(|&i| !emitted[i] && callers[i].iter().all(|&caller| emitted[caller]))
            .or_else(|| (0..count).find(|&i| !emitted[i]))
            .expect("an unemitted function remains");
        emitted[next] = true;
        order.push(next);
    }
    let mut slots: Vec<Option<LlFunction>> = functions.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("each function once"))
        .collect()
}
