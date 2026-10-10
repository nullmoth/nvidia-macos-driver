use super::*;
use crate::native::cfg::BodyBlock;
use crate::native::tir::{RetEmit, TirOperand, TirTerminator};

#[cfg(test)]
mod tests;

#[derive(Clone)]
struct OrdinaryLeaf {
    ordinal: usize,
    name: String,
    params: Vec<(String, LlType)>,
    ret: LlType,
    blocks: Vec<BodyBlock>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::native) struct TypedInlineStats {
    pub(in crate::native) splices: usize,
    pub(in crate::native) helper_instances: usize,
}

fn is_call_opcode(opcode: &str) -> bool {
    matches!(opcode, "call" | "tail" | "musttail" | "notail")
}

fn is_residual_intrinsic(name: &str) -> bool {
    name.starts_with("air.")
        || name.starts_with("llvm.fabs.")
        || name.starts_with("llvm.fmuladd.")
        || matches!(
            name,
            "llvm.agx2.f16matmad4x4.v2f16"
                | "llvm.agx2.f32matmad4x4.v2f32"
                | "llvm.agx2.f16matmad8x8.v2f16"
                | "llvm.agx2.f32matmad8x8.v2f32"
        )
        || name.starts_with("llvm.bswap.")
        || name.starts_with("llvm.maxnum.")
        || name.starts_with("llvm.minnum.")
        || name == "llvm.assume"
}

fn is_literal(value: &LlValue) -> bool {
    match value {
        LlValue::Local(_) | LlValue::Global(_) | LlValue::Gep(_) => false,
        LlValue::IntToPtr { source, .. } => is_literal(&source.value),
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            values.iter().all(|value| is_literal(&value.value))
        }
        LlValue::Splat(value) => is_literal(&value.value),
        LlValue::Bool(_)
        | LlValue::Int(_)
        | LlValue::SignedInt(_)
        | LlValue::Hex(_)
        | LlValue::Float(_)
        | LlValue::Float32Bits(_)
        | LlValue::HalfBits(_)
        | LlValue::BFloatBits(_)
        | LlValue::Zero
        | LlValue::Undef => true,
    }
}

fn eligible_ordinary_helper(
    function: &LlFunction,
    bodied_functions: &HashSet<String>,
) -> Option<Vec<BodyBlock>> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| block.typed.as_ref())
        .collect::<Option<Vec<_>>>()?;
    if blocks.len() != 1
        || blocks.iter().any(|block| {
            block.insts.iter().any(|instruction| {
                instruction.opcode == "alloca"
                    || (is_call_opcode(&instruction.opcode)
                        && instruction
                            .call()
                            .as_ref()
                            .is_none_or(|call| bodied_functions.contains(&call.callee)))
            })
        })
    {
        return None;
    }
    let mut returns = 0usize;
    for block in blocks {
        match (&function.ret, &block.terminator, &block.ret) {
            (LlType::Void, TirTerminator::Ret(None), RetEmit::Void) => returns += 1,
            (return_type, TirTerminator::Ret(Some(_)), RetEmit::Value(value))
                if &value.ty == return_type =>
            {
                returns += 1;
            }
            (_, TirTerminator::Ret(_), _) => return None,
            _ => {}
        }
    }
    (returns > 0).then(|| function.blocks.clone())
}

fn reachable_functions(
    module: &LlModule,
    entry_name: &str,
    bodied_functions: &HashSet<String>,
) -> HashSet<String> {
    let mut reachable = HashSet::from([entry_name.to_string()]);
    let mut pending = vec![entry_name.to_string()];
    for function in &module.functions {
        if function.name != entry_name
            && function.is_static_initializer
            && !module
                .preinlined_static_initializers
                .contains(&function.name)
            && reachable.insert(function.name.clone())
        {
            pending.push(function.name.clone());
        }
    }
    while let Some(name) = pending.pop() {
        let Some(function) = module
            .functions
            .iter()
            .find(|function| function.name == name)
        else {
            continue;
        };
        for instruction in function.carrier_insts() {
            let Some(call) = instruction.call().as_ref() else {
                continue;
            };
            if !is_residual_intrinsic(&call.callee)
                && bodied_functions.contains(&call.callee)
                && reachable.insert(call.callee.clone())
            {
                pending.push(call.callee.clone());
            }
        }
    }
    reachable
}

