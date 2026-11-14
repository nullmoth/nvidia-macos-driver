use super::*;
use crate::native::cfg::BodyBlock;
use crate::native::tir::{RetEmit, TirInst, TirTerminator};

fn resolved_value(value: &TypedValue, substitutions: &HashMap<String, TypedValue>) -> TypedValue {
    let mut value = value.clone();
    let mut remaining = substitutions.len();
    while remaining > 0 {
        let LlValue::Local(name) = &value.value else {
            break;
        };
        let Some(replacement) = substitutions.get(name) else {
            break;
        };
        value = replacement.clone();
        remaining -= 1;
    }
    value
}

fn constant_int(value: &TypedValue) -> Option<u64> {
    match value.value {
        LlValue::Bool(value) => Some(u64::from(value)),
        LlValue::Int(value) => Some(value),
        LlValue::SignedInt(value) => Some(value as u64),
        LlValue::Zero => Some(0),
        _ => None,
    }
}

fn prune_constant_cfg_edges(blocks: &mut Vec<BodyBlock>) {
    for block in blocks.iter_mut() {
        let Some(typed) = block.typed_mut() else {
            continue;
        };
        let target = match &typed.terminator {
            TirTerminator::BrCond { cond, t, .. } if cond == "true" => Some(t.clone()),
            TirTerminator::BrCond { cond, f, .. } if cond == "false" => Some(f.clone()),
            TirTerminator::Switch {
                selector,
                default,
                cases,
            } if selector.parse::<i128>().is_ok() => Some(
                cases
                    .iter()
                    .find_map(|(value, label)| (value == selector).then(|| label.clone()))
                    .unwrap_or_else(|| default.clone()),
            ),
            _ => None,
        };
        if let Some(target) = target {
            typed.set_unconditional_branch(&target);
        }
    }

    let Some(cfg) = crate::native::cfg::graph::Cfg::from_blocks(blocks) else {
        return;
    };
    let reachable = cfg.reachable_from(&cfg.entry);
    for block in blocks
        .iter_mut()
        .filter(|block| reachable.contains(&block.name))
    {
        let predecessors = cfg
            .predecessors
            .get(&block.name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|predecessor| reachable.contains(predecessor))
            .collect::<HashSet<_>>();
        if let Some(typed) = block.typed_mut() {
            typed.rebuild_phi_incomings(|predecessor| predecessors.contains(predecessor));
        }
    }
    blocks.retain(|block| reachable.contains(&block.name));
}

/// Rounds of constant-argument propagation. One round exposes callees one call level deeper
/// than the last, so this is a call-depth bound; shader helper chains are far shallower.
const CONSTANT_ARGUMENT_ROUNDS: usize = 8;

/// Passes over one function body. Definitions normally precede uses in block order; the extra
/// passes cover a block that is listed before the block that dominates it.
const FUNCTION_FOLD_PASSES: usize = 4;

/// Known integer fields of a first-class aggregate, sorted by field index.
type AggregateFields = Vec<(u32, TypedValue)>;

/// What every live call site agrees one argument is.
#[derive(Clone, Debug, PartialEq)]
enum ArgumentFact {
    Scalar(TypedValue),
    Fields(AggregateFields),
}

fn int_width(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Bool => Some(1),
        LlType::Int(width) if (1..=64).contains(width) => Some(*width),
        _ => None,
    }
}

fn width_mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

fn sign_extend(bits: u64, width: u32) -> i64 {
    let shift = 64 - width;
    ((bits << shift) as i64) >> shift
}

/// Width and zero-extended bits of an integer constant; `None` for anything else.
fn int_bits(value: &TypedValue) -> Option<(u32, u64)> {
    let width = int_width(&value.ty)?;
    Some((width, constant_int(value)? & width_mask(width)))
}

/// One spelling per constant, so equal constants compare equal. A one-bit value is spelled as a
/// boolean because that is the form the branch pruning recognises.
fn int_constant(ty: &LlType, bits: u64) -> Option<TypedValue> {
    let width = int_width(ty)?;
    let bits = bits & width_mask(width);
    Some(TypedValue {
        ty: ty.clone(),
        value: if width == 1 {
            LlValue::Bool(bits != 0)
        } else {
            LlValue::Int(bits)
        },
    })
}

fn fold_int_binary(opcode: &str, lhs: &TypedValue, rhs: &TypedValue) -> Option<TypedValue> {
    let (width, a) = int_bits(lhs)?;
    let (rhs_width, b) = int_bits(rhs)?;
    if width != rhs_width {
        return None;
    }
    // A shift by the width or more is poison in LLVM; leave it for the emitter.
    let in_range = b < u64::from(width);
    let bits = match opcode {
        "and" => a & b,
        "or" => a | b,
        "xor" => a ^ b,
        "add" => a.wrapping_add(b),
        "sub" => a.wrapping_sub(b),
        "mul" => a.wrapping_mul(b),
        "shl" if in_range => a << b,
        "lshr" if in_range => a >> b,
        "ashr" if in_range => (sign_extend(a, width) >> b) as u64,
        _ => return None,
    };
    int_constant(&lhs.ty, bits)
}