fn collect_type_capabilities(
    ty: &LlType,
    aliases: &HashMap<String, LlType>,
    visiting: &mut HashSet<String>,
    capabilities: &mut HashSet<LlTypeCapability>,
) {
    match ty {
        LlType::Half => {
            capabilities.insert(LlTypeCapability::Float16);
        }
        LlType::BFloat | LlType::Int(16) => {
            capabilities.insert(LlTypeCapability::Int16);
        }
        LlType::Int(8) => {
            capabilities.insert(LlTypeCapability::Int8);
        }
        LlType::Int(64) => {
            capabilities.insert(LlTypeCapability::Int64);
        }
        LlType::Vector(element, _) | LlType::Array(element, _) => {
            collect_type_capabilities(element, aliases, visiting, capabilities);
        }
        LlType::Struct(fields) => {
            for field in fields {
                collect_type_capabilities(field, aliases, visiting, capabilities);
            }
        }
        LlType::Named(name) if visiting.insert(name.clone()) => {
            if let Some(resolved) = aliases.get(name) {
                collect_type_capabilities(resolved, aliases, visiting, capabilities);
            }
            visiting.remove(name);
        }
        LlType::Void
        | LlType::Bool
        | LlType::Float
        | LlType::Ptr(_)
        | LlType::Int(_)
        | LlType::Named(_) => {}
    }
}

fn collect_value_capabilities(
    value: &LlValue,
    aliases: &HashMap<String, LlType>,
    visiting: &mut HashSet<String>,
    capabilities: &mut HashSet<LlTypeCapability>,
) {
    match value {
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                collect_type_capabilities(&value.ty, aliases, visiting, capabilities);
                collect_value_capabilities(&value.value, aliases, visiting, capabilities);
            }
        }
        LlValue::Splat(value) => {
            collect_type_capabilities(&value.ty, aliases, visiting, capabilities);
            collect_value_capabilities(&value.value, aliases, visiting, capabilities);
        }
        LlValue::Gep(gep) => {
            collect_type_capabilities(&gep.source_ty, aliases, visiting, capabilities);
            collect_type_capabilities(&gep.base.ty, aliases, visiting, capabilities);
            collect_value_capabilities(&gep.base.value, aliases, visiting, capabilities);
            for index in &gep.indices {
                collect_type_capabilities(&index.ty, aliases, visiting, capabilities);
                collect_value_capabilities(&index.value, aliases, visiting, capabilities);
            }
        }
        LlValue::IntToPtr {
            source,
            destination,
        } => {
            collect_type_capabilities(&source.ty, aliases, visiting, capabilities);
            collect_value_capabilities(&source.value, aliases, visiting, capabilities);
            collect_type_capabilities(destination, aliases, visiting, capabilities);
        }
        LlValue::Local(_)
        | LlValue::Global(_)
        | LlValue::Bool(_)
        | LlValue::Int(_)
        | LlValue::SignedInt(_)
        | LlValue::Hex(_)
        | LlValue::Float(_)
        | LlValue::Float32Bits(_)
        | LlValue::HalfBits(_)
        | LlValue::BFloatBits(_)
        | LlValue::Zero
        | LlValue::Undef => {}
    }
}