fn fold_int_cast(opcode: &str, source: &TypedValue, destination: &LlType) -> Option<TypedValue> {
    let (width, bits) = int_bits(source)?;
    let destination_width = int_width(destination)?;
    let bits = match opcode {
        "trunc" if destination_width <= width => bits,
        "zext" if destination_width >= width => bits,
        "sext" if destination_width >= width => sign_extend(bits, width) as u64,
        _ => return None,
    };
    int_constant(destination, bits)
}

fn fold_int_compare(predicate: &str, lhs: &TypedValue, rhs: &TypedValue) -> Option<bool> {
    let (width, a) = int_bits(lhs)?;
    let (rhs_width, b) = int_bits(rhs)?;
    if width != rhs_width {
        return None;
    }
    let (signed_a, signed_b) = (sign_extend(a, width), sign_extend(b, width));
    Some(match predicate {
        "eq" => a == b,
        "ne" => a != b,
        "ult" => a < b,
        "ule" => a <= b,
        "ugt" => a > b,
        "uge" => a >= b,
        "slt" => signed_a < signed_b,
        "sle" => signed_a <= signed_b,
        "sgt" => signed_a > signed_b,
        "sge" => signed_a >= signed_b,
        _ => return None,
    })
}

fn operand_value(
    inst: &TirInst,
    index: usize,
    substitutions: &HashMap<String, TypedValue>,
) -> Option<TypedValue> {
    inst.operands
        .get(index)?
        .as_typed_value()
        .map(|value| resolved_value(&value, substitutions))
}

fn is_call_opcode(opcode: &str) -> bool {
    matches!(opcode, "call" | "tail" | "musttail" | "notail")
}

fn symbol_key(name: &str) -> &str {
    name.trim_start_matches('@').trim_matches('"')
}

fn collect_global_references(value: &LlValue, referenced: &mut HashSet<String>) {
    match value {
        LlValue::Global(name) => {
            referenced.insert(symbol_key(name).to_string());
        }
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                collect_global_references(&value.value, referenced);
            }
        }
        LlValue::Splat(value) => collect_global_references(&value.value, referenced),
        LlValue::Gep(gep) => {
            collect_global_references(&gep.base.value, referenced);
            for index in &gep.indices {
                collect_global_references(&index.value, referenced);
            }
        }
        LlValue::IntToPtr { source, .. } => collect_global_references(&source.value, referenced),
        LlValue::Local(_)
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

fn argument_fact(
    argument: &TypedValue,
    aggregates: Option<&HashMap<String, AggregateFields>>,
) -> Option<ArgumentFact> {
    if let LlValue::Local(name) = &argument.value {
        return aggregates?
            .get(name)
            .filter(|fields| !fields.is_empty())
            .cloned()
            .map(ArgumentFact::Fields);
    }
    let (_, bits) = int_bits(argument)?;
    int_constant(&argument.ty, bits).map(ArgumentFact::Scalar)
}

/// Keeps only what this call site and every earlier one agree on.
fn meet_argument_facts(known: &mut [Option<ArgumentFact>], site: Vec<Option<ArgumentFact>>) {
    if known.len() != site.len() {
        known.iter_mut().for_each(|fact| *fact = None);
        return;
    }
    for (known, site) in known.iter_mut().zip(site) {
        *known = match (known.take(), site) {
            (Some(ArgumentFact::Scalar(a)), Some(ArgumentFact::Scalar(b))) if a == b => {
                Some(ArgumentFact::Scalar(a))
            }
            (Some(ArgumentFact::Fields(a)), Some(ArgumentFact::Fields(b))) => {
                let common = a
                    .into_iter()
                    .filter(|field| b.contains(field))
                    .collect::<Vec<_>>();
                (!common.is_empty()).then_some(ArgumentFact::Fields(common))
            }
            _ => None,
        };
    }
}

/// Folds the integer constants one function body can prove, given what is known about its
/// parameters, removes the folded instructions and prunes the branches they decide.
///
/// Returns whether an instruction was folded and the aggregates whose integer fields are known.
/// Aggregates are tracked but never substituted: they are only read back by `extractvalue` and
/// handed to callees as facts.
fn fold_function_constants(
    function: &mut LlFunction,
    globals: &HashMap<String, meta::StaticIntValue>,
    seeds: Option<&Vec<Option<ArgumentFact>>>,
) -> (bool, HashMap<String, AggregateFields>) {
    let mut substitutions = HashMap::<String, TypedValue>::default();
    let mut aggregates = HashMap::<String, AggregateFields>::default();
    if let Some(seeds) = seeds {
        for ((name, ty), seed) in function.params.iter().zip(seeds) {
            match seed {
                Some(ArgumentFact::Scalar(value)) if value.ty == *ty => {
                    substitutions.insert(name.clone(), value.clone());
                }
                Some(ArgumentFact::Fields(fields)) => {
                    aggregates.insert(name.clone(), fields.clone());
                }
                _ => {}
            }
        }
    }
    for _ in 0..FUNCTION_FOLD_PASSES {
        let before = substitutions.len() + aggregates.len();
        for block in &function.blocks {
            let Some(typed) = block.typed.as_ref() else {
                continue;
            };
            for inst in &typed.insts {
                let Some(result) = inst.result.as_ref() else {
                    continue;
                };
                if substitutions.contains_key(result) {
                    continue;
                }
                let opcode = inst.opcode.as_str();
                if opcode == "insertvalue" {
                    if let (Some([index]), Some(base), Some(element)) = (
                        inst.aggregate_indices().as_deref(),
                        operand_value(inst, 0, &substitutions),
                        operand_value(inst, 1, &substitutions),
                    ) {
                        let mut fields = match &base.value {
                            LlValue::Local(name) => {
                                aggregates.get(name).cloned().unwrap_or_default()
                            }
                            _ => Vec::new(),
                        };
                        fields.retain(|(field, _)| field != index);
                        if let Some(value) = int_bits(&element)
                            .and_then(|(_, bits)| int_constant(&element.ty, bits))
                        {
                            fields.push((*index, value));
                        }
                        fields.sort_by_key(|(field, _)| *field);
                        if !fields.is_empty() {
                            aggregates.insert(result.clone(), fields);
                        }
                    }
                    continue;
                }
                let folded: Option<TypedValue> = (|| {
                    if opcode == "load" {
                        inst.load().as_ref().and_then(|load| {
                            let LlValue::Global(global) = &load.ptr.value else {
                                return None;
                            };
                            let result_ty = inst.result_ty.clone()?;
                            let value = globals.get(global)?;
                            match value {
                                meta::StaticIntValue::Scalar(value) => Some(TypedValue {
                                    ty: result_ty,
                                    value: LlValue::Int(u64::from(*value)),
                                }),
                                meta::StaticIntValue::Vector(values) => {
                                    let LlType::Vector(element, lanes) = &result_ty else {
                                        return None;
                                    };
                                    if values.len() != *lanes as usize {
                                        return None;
                                    }
                                    let element = (**element).clone();
                                    let lanes = values
                                        .iter()
                                        .map(|value| TypedValue {
                                            ty: element.clone(),
                                            value: LlValue::Int(u64::from(*value)),
                                        })
                                        .collect();
                                    Some(TypedValue {
                                        ty: result_ty,
                                        value: LlValue::Vector(lanes),
                                    })
                                }
                            }
                        })
                    } else if opcode == "extractelement" {
                        let mut operands = inst.operands.iter().filter_map(|operand| {
                            operand
                                .as_typed_value()
                                .map(|value| resolved_value(&value, &substitutions))
                        });
                        let vector = operands.next()?;
                        let index = constant_int(&operands.next()?)? as usize;
                        let LlValue::Vector(values) = vector.value else {
                            return None;
                        };
                        values.get(index).cloned()
                    } else if opcode == "extractvalue" {
                        let [index] = inst.aggregate_indices().as_deref()? else {
                            return None;
                        };
                        let aggregate = operand_value(inst, 0, &substitutions)?;
                        let LlValue::Local(name) = &aggregate.value else {
                            return None;
                        };
                        let (_, value) = aggregates
                            .get(name)?
                            .iter()
                            .find(|(field, _)| field == index)?;
                        inst.result_ty
                            .as_ref()
                            .is_none_or(|ty| *ty == value.ty)
                            .then(|| value.clone())
                    } else if matches!(
                        opcode,
                        "and" | "or" | "xor" | "add" | "sub" | "mul" | "shl" | "lshr" | "ashr"
                    ) {
                        fold_int_binary(
                            opcode,
                            &operand_value(inst, 0, &substitutions)?,
                            &operand_value(inst, 1, &substitutions)?,
                        )
                    } else if matches!(opcode, "trunc" | "zext" | "sext") {
                        fold_int_cast(
                            opcode,
                            &operand_value(inst, 0, &substitutions)?,
                            inst.result_ty.as_ref()?,
                        )
                    } else if opcode == "icmp" {
                        let mut operands = inst.operands.iter().filter_map(|operand| {
                            operand
                                .as_typed_value()
                                .map(|value| resolved_value(&value, &substitutions))
                        });
                        let lhs_value = operands.next()?;
                        let rhs_value = operands.next()?;
                        let predicate = inst.cmp_predicate().as_deref();
                        if let Some(result) = predicate
                            .and_then(|predicate| fold_int_compare(predicate, &lhs_value, &rhs_value))
                        {
                            return Some(TypedValue {
                                ty: LlType::Bool,
                                value: LlValue::Bool(result),
                            });
                        }
                        let lhs = constant_int(&lhs_value);
                        let rhs = constant_int(&rhs_value);
                        match (predicate, lhs, rhs) {
                            (Some("eq"), Some(lhs), Some(rhs)) => Some(TypedValue {
                                ty: LlType::Bool,
                                value: LlValue::Bool(lhs == rhs),
                            }),
                            (Some("ne"), Some(lhs), Some(rhs)) => Some(TypedValue {
                                ty: LlType::Bool,
                                value: LlValue::Bool(lhs != rhs),
                            }),
                            (Some("ult"), _, Some(0)) => Some(TypedValue {
                                ty: LlType::Bool,
                                value: LlValue::Bool(false),
                            }),
                            (Some("uge"), _, Some(0)) => Some(TypedValue {
                                ty: LlType::Bool,
                                value: LlValue::Bool(true),
                            }),
                            _ => None,
                        }
                    } else if opcode == "select" {
                        let condition = inst
                            .operands
                            .first()
                            .and_then(|operand| operand.as_typed_value())
                            .map(|value| resolved_value(&value, &substitutions));
                        let take_true = condition.as_ref().and_then(constant_int)? != 0;
                        let (true_value, false_value) = inst.select_arms().as_deref()?;
                        Some(resolved_value(
                            if take_true { true_value } else { false_value },
                            &substitutions,
                        ))
                    } else {
                        None
                    }
                })();
                if let Some(value) = folded {
                    substitutions.insert(result.clone(), value);
                }
            }
        }
        if substitutions.len() + aggregates.len() == before {
            break;
        }
    }
    if substitutions.is_empty() {
        return (false, aggregates);
    }
    let mut removed = 0usize;
    for block in &mut function.blocks {
        let Some(typed) = block.typed_mut() else {
            continue;
        };
        let before = typed.insts.len();
        typed.insts.retain(|inst| {
            inst.result
                .as_ref()
                .is_none_or(|result| !substitutions.contains_key(result))
        });
        removed += before - typed.insts.len();
        typed.substitute_values(&substitutions);
    }
    prune_constant_cfg_edges(&mut function.blocks);
    (removed > 0, aggregates)
}