fn function_type_capabilities(
    function: &LlFunction,
    aliases: &HashMap<String, LlType>,
    ptr_pointees: &HashMap<(String, String), LlType>,
    raw_buffer_params: &HashSet<(String, String)>,
) -> HashSet<LlTypeCapability> {
    let mut capabilities = HashSet::new();
    let mut visiting = HashSet::new();
    if matches!(function.ret, LlType::Ptr(_)) {
        capabilities.insert(LlTypeCapability::Int8);
    }
    collect_type_capabilities(&function.ret, aliases, &mut visiting, &mut capabilities);
    for (name, ty) in &function.params {
        if matches!(ty, LlType::Ptr(_)) {
            let key = (function.name.clone(), name.clone());
            if raw_buffer_params.contains(&key) {
                continue;
            }
            if let Some(pointee) = ptr_pointees.get(&key) {
                collect_type_capabilities(pointee, aliases, &mut visiting, &mut capabilities);
            } else {
                capabilities.insert(LlTypeCapability::Int8);
            }
        } else {
            collect_type_capabilities(ty, aliases, &mut visiting, &mut capabilities);
        }
    }
    for block in &function.blocks {
        let Some(typed) = &block.typed else {
            continue;
        };
        if let RetEmit::Value(value) = &typed.ret {
            collect_type_capabilities(&value.ty, aliases, &mut visiting, &mut capabilities);
            collect_value_capabilities(&value.value, aliases, &mut visiting, &mut capabilities);
        }
        for instruction in &typed.insts {
            if let Some(ty) = &instruction.result_ty {
                collect_type_capabilities(ty, aliases, &mut visiting, &mut capabilities);
            }
            for operand in &instruction.operands {
                match operand {
                    TirOperand::Value { ty, .. } => {
                        collect_type_capabilities(ty, aliases, &mut visiting, &mut capabilities);
                    }
                    TirOperand::Const { value, ty } => {
                        collect_type_capabilities(ty, aliases, &mut visiting, &mut capabilities);
                        collect_value_capabilities(
                            value,
                            aliases,
                            &mut visiting,
                            &mut capabilities,
                        );
                    }
                    TirOperand::Unresolved => {}
                }
            }
            if let Some(call) = &instruction.call() {
                collect_type_capabilities(&call.ret, aliases, &mut visiting, &mut capabilities);
                for argument in &call.args {
                    collect_type_capabilities(
                        &argument.ty,
                        aliases,
                        &mut visiting,
                        &mut capabilities,
                    );
                    collect_value_capabilities(
                        &argument.value,
                        aliases,
                        &mut visiting,
                        &mut capabilities,
                    );
                }
            }
            if let Some(gep) = &instruction.gep() {
                collect_type_capabilities(
                    &gep.source_ty,
                    aliases,
                    &mut visiting,
                    &mut capabilities,
                );
                collect_type_capabilities(&gep.base.ty, aliases, &mut visiting, &mut capabilities);
                collect_value_capabilities(
                    &gep.base.value,
                    aliases,
                    &mut visiting,
                    &mut capabilities,
                );
                for index in &gep.indices {
                    collect_type_capabilities(&index.ty, aliases, &mut visiting, &mut capabilities);
                    collect_value_capabilities(
                        &index.value,
                        aliases,
                        &mut visiting,
                        &mut capabilities,
                    );
                }
            }
        }
    }
    capabilities
}

impl LlModule {
    /// Inlines leaf helpers until none is left. A helper that calls other helpers (Geekbench 6's OpenCL compare_intensity
    /// calls the cos/sin/round wrappers) only becomes a leaf once those are spliced into it, and one pass never came
    /// back for it: the call stayed, and a descriptor-backed pointer argument cannot cross a call, so the kernel failed
    /// to translate ("byte cursor cannot cross the call", Feature Matching). Bounded: a call graph is at most this deep
    /// before a round splices nothing.
    pub(in crate::native) fn inline_ordinary_leaf_helpers(&mut self) -> TypedInlineStats {
        const MAX_ROUNDS: usize = 8;
        let mut total = TypedInlineStats::default();
        let mut site = 0usize;
        for _ in 0..MAX_ROUNDS {
            let (stats, next_site) = self.inline_ordinary_leaf_round(site);
            site = next_site;
            total.splices += stats.splices;
            total.helper_instances += stats.helper_instances;
            if stats.splices == 0 {
                break;
            }
        }
        total
    }