struct StaticInitializer {
    ordinal: usize,
    name: String,
    blocks: Vec<BodyBlock>,
}

fn terminator_targets(term: &TirTerminator, target: &str) -> bool {
    match term {
        TirTerminator::Br(label) => label == target,
        TirTerminator::BrCond { t, f, .. } => t == target || f == target,
        TirTerminator::Switch { default, cases, .. } => {
            default == target || cases.iter().any(|(_, label)| label == target)
        }
        TirTerminator::Ret(_) | TirTerminator::Unreachable => false,
    }
}

fn eligible_static_initializer(function: &LlFunction) -> Option<Vec<BodyBlock>> {
    if !function.params.is_empty() {
        return None;
    }
    let carriers = function
        .blocks
        .iter()
        .map(|block| block.typed.as_ref())
        .collect::<Option<Vec<_>>>()?;
    if carriers.iter().any(|block| {
        block.insts.iter().any(|instruction| {
            instruction.opcode == "alloca"
                || (matches!(
                    instruction.opcode.as_str(),
                    "call" | "tail" | "musttail" | "notail"
                ) && instruction.call().is_none())
        })
    }) {
        return None;
    }
    let returns = carriers
        .iter()
        .filter(|block| {
            matches!(block.terminator, TirTerminator::Ret(None))
                && matches!(block.ret, RetEmit::Void)
        })
        .count();
    if returns != 1
        || carriers.iter().any(|block| {
            matches!(
                block.terminator,
                TirTerminator::Ret(Some(_)) | TirTerminator::Unreachable
            )
        })
    {
        return None;
    }
    if carriers.len() > 1 {
        let entry = &carriers[0].label;
        if carriers
            .iter()
            .any(|block| terminator_targets(&block.terminator, entry))
        {
            return None;
        }
    }
    Some(function.blocks.clone())
}

impl LlModule {
    pub(in crate::native) fn prune_unreachable_function_bodies(&mut self) {
        let before = self.functions.len();
        let bodied = self
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<HashSet<_>>();
        let Some(entry) = self.entry_name.as_deref() else {
            return;
        };
        let calls = self
            .functions
            .iter()
            .map(|function| {
                let callees = function
                    .blocks
                    .iter()
                    .filter_map(|block| block.typed.as_ref())
                    .flat_map(|block| block.insts.iter())
                    .filter_map(|instruction| instruction.call().as_deref())
                    .filter(|call| bodied.contains(call.callee.as_str()))
                    .map(|call| call.callee.clone())
                    .collect::<Vec<_>>();
                (function.name.clone(), callees)
            })
            .collect::<HashMap<_, _>>();
        let mut reachable = self
            .functions
            .iter()
            .filter(|function| function.name == entry || function.is_static_initializer)
            .map(|function| function.name.clone())
            .collect::<HashSet<_>>();
        let mut pending = reachable.iter().cloned().collect::<Vec<_>>();
        while let Some(function) = pending.pop() {
            for callee in calls.get(&function).into_iter().flatten() {
                if reachable.insert(callee.clone()) {
                    pending.push(callee.clone());
                }
            }
        }
        self.functions
            .retain(|function| reachable.contains(&function.name));
        if crate::env_vars::retry_debug() {
            eprintln!(
                "[retry-debug] native IR: reachable function pruning {before}->{}",
                self.functions.len()
            );
        }
    }