    /// One round; `first_site` continues the previous round's numbering so spliced locals never share a name.
    fn inline_ordinary_leaf_round(&mut self, first_site: usize) -> (TypedInlineStats, usize) {
        let Some(entry_name) = self
            .entry_name
            .clone()
            .or_else(|| self.functions.first().map(|function| function.name.clone()))
        else {
            return (TypedInlineStats::default(), first_site);
        };
        let bodied_functions = self
            .functions
            .iter()
            .map(|function| function.name.clone())
            .collect::<HashSet<_>>();
        let reachable = reachable_functions(self, &entry_name, &bodied_functions);
        let helpers = self
            .functions
            .iter()
            .enumerate()
            .filter(|(_, function)| {
                reachable.contains(&function.name)
                    && function.blocks.len() == 1
                    && function.name != entry_name
                    && !function.is_static_initializer
                    && !is_residual_intrinsic(&function.name)
            })
            .filter_map(|(ordinal, function)| {
                eligible_ordinary_helper(function, &bodied_functions).map(|blocks| {
                    (
                        function.name.clone(),
                        OrdinaryLeaf {
                            ordinal,
                            name: function.name.clone(),
                            params: function.params.clone(),
                            ret: function.ret.clone(),
                            blocks,
                        },
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        if helpers.is_empty() {
            return (TypedInlineStats::default(), first_site);
        }
        let source_pointees = self.ptr_pointees.clone();
        let mut source_value_pointees = source_pointees.clone();
        source_value_pointees.extend(self.local_alloca_pointees.clone());
        for function in &self.functions {
            for instruction in function.carrier_insts() {
                if let (Some(result), Some(allocated)) =
                    (&instruction.result, &instruction.alloca_ty())
                {
                    source_value_pointees
                        .insert((function.name.clone(), result.clone()), allocated.clone());
                }
            }
        }
        let source_raw_buffers = self.raw_buffer_params.clone();
        let source_data_buffers = self.metadata_data_buffer_params.clone();
        let mut cloned_pointees = Vec::new();
        let mut cloned_raw_buffers = Vec::new();
        let mut cloned_data_buffers = Vec::new();
        let mut cloned_pointer_loads = Vec::new();
        let mut processed_helpers = HashSet::new();
        let mut stats = TypedInlineStats::default();
        let mut site = first_site;
        for function_index in 0..self.functions.len() {
            let caller_name = self.functions[function_index].name.clone();
            if !reachable.contains(&caller_name) {
                continue;
            }
            let mut block_index = 0usize;
            while block_index < self.functions[function_index].blocks.len() {
                let mut instruction_index = 0usize;
                loop {
                    let plan = self.functions[function_index].blocks[block_index]
                        .typed
                        .as_ref()
                        .and_then(|block| block.insts.get(instruction_index))
                        .and_then(|instruction| {
                            if !is_call_opcode(&instruction.opcode) {
                                return None;
                            }
                            let call = instruction.call().as_ref()?;
                            let helper = helpers.get(&call.callee)?.clone();
                            if call.args.len() != helper.params.len() {
                                return None;
                            }
                            let call_result = match (&helper.ret, &instruction.result, &call.ret) {
                                (LlType::Void, None, LlType::Void) => None,
                                (return_type, Some(result), call_return)
                                    if return_type == call_return =>
                                {
                                    Some(result.clone())
                                }
                                _ => return None,
                            };
                            let arguments = helper
                                .params
                                .iter()
                                .zip(&call.args)
                                .map(|((parameter, expected_type), argument)| {
                                    if &argument.ty == expected_type
                                        && (matches!(argument.value, LlValue::Local(_))
                                            || matches!(argument.value, LlValue::Gep(_))
                                            || is_literal(&argument.value))
                                    {
                                        if let (LlType::Ptr(_), LlValue::Local(argument_name)) =
                                            (expected_type, &argument.value)
                                        {
                                            let helper_pointee = source_pointees
                                                .get(&(helper.name.clone(), parameter.clone()));
                                            let caller_pointee = source_value_pointees
                                                .get(&(caller_name.clone(), argument_name.clone()));
                                            if matches!(
                                                (helper_pointee, caller_pointee),
                                                (Some(helper), Some(caller)) if helper != caller
                                            ) {
                                                return None;
                                            }
                                        }
                                        Some(argument.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Option<Vec<_>>>()?;
                            Some((helper, arguments, call_result))
                        });
                    let Some((helper, arguments, call_result)) = plan else {
                        let Some(block) = self.functions[function_index].blocks[block_index]
                            .typed
                            .as_ref()
                        else {
                            break;
                        };
                        if instruction_index >= block.insts.len() {
                            break;
                        }
                        instruction_index += 1;
                        continue;
                    };

                    let mut local_rename = HashMap::new();
                    let mut parameter_bindings = Vec::with_capacity(arguments.len());
                    for (index, ((parameter, parameter_type), argument)) in
                        helper.params.iter().zip(arguments).enumerate()
                    {
                        // A pointer passed as a plain caller local IS that local: use its name. Through a proxy the
                        // emitter lost what it knows about a kernel buffer's struct layout, so an i64 load across two
                        // i32 fields translated in the entry and failed in a one-block helper ("reinterpret load bit
                        // width mismatch Int(32) vs Int(64)").
                        // Only for a kernel buffer of structs the metadata describes; any other pointer keeps its
                        // proxy (a pointer-chasing buffer relies on it: the loop-carried device address case).
                        if let (LlType::Ptr(_), LlValue::Local(argument_name)) = (parameter_type, &argument.value) {
                            let key = (caller_name.clone(), argument_name.clone());
                            if source_data_buffers.contains(&key)
                                && matches!(
                                    source_value_pointees.get(&key),
                                    Some(LlType::Struct(_)) | Some(LlType::Named(_))
                                )
                            {
                                local_rename.insert(parameter.clone(), argument_name.clone());
                                continue;
                            }
                        }
                        let proxy = format!(
                            "%metal2vulkan.helper.{}.{}.param.{index}",
                            helper.ordinal, site
                        );
                        local_rename.insert(parameter.clone(), proxy.clone());
                        parameter_bindings.push(crate::native::tir::TirInst::inline_parameter(
                            proxy, argument,
                        ));
                    }
                    for block in &helper.blocks {
                        let typed = block
                            .typed
                            .as_ref()
                            .expect("eligible ordinary helper has typed blocks");
                        for instruction in &typed.insts {
                            if let Some(result) = &instruction.result {
                                local_rename.insert(
                                    result.clone(),
                                    format!(
                                        "%metal2vulkan.helper.{}.{}.{}",
                                        helper.ordinal,
                                        site,
                                        result.trim_start_matches('%')
                                    ),
                                );
                            }
                        }
                    }

                    let mut block = helper
                        .blocks
                        .into_iter()
                        .next()
                        .expect("eligible ordinary helper has one block");
                    let typed = block
                        .typed_mut()
                        .expect("eligible ordinary helper has a typed block");
                    typed.rename(&local_rename);
                    cloned_pointer_loads.extend(typed.insts.iter().filter_map(|instruction| {
                        (instruction.opcode == "load"
                            && matches!(instruction.result_ty, Some(LlType::Ptr(_))))
                        .then(|| instruction.result.clone())
                        .flatten()
                    }));
                    typed.insts.splice(0..0, parameter_bindings);
                    for (parameter, _) in &helper.params {
                        let Some(proxy) = local_rename.get(parameter) else {
                            continue;
                        };
                        if source_raw_buffers.contains(&(helper.name.clone(), parameter.clone())) {
                            cloned_raw_buffers.push((caller_name.clone(), proxy.clone()));
                        }
                        if source_data_buffers.contains(&(helper.name.clone(), parameter.clone())) {
                            cloned_data_buffers.push((caller_name.clone(), proxy.clone()));
                        }
                    }

                    let returned = match (&helper.ret, &typed.ret) {
                        (LlType::Void, RetEmit::Void) => None,
                        (_, RetEmit::Value(value)) => Some(value.clone()),
                        _ => {
                            instruction_index += 1;
                            continue;
                        }
                    };
                    let result_substitution = match (call_result, returned) {
                        (None, None) => None,
                        (Some(result), Some(returned)) => Some(HashMap::from([(result, returned)])),
                        _ => {
                            instruction_index += 1;
                            continue;
                        }
                    };

                    for ((function, local), pointee) in &source_pointees {
                        if function == &helper.name {
                            if let Some(renamed) = local_rename.get(local) {
                                cloned_pointees.push((
                                    (caller_name.clone(), renamed.clone()),
                                    pointee.clone(),
                                ));
                            }
                        }
                    }

                    let inserted = typed.insts.len();
                    let replacement = typed.insts.clone();
                    self.functions[function_index].blocks[block_index]
                        .typed_mut()
                        .expect("call site came from a typed block")
                        .insts
                        .splice(instruction_index..=instruction_index, replacement);
                    if let Some(result_substitution) = &result_substitution {
                        for caller_block in &mut self.functions[function_index].blocks {
                            if let Some(typed) = caller_block.typed_mut() {
                                typed.substitute_values(result_substitution);
                            }
                        }
                    }
                    instruction_index += inserted;
                    site += 1;
                    stats.splices += 1;
                    processed_helpers.insert(helper.name);
                }
                block_index += 1;
            }
        }
        for (key, pointee) in cloned_pointees {
            self.ptr_pointees.entry(key).or_insert(pointee);
        }
        self.raw_buffer_params.extend(cloned_raw_buffers);
        self.metadata_data_buffer_params.extend(cloned_data_buffers);
        self.preinlined_helper_pointer_loads
            .extend(cloned_pointer_loads);
        stats.helper_instances = processed_helpers.len();
        let remaining_bodied = self
            .functions
            .iter()
            .map(|function| function.name.clone())
            .collect::<HashSet<_>>();
        let remaining_reachable = reachable_functions(self, &entry_name, &remaining_bodied);
        for function in self.functions.iter().filter(|function| {
            processed_helpers.contains(&function.name)
                && !remaining_reachable.contains(&function.name)
        }) {
            self.preinlined_helper_type_capabilities
                .extend(function_type_capabilities(
                    function,
                    &self.types,
                    &self.ptr_pointees,
                    &self.raw_buffer_params,
                ));
        }
        self.functions
            .retain(|function| remaining_reachable.contains(&function.name));
        (stats, site)
    }
}