    /// Folds what the function constants decide, the way the vendor's optimizer does after
    /// specialization: integer arithmetic on constants inside each function, and constants that
    /// every live call site passes to an internal function. A shader that tests a state word in
    /// a helper (`if (state.word & flag) hook(...)`) only loses the dead call once the word has
    /// travelled from the entry point into that helper.
    pub(in crate::native) fn fold_static_initializer_constants(&mut self) {
        if self.static_init_globals.is_empty() {
            return;
        }
        let mut seeds = HashMap::<String, Vec<Option<ArgumentFact>>>::default();
        for _ in 0..CONSTANT_ARGUMENT_ROUNDS {
            let mut changed = false;
            let mut aggregates = HashMap::<String, HashMap<String, AggregateFields>>::default();
            for function in &mut self.functions {
                let (folded, known) = fold_function_constants(
                    function,
                    &self.static_init_globals,
                    seeds.get(&function.name),
                );
                changed |= folded;
                aggregates.insert(function.name.clone(), known);
            }
            // Call sites in functions nothing reaches any more must not hold a fact back.
            self.prune_unreachable_function_bodies();
            let next = self.constant_argument_facts(&aggregates);
            if !changed && next == seeds {
                break;
            }
            seeds = next;
        }
    }

    /// For each internal function, the arguments on which all of its call sites agree.
    ///
    /// Sound only when every caller is visible, so a function is skipped unless it has internal
    /// linkage, is not an entry point or static initializer, and is never used as a value. If
    /// any instruction cannot be inspected the answer is "nothing known".
    fn constant_argument_facts(
        &self,
        aggregates: &HashMap<String, HashMap<String, AggregateFields>>,
    ) -> HashMap<String, Vec<Option<ArgumentFact>>> {
        let unknown = |reason: &str| {
            if crate::env_vars::retry_debug() {
                eprintln!("[retry-debug] native IR: constant arguments not propagated: {reason}");
            }
            HashMap::default()
        };
        let mut facts = HashMap::<String, Vec<Option<ArgumentFact>>>::default();
        let mut referenced = HashSet::<String>::default();
        for function in &self.functions {
            let known = aggregates.get(&function.name);
            for block in &function.blocks {
                let Some(typed) = block.typed.as_ref() else {
                    return unknown("untyped block");
                };
                for inst in &typed.insts {
                    if is_call_opcode(inst.opcode.as_str()) {
                        let Some(call) = inst.call().as_deref() else {
                            return unknown("unparsed call");
                        };
                        for argument in &call.args {
                            collect_global_references(&argument.value, &mut referenced);
                        }
                        let site = call
                            .args
                            .iter()
                            .map(|argument| argument_fact(argument, known))
                            .collect::<Vec<_>>();
                        match facts.get_mut(&call.callee) {
                            Some(agreed) => meet_argument_facts(agreed, site),
                            None => {
                                facts.insert(call.callee.clone(), site);
                            }
                        }
                        continue;
                    }
                    for operand in &inst.operands {
                        let Some(value) = operand.as_typed_value() else {
                            return unknown("unresolved operand");
                        };
                        collect_global_references(&value.value, &mut referenced);
                    }
                }
                if let RetEmit::Value(value) = &typed.ret {
                    collect_global_references(&value.value, &mut referenced);
                }
            }
        }
        for global in &self.globals {
            if let Some(initializer) = &global.initializer {
                collect_global_references(&initializer.value, &mut referenced);
            }
        }
        let entry = self.entry_name.as_deref();
        facts.retain(|callee, arguments| {
            arguments.iter().any(Option::is_some)
                && Some(callee.as_str()) != entry
                && !self.entry_functions.contains(callee)
                && !referenced.contains(symbol_key(callee))
                && self.functions.iter().any(|function| {
                    function.name == *callee
                        && function.is_internal
                        && !function.is_static_initializer
                        && function.params.len() == arguments.len()
                })
        });
        facts
    }

    pub(in crate::native) fn inline_simple_static_initializers(&mut self) {
        let Some(entry_name) = self
            .entry_name
            .clone()
            .or_else(|| self.functions.first().map(|function| function.name.clone()))
        else {
            return;
        };
        let Some(entry_index) = self
            .functions
            .iter()
            .position(|function| function.name == entry_name)
        else {
            return;
        };
        if self.functions[entry_index]
            .blocks
            .first()
            .and_then(|block| block.typed.as_ref())
            .is_none()
        {
            return;
        }

        let initializers = self
            .functions
            .iter()
            .enumerate()
            .filter(|(_, function)| function.name != entry_name && function.is_static_initializer)
            .map(|(ordinal, function)| {
                (
                    ordinal,
                    function.name.clone(),
                    (function.blocks.len() == 1)
                        .then(|| eligible_static_initializer(function))
                        .flatten(),
                )
            })
            .collect::<Vec<_>>();
        let suffix_start = initializers
            .iter()
            .rposition(|(_, _, blocks)| blocks.is_none())
            .map_or(0, |index| index + 1);
        let initializers = initializers
            .into_iter()
            .skip(suffix_start)
            .filter_map(|(ordinal, name, blocks)| {
                blocks.map(|blocks| StaticInitializer {
                    ordinal,
                    name,
                    blocks,
                })
            })
            .collect::<Vec<_>>();
        if initializers.is_empty() {
            return;
        }

        let source_pointees = self.ptr_pointees.clone();
        let mut pointees = Vec::new();
        let mut preinlined = Vec::new();
        let entry = &mut self.functions[entry_index];
        let mut cursor = 0;
        for StaticInitializer {
            ordinal,
            name,
            mut blocks,
        } in initializers
        {
            let mut rename = blocks
                .iter()
                .filter_map(|block| block.typed.as_ref())
                .flat_map(|block| block.insts.iter())
                .filter_map(|instruction| instruction.result.as_ref())
                .map(|result| {
                    (
                        result.clone(),
                        format!(
                            "%metal2vulkan.static_init.{ordinal}.{}",
                            result.trim_start_matches('%')
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            for block in &blocks {
                rename.insert(
                    block.name.clone(),
                    format!(
                        "%metal2vulkan.static_init.{ordinal}.block.{}",
                        block.name.trim_start_matches('%')
                    ),
                );
            }
            for block in &mut blocks {
                let typed = block
                    .typed_mut()
                    .expect("eligible static initializer has typed blocks");
                typed.rename(&rename);
                let label = typed.label.clone();
                block.name = label;
            }
            for ((function, local), pointee) in &source_pointees {
                if function == &name {
                    if let Some(renamed) = rename.get(local) {
                        pointees.push(((entry_name.clone(), renamed.clone()), pointee.clone()));
                    }
                }
            }
            preinlined.push(name);

            let block = blocks[0]
                .typed_mut()
                .expect("eligible static initializer has a typed block");
            let inserted = block.insts.len();
            entry.blocks[0]
                .typed_mut()
                .expect("entry carrier checked before static-initializer mutation")
                .insts
                .splice(cursor..cursor, block.insts.iter().cloned());
            block.insts.clear();
            cursor += inserted;
        }

        self.ptr_pointees.extend(pointees);
        self.preinlined_static_initializers.extend(preinlined);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(width: u32, value: LlValue) -> TypedValue {
        TypedValue {
            ty: LlType::Int(width),
            value,
        }
    }

    #[test]
    fn integer_folds_wrap_and_compare_at_the_operand_width() {
        let folded = |opcode: &str, width: u32, a: u64, b: u64| {
            fold_int_binary(opcode, &int(width, LlValue::Int(a)), &int(width, LlValue::Int(b)))
                .map(|value| value.value)
        };
        assert_eq!(
            folded("and", 32, 147_456, 1 << 30),
            Some(LlValue::Int(0)),
            "a clear flag bit must fold to zero"
        );
        assert_eq!(folded("add", 8, 250, 10), Some(LlValue::Int(4)), "i8 add wraps");
        assert_eq!(folded("ashr", 8, 0x80, 7), Some(LlValue::Int(0xff)), "ashr keeps the sign");
        assert_eq!(folded("shl", 8, 1, 8), None, "a shift by the width is poison, not zero");
        assert_eq!(folded("udiv", 32, 8, 2), None, "division is left alone");
        assert_eq!(
            fold_int_binary("and", &int(32, LlValue::Int(1)), &int(8, LlValue::Int(1))),
            None,
            "mismatched widths are not folded"
        );
        assert_eq!(
            fold_int_compare("slt", &int(8, LlValue::Int(0xff)), &int(8, LlValue::Int(0))),
            Some(true),
            "0xff is -1 as an i8"
        );
        assert_eq!(
            fold_int_compare("ult", &int(8, LlValue::SignedInt(-1)), &int(8, LlValue::Int(0))),
            Some(false),
            "-1 is 255 unsigned"
        );
        assert_eq!(
            fold_int_cast("sext", &int(8, LlValue::Int(0x80)), &LlType::Int(32)).map(|v| v.value),
            Some(LlValue::Int(0xffff_ff80))
        );
        assert_eq!(
            fold_int_cast("trunc", &int(32, LlValue::Int(0x104)), &LlType::Int(1)).map(|v| v.value),
            Some(LlValue::Bool(false)),
            "a one-bit result is spelled as a boolean so branch pruning sees it"
        );
        assert_eq!(
            fold_int_cast("zext", &int(32, LlValue::Int(1)), &LlType::Float),
            None,
            "only integer destinations fold"
        );
    }

    #[test]
    fn call_sites_must_agree_before_an_argument_is_a_fact() {
        let scalar = |value: u64| Some(ArgumentFact::Scalar(int(32, LlValue::Int(value))));
        let fields = |value: u64| Some(ArgumentFact::Fields(vec![(0, int(32, LlValue::Int(value)))]));
        let mut known = vec![scalar(7), scalar(7), fields(1), fields(1), scalar(3)];
        meet_argument_facts(
            &mut known,
            vec![scalar(7), scalar(8), fields(1), fields(2), None],
        );
        assert_eq!(known, vec![scalar(7), None, fields(1), None, None]);
        let mut known = vec![scalar(7)];
        meet_argument_facts(&mut known, vec![scalar(7), scalar(7)]);
        assert_eq!(known, vec![None], "a call with another argument count agrees on nothing");
    }

    #[test]
    fn function_constant_default_folds_cross_storage_pointer_select() {
        let ll = r#"
@fc.MTL_FC_INIT_23_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@has_buffer = internal addrspace(2) global i8 undef
@fallback = internal addrspace(2) constant [1 x float] zeroinitializer

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %value = load i8, ptr addrspace(2) @fc.MTL_FC_INIT_23_b
  store i8 %value, ptr addrspace(2) @has_buffer
  ret void
}

define void @main(ptr addrspace(1) %buffer) {
  %present = load i8, ptr addrspace(2) @has_buffer
  %disabled = icmp eq i8 %present, 0
  %selected = select i1 %disabled, ptr addrspace(2) @fallback, ptr addrspace(1) %buffer
  %value = load float, ptr addrspace(2) %selected
  ret void
}

!air.fragment = !{!0}
!0 = !{ptr @main, ptr addrspace(2) @fc.MTL_FC_INIT_23_b, ptr addrspace(2) @has_buffer}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        module.fold_static_initializer_constants();
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        let instructions = entry.carrier_insts().collect::<Vec<_>>();
        assert!(
            instructions
                .iter()
                .all(|inst| !matches!(inst.opcode.as_str(), "icmp" | "select")),
            "constant comparison/select must disappear before pointer emission"
        );
        let load = instructions
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%value"))
            .and_then(|inst| inst.load().as_ref())
            .expect("surviving payload load");
        assert!(matches!(load.ptr.value, LlValue::Global(ref name) if name == "@fallback"));
    }

    #[test]
    fn function_constant_default_prunes_dead_cfg_arm_and_phi_incoming() {
        let ll = r#"
@fc.MTL_FC_INIT_7_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@enabled = internal addrspace(2) global i8 undef

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %value = load i8, ptr addrspace(2) @fc.MTL_FC_INIT_7_b
  %defined = call i1 @air.is_function_constant_defined(ptr addrspace(2) @fc.MTL_FC_INIT_7_b)
  %selected = select i1 %defined, i8 %value, i8 0
  store i8 %selected, ptr addrspace(2) @enabled
  ret void
}

define i32 @main() {
entry:
  %value = load i8, ptr addrspace(2) @enabled
  %disabled = icmp eq i8 %value, 0
  br i1 %disabled, label %live, label %dead
live:
  br label %merge
dead:
  br label %merge
merge:
  %result = phi i32 [ 7, %live ], [ 9, %dead ]
  ret i32 %result
}

declare i1 @air.is_function_constant_defined(ptr addrspace(2))
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        module.fold_static_initializer_constants();
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(
            entry
                .blocks
                .iter()
                .map(|block| block.name.as_str())
                .collect::<Vec<_>>(),
            ["%entry", "%live", "%merge"]
        );
        let phi = entry.blocks[2]
            .typed
            .as_ref()
            .and_then(|block| block.insts.first())
            .and_then(|inst| inst.phi_incoming().as_ref())
            .expect("merge phi");
        assert_eq!(phi.1.len(), 1);
        assert!(
            matches!(phi.1.as_slice(), [(LlValue::Int(7), predecessor)] if predecessor == "%live")
        );
    }

    #[test]
    fn vector_function_constant_default_prunes_a_shape_dispatch() {
        let ll = r#"
@fc.MTL_FC_INIT_1_Dv2_t = internal addrspace(2) externally_initialized constant <2 x i16> undef, section "air.fc_initializer", align 4
@shape = internal addrspace(2) global <2 x i16> undef, align 4

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %value = load <2 x i16>, ptr addrspace(2) @fc.MTL_FC_INIT_1_Dv2_t
  store <2 x i16> %value, ptr addrspace(2) @shape
  ret void
}

define void @main() {
entry:
  %shape = load <2 x i16>, ptr addrspace(2) @shape
  %x = extractelement <2 x i16> %shape, i64 0
  %x_is_one = icmp eq i16 %x, 1
  %y = extractelement <2 x i16> %shape, i64 1
  %y_is_one = icmp eq i16 %y, 1
  %both = select i1 %x_is_one, i1 %y_is_one, i1 false
  br i1 %both, label %specialized, label %default
specialized:
  br label %exit
default:
  br label %exit
exit:
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        module.fold_static_initializer_constants();
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(
            entry
                .blocks
                .iter()
                .map(|block| block.name.as_str())
                .collect::<Vec<_>>(),
            ["%entry", "%default", "%exit"]
        );
    }

    #[test]
    fn function_constant_zero_bound_removes_an_unsigned_loop_backedge() {
        let ll = r#"
@fc.MTL_FC_INIT_2_t = internal addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@rounded = internal addrspace(2) global i16 undef, align 2

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %value = load i16, ptr addrspace(2) @fc.MTL_FC_INIT_2_t
  %biased = add i16 %value, 15
  %masked = and i16 %biased, -16
  store i16 %masked, ptr addrspace(2) @rounded
  ret void
}

define void @main() {
entry:
  br label %loop
loop:
  %index = phi i16 [ 0, %entry ], [ %next, %loop ]
  %next = add i16 %index, 16
  %bound = load i16, ptr addrspace(2) @rounded
  %more = icmp ult i16 %next, %bound
  br i1 %more, label %loop, label %exit
exit:
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        module.fold_static_initializer_constants();
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        let loop_block = entry
            .blocks
            .iter()
            .find(|block| block.name == "%loop")
            .and_then(|block| block.typed.as_ref())
            .expect("loop block");
        assert_eq!(loop_block.terminator, TirTerminator::Br("%exit".into()));
        let phi = loop_block
            .insts
            .iter()
            .find(|inst| inst.opcode == "phi")
            .and_then(|inst| inst.phi_incoming().as_ref())
            .expect("loop phi");
        assert!(matches!(
            phi.1.as_slice(),
            [(LlValue::Int(0), predecessor)] if predecessor == "%entry"
        ));
    }

    #[test]
    fn static_initializer_suffix_carries_direct_calls_in_source_order() {
        let ll = r#"
@a = internal addrspace(2) global i8 0

define internal void @_GLOBAL__sub_I_leaf() section "air.static_init" {
  store i8 1, ptr addrspace(2) @a
  ret void
}

define internal void @_GLOBAL__sub_I_callful() section "air.static_init" {
  call void @air.test()
  ret void
}

define void @main() {
  store i8 2, ptr addrspace(2) @a
  ret void
}

declare void @air.test()
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        assert_eq!(module.preinlined_static_initializers.len(), 2);
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(
            entry
                .carrier_insts()
                .map(|instruction| instruction.opcode.as_str())
                .collect::<Vec<_>>(),
            ["store", "call", "store"],
            "direct-call initializer must retain source order in the typed suffix"
        );
    }

    #[test]
    fn static_initializer_three_block_cfg_remains_independent_until_emitted_closure() {
        let ll = r#"
@a = internal addrspace(2) global i8 0

define internal void @_GLOBAL__sub_I_before() section "air.static_init" {
  store i8 1, ptr addrspace(2) @a
  ret void
}

define internal void @_GLOBAL__sub_I_cfg() section "air.static_init" {
entry:
  %c = icmp eq i8 0, 0
  br i1 %c, label %then, label %merge
then:
  store i8 2, ptr addrspace(2) @a
  br label %merge
merge:
  %v = phi i8 [ 3, %entry ], [ 4, %then ]
  store i8 %v, ptr addrspace(2) @a
  ret void
}

define internal void @_GLOBAL__sub_I_after() section "air.static_init" {
  store i8 5, ptr addrspace(2) @a
  ret void
}

define void @main() {
  store i8 9, ptr addrspace(2) @a
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        assert_eq!(
            module.preinlined_static_initializers,
            crate::fxhash::set_from(["_GLOBAL__sub_I_after".to_string()]),
            "only the straight-line suffix crosses the pre-emission inference boundary"
        );
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(
            entry
                .carrier_insts()
                .map(|instruction| instruction.opcode.as_str())
                .collect::<Vec<_>>(),
            ["store", "store"],
            "the post-CFG straight-line constructor stays before the entry body"
        );
    }

    #[test]
    fn static_initializer_two_block_cfg_remains_independent_until_emitted_closure() {
        let ll = r#"
@a = internal addrspace(2) global i8 0

define internal void @_GLOBAL__sub_I_cfg() section "air.static_init" {
entry:
  br label %exit
exit:
  store i8 1, ptr addrspace(2) @a
  ret void
}

define void @main() {
  store i8 9, ptr addrspace(2) @a
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        assert!(module.preinlined_static_initializers.is_empty());
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(entry.blocks.len(), 1);
    }

    #[test]
    fn static_initializer_chained_cfgs_remain_independent_until_emitted_closure() {
        let ll = r#"
define internal void @_GLOBAL__sub_I_first() section "air.static_init" {
entry:
  %c = icmp eq i8 0, 0
  br i1 %c, label %then, label %merge
then:
  br label %merge
merge:
  ret void
}

define internal void @_GLOBAL__sub_I_second() section "air.static_init" {
entry:
  %c = icmp eq i8 0, 0
  br i1 %c, label %then, label %merge
then:
  br label %merge
merge:
  ret void
}

define void @main() {
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        assert!(
            module.preinlined_static_initializers.is_empty(),
            "constructor CFGs retain their independent pre-emission domains"
        );
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(entry.blocks.len(), 1);
    }

    #[test]
    fn static_initializer_suffix_leaves_indirect_calls_residual() {
        let ll = r#"
@a = internal addrspace(2) global i8 0

define internal void @_GLOBAL__sub_I_indirect() section "air.static_init" {
  %fn = inttoptr i64 0 to ptr
  call void %fn()
  ret void
}

define void @main() {
  store i8 2, ptr addrspace(2) @a
  ret void
}
"#;
        let mut module =
            LlModule::parse_with_stage_meta(ll, None, Some("main")).expect("typed module");
        module.inline_simple_static_initializers();
        assert!(
            module.preinlined_static_initializers.is_empty(),
            "indirect-call constructor must remain for the emitted-graph closure"
        );
        let entry = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("entry");
        assert_eq!(entry.carrier_insts().count(), 1);
    }
}
