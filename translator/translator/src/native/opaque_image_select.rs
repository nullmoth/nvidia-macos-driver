use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Op, Word};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Site {
    function: usize,
    block: usize,
    instruction: usize,
}

#[derive(Clone, Copy, Debug)]
struct UseSite {
    site: Site,
    operand: usize,
}

struct TreePlan {
    select_ids: BTreeSet<Word>,
    sampled_image_ids: HashSet<Word>,
    replacements: HashMap<Site, Vec<Instruction>>,
}

#[derive(Clone)]
enum NestedPhiArm {
    Image { label: Word, ordinal: u32 },
    Phi { result: Word, label: Word },
}

#[derive(Clone)]
struct NestedPhiNode {
    site: Site,
    result: Word,
    arms: Vec<NestedPhiArm>,
}

#[derive(Clone)]
enum NestedPhiConsumer {
    Direct(UseSite),
    Sampled {
        sampled_site: Site,
        sample_uses: Vec<UseSite>,
    },
}

struct NestedPhiPlan {
    image_ty: Word,
    nodes: Vec<NestedPhiNode>,
    leaves: Vec<Word>,
    consumers: Vec<NestedPhiConsumer>,
}

pub(super) fn construct_opaque_image_selects(module: &mut Module) -> bool {
    if !has_opaque_image_merge_consumer(module) {
        return false;
    }
    let direct_changed = rewrite_opaque_image_direct_selects(module);
    let nested_phi_changed = rewrite_nested_opaque_image_phis(module);
    let phi_changed = rewrite_opaque_image_phis(module);
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    let mut value_defs = type_defs.clone();
    let mut value_types: HashMap<Word, Word> = module
        .types_global_values
        .iter()
        .filter_map(|inst| match (inst.result_id, inst.result_type) {
            (Some(id), Some(ty)) => Some((id, ty)),
            _ => None,
        })
        .collect();
    let global_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id)
        .collect();
    let mut positions: HashMap<Word, Site> = HashMap::default();
    let mut parameter_function: HashMap<Word, usize> = HashMap::default();
    let mut uses: HashMap<Word, Vec<UseSite>> = HashMap::default();

    for (function, func) in module.functions.iter().enumerate() {
        for parameter in &func.parameters {
            if let Some(id) = parameter.result_id {
                parameter_function.insert(id, function);
                if let Some(ty) = parameter.result_type {
                    value_types.insert(id, ty);
                }
            }
        }
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(id) = inst.result_id {
                    value_defs.insert(id, inst.clone());
                    positions.insert(id, site);
                    if let Some(ty) = inst.result_type {
                        value_types.insert(id, ty);
                    }
                }
                for (operand, op) in inst.operands.iter().enumerate() {
                    if let Operand::IdRef(id) = op {
                        uses.entry(*id).or_default().push(UseSite { site, operand });
                    }
                }
            }
        }
    }

    let mut candidates = Vec::new();
    for (function, func) in module.functions.iter().enumerate() {
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                if inst.class.opcode != Op::SampledImage {
                    continue;
                }
                let Some(Operand::IdRef(image)) = inst.operands.first() else {
                    continue;
                };
                if value_defs
                    .get(image)
                    .is_some_and(|def| def.class.opcode == Op::Select)
                {
                    candidates.push((
                        Site {
                            function,
                            block,
                            instruction,
                        },
                        *image,
                    ));
                }
            }
        }
    }
    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(0);
    let mut processed_selects = HashSet::default();
    let mut select_ids_to_delete = HashSet::default();
    let mut sampled_image_ids_to_delete = HashSet::default();
    let mut replacements: HashMap<Site, Vec<Instruction>> = HashMap::default();

    for (_, root) in candidates {
        if processed_selects.contains(&root) {
            continue;
        }
        let Some(plan) = plan_tree(
            module,
            root,
            &type_defs,
            &value_defs,
            &value_types,
            &positions,
            &parameter_function,
            &global_ids,
            &uses,
            &mut next_id,
        ) else {
            continue;
        };
        if plan
            .select_ids
            .iter()
            .any(|id| processed_selects.contains(id))
        {
            continue;
        }
        processed_selects.extend(plan.select_ids.iter().copied());
        select_ids_to_delete.extend(plan.select_ids);
        sampled_image_ids_to_delete.extend(plan.sampled_image_ids);
        replacements.extend(plan.replacements);
    }

    if replacements.is_empty() {
        let changed = direct_changed || nested_phi_changed || phi_changed;
        if changed {
            super::constfold::dce_preserving(module, &HashSet::default());
        }
        return changed;
    }

    for (function, func) in module.functions.iter_mut().enumerate() {
        for (block, blk) in func.blocks.iter_mut().enumerate() {
            let old = blk.instructions.clone();
            let mut rebuilt = Vec::with_capacity(old.len());
            for (instruction, inst) in old.into_iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(replacement) = replacements.remove(&site) {
                    rebuilt.extend(replacement);
                    continue;
                }
                if inst.result_id.is_some_and(|id| {
                    select_ids_to_delete.contains(&id) || sampled_image_ids_to_delete.contains(&id)
                }) {
                    continue;
                }
                rebuilt.push(inst);
            }
            blk.instructions = rebuilt;
        }
    }

    let removed: HashSet<Word> = select_ids_to_delete
        .into_iter()
        .chain(sampled_image_ids_to_delete)
        .collect();
    let targets_removed = |inst: &Instruction| matches!(inst.operands.first(), Some(Operand::IdRef(id)) if removed.contains(id));
    module.debug_names.retain(|inst| !targets_removed(inst));
    module.annotations.retain(|inst| !targets_removed(inst));
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    super::constfold::dce_preserving(module, &HashSet::default());
    true
}

fn has_opaque_image_merge_consumer(module: &Module) -> bool {
    let merge_ids = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction.class.opcode, Op::Select | Op::Phi))
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    if merge_ids.is_empty() {
        return false;
    }
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            (is_direct_image_value_op(instruction.class.opcode)
                || instruction.class.opcode == Op::SampledImage)
                && matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if merge_ids.contains(id))
        })
}

fn rewrite_opaque_image_direct_selects(module: &mut Module) -> bool {
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
        .collect();
    let mut value_defs = type_defs.clone();
    let mut value_types = module
        .types_global_values
        .iter()
        .filter_map(
            |instruction| match (instruction.result_id, instruction.result_type) {
                (Some(id), Some(ty)) => Some((id, ty)),
                _ => None,
            },
        )
        .collect::<HashMap<_, _>>();
    let global_ids = module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let mut positions = HashMap::default();
    let mut parameter_function = HashMap::default();
    let mut uses: HashMap<Word, Vec<UseSite>> = HashMap::default();
    let mut candidates = BTreeSet::new();
    for (function, func) in module.functions.iter().enumerate() {
        for parameter in &func.parameters {
            if let Some(id) = parameter.result_id {
                parameter_function.insert(id, function);
                if let Some(ty) = parameter.result_type {
                    value_types.insert(id, ty);
                }
            }
        }
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(id) = inst.result_id {
                    value_defs.insert(id, inst.clone());
                    positions.insert(id, site);
                    if let Some(ty) = inst.result_type {
                        value_types.insert(id, ty);
                    }
                }
                for (operand, value) in inst.operands.iter().enumerate() {
                    if let Operand::IdRef(id) = value {
                        uses.entry(*id).or_default().push(UseSite { site, operand });
                    }
                }
                if is_direct_image_value_op(inst.class.opcode) {
                    if let Some(Operand::IdRef(image)) = inst.operands.first() {
                        if value_defs
                            .get(image)
                            .is_some_and(|definition| definition.class.opcode == Op::Select)
                        {
                            candidates.insert(*image);
                        }
                    }
                }
            }
        }
    }
    let dominators = module
        .functions
        .iter()
        .map(block_dominator_indices)
        .collect::<Vec<_>>();

    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(0);
    let mut processed = HashSet::default();
    let mut delete = HashSet::default();
    let mut replacements = HashMap::<Site, Vec<Instruction>>::default();
    for root in candidates {
        if processed.contains(&root) {
            continue;
        }
        let mut image_visiting = HashSet::default();
        let Some(image_ty) = select_tree_image_type(
            root,
            &type_defs,
            &value_defs,
            &value_types,
            &mut image_visiting,
        ) else {
            continue;
        };
        let mut select_ids = BTreeSet::new();
        let mut visiting = HashSet::default();
        if collect_select_tree(
            root,
            image_ty,
            &type_defs,
            &value_defs,
            &value_types,
            &mut select_ids,
            &mut visiting,
        )
        .is_none()
            || select_ids.iter().any(|id| processed.contains(id))
        {
            continue;
        }

        let mut consumers = Vec::new();
        let mut valid = true;
        for &select in &select_ids {
            for use_site in uses.get(&select).into_iter().flatten() {
                let Some(consumer) = instruction_at(module, use_site.site) else {
                    valid = false;
                    break;
                };
                let tree_parent = consumer.class.opcode == Op::Select
                    && consumer
                        .result_id
                        .is_some_and(|id| select_ids.contains(&id))
                    && use_site.operand >= 1;
                if tree_parent {
                    continue;
                }
                if use_site.operand != 0
                    || !is_direct_image_value_op(consumer.class.opcode)
                    || consumer.result_id.is_none()
                    || consumer
                        .result_type
                        .is_none_or(|ty| is_opaque_type(&type_defs, ty))
                    || !tree_values_dominate_site(
                        &select_ids,
                        use_site.site,
                        &value_defs,
                        &positions,
                        &parameter_function,
                        &global_ids,
                        &dominators,
                    )
                {
                    valid = false;
                    break;
                }
                consumers.push((select, *use_site));
            }
            if !valid {
                break;
            }
        }
        if !valid || consumers.is_empty() {
            continue;
        }

        let mut planned = Vec::new();
        for (selected_image, use_site) in consumers {
            let Some(consumer) = instruction_at(module, use_site.site) else {
                valid = false;
                break;
            };
            let mut replacement = Vec::new();
            let mut replay_visiting = HashSet::default();
            if replay_selected_direct_image(
                selected_image,
                image_ty,
                consumer,
                consumer.result_id,
                &type_defs,
                &value_defs,
                &value_types,
                &mut replay_visiting,
                &mut next_id,
                &mut replacement,
            )
            .is_none()
            {
                valid = false;
                break;
            }
            planned.push((use_site.site, replacement));
        }
        if !valid
            || planned
                .iter()
                .any(|(site, _)| replacements.contains_key(site))
        {
            continue;
        }
        replacements.extend(planned);
        processed.extend(select_ids.iter().copied());
        delete.extend(select_ids);
    }
    if replacements.is_empty() {
        return false;
    }

    for (function, func) in module.functions.iter_mut().enumerate() {
        for (block, blk) in func.blocks.iter_mut().enumerate() {
            let old = blk.instructions.clone();
            let mut rebuilt = Vec::with_capacity(old.len());
            for (instruction, inst) in old.into_iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(replacement) = replacements.remove(&site) {
                    rebuilt.extend(replacement);
                } else if !inst.result_id.is_some_and(|id| delete.contains(&id)) {
                    rebuilt.push(inst);
                }
            }
            blk.instructions = rebuilt;
        }
    }
    let targets_deleted = |instruction: &Instruction| matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if delete.contains(id));
    module
        .debug_names
        .retain(|instruction| !targets_deleted(instruction));
    module
        .annotations
        .retain(|instruction| !targets_deleted(instruction));
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn collect_nested_phi_tree(
    result: Word,
    function: usize,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    positions: &HashMap<Word, Site>,
    visiting: &mut HashSet<Word>,
    image_types: &mut HashMap<Word, Word>,
    nodes: &mut Vec<NestedPhiNode>,
    leaves: &mut Vec<Word>,
) -> Option<Word> {
    if let Some(image_ty) = image_types.get(&result) {
        return Some(*image_ty);
    }
    if !visiting.insert(result) {
        return None;
    }
    let definition = value_defs.get(&result)?;
    let site = *positions.get(&result)?;
    if site.function != function
        || definition.class.opcode != Op::Phi
        || !definition
            .result_type
            .is_some_and(|ty| is_pointer_type(type_defs, ty))
        || definition.operands.len() < 4
        || !definition.operands.len().is_multiple_of(2)
    {
        visiting.remove(&result);
        return None;
    }
    let mut image_ty = None;
    let mut arms = Vec::new();
    for pair in definition.operands.chunks_exact(2) {
        let (Operand::IdRef(value), Operand::IdRef(label)) = (&pair[0], &pair[1]) else {
            visiting.remove(&result);
            return None;
        };
        let value_ty = *value_types.get(value)?;
        let (arm, arm_image_ty) = if is_image_type(type_defs, value_ty) {
            let ordinal = leaves.len() as u32;
            leaves.push(*value);
            (
                NestedPhiArm::Image {
                    label: *label,
                    ordinal,
                },
                value_ty,
            )
        } else if value_defs
            .get(value)
            .is_some_and(|inst| inst.class.opcode == Op::Phi)
        {
            let child_image_ty = collect_nested_phi_tree(
                *value,
                function,
                type_defs,
                value_defs,
                value_types,
                positions,
                visiting,
                image_types,
                nodes,
                leaves,
            )?;
            (
                NestedPhiArm::Phi {
                    result: *value,
                    label: *label,
                },
                child_image_ty,
            )
        } else {
            visiting.remove(&result);
            return None;
        };
        if image_ty.is_some_and(|expected| expected != arm_image_ty) {
            visiting.remove(&result);
            return None;
        }
        image_ty = Some(arm_image_ty);
        arms.push(arm);
    }
    visiting.remove(&result);
    let image_ty = image_ty?;
    image_types.insert(result, image_ty);
    nodes.push(NestedPhiNode { site, result, arms });
    Some(image_ty)
}

fn rewrite_nested_opaque_image_phis(module: &mut Module) -> bool {
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    let value_types: HashMap<Word, Word> = module
        .all_inst_iter()
        .filter_map(|inst| match (inst.result_id, inst.result_type) {
            (Some(id), Some(ty)) => Some((id, ty)),
            _ => None,
        })
        .collect();
    let value_defs: HashMap<Word, Instruction> = module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    let global_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id)
        .collect();
    let mut positions = HashMap::default();
    let mut uses: HashMap<Word, Vec<UseSite>> = HashMap::default();
    for (function, func) in module.functions.iter().enumerate() {
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(id) = inst.result_id {
                    positions.insert(id, site);
                }
                for (operand, value) in inst.operands.iter().enumerate() {
                    if let Operand::IdRef(id) = value {
                        uses.entry(*id).or_default().push(UseSite { site, operand });
                    }
                }
            }
        }
    }
    let dominators = module
        .functions
        .iter()
        .map(block_dominator_indices)
        .collect::<Vec<_>>();
    let value_dominates_site = |value: Word, site: Site| {
        if global_ids.contains(&value) {
            return true;
        }
        let Some(definition) = positions.get(&value).copied() else {
            return false;
        };
        definition.function == site.function
            && if definition.block == site.block {
                definition.instruction < site.instruction
            } else {
                dominators
                    .get(site.function)
                    .and_then(|function| function.get(site.block))
                    .is_some_and(|blocks| blocks.contains(&definition.block))
            }
    };
    let image_available_at_site = |value: Word, site: Site| {
        value_dominates_site(value, site)
            || value_defs.get(&value).is_some_and(|inst| {
                inst.class.opcode == Op::Load
                    && matches!(inst.operands.first(), Some(Operand::IdRef(root)) if global_ids.contains(root))
            })
    };
    let uint_ty = type_defs.iter().find_map(|(&id, inst)| {
        (inst.class.opcode == Op::TypeInt
            && matches!(
                inst.operands.as_slice(),
                [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
            ))
        .then_some(id)
    });
    let bool_ty = type_defs
        .iter()
        .find_map(|(&id, inst)| (inst.class.opcode == Op::TypeBool).then_some(id));
    let (Some(uint_ty), Some(bool_ty)) = (uint_ty, bool_ty) else {
        return false;
    };

    let candidates = positions
        .iter()
        .filter_map(|(&result, &site)| {
            let definition = value_defs.get(&result)?;
            (definition.class.opcode == Op::Phi
                && definition
                    .result_type
                    .is_some_and(|ty| is_pointer_type(&type_defs, ty))
                && uses.get(&result).is_some_and(|result_uses| {
                    result_uses.iter().any(|use_site| {
                        instruction_at(module, use_site.site).is_some_and(|consumer| {
                            use_site.operand == 0
                                && (is_direct_image_value_op(consumer.class.opcode)
                                    || consumer.class.opcode == Op::SampledImage)
                        })
                    })
                }))
            .then_some((result, site.function))
        })
        .collect::<BTreeSet<_>>();

    let mut plans = Vec::new();
    let mut processed = HashSet::default();
    for (root, function) in candidates {
        if processed.contains(&root) {
            continue;
        }
        let mut nodes = Vec::new();
        let mut leaves = Vec::new();
        let mut visiting = HashSet::default();
        let mut image_types = HashMap::default();
        let Some(image_ty) = collect_nested_phi_tree(
            root,
            function,
            &type_defs,
            &value_defs,
            &value_types,
            &positions,
            &mut visiting,
            &mut image_types,
            &mut nodes,
            &mut leaves,
        ) else {
            continue;
        };
        if nodes.len() < 2 || nodes.iter().any(|node| processed.contains(&node.result)) {
            continue;
        }
        let node_ids = nodes.iter().map(|node| node.result).collect::<HashSet<_>>();
        let mut consumers = Vec::new();
        let mut valid = true;
        for node in &nodes {
            for use_site in uses.get(&node.result).into_iter().flatten() {
                let Some(consumer) = instruction_at(module, use_site.site) else {
                    valid = false;
                    break;
                };
                let tree_parent = consumer.class.opcode == Op::Phi
                    && consumer.result_id.is_some_and(|id| node_ids.contains(&id))
                    && use_site.operand.is_multiple_of(2);
                if tree_parent {
                    continue;
                }
                if node.result != root || use_site.operand != 0 {
                    valid = false;
                    break;
                }
                if is_direct_image_value_op(consumer.class.opcode)
                    && consumer.result_id.is_some()
                    && consumer.result_type.is_some()
                    && !consumer
                        .result_type
                        .is_some_and(|ty| is_opaque_type(&type_defs, ty))
                    && leaves
                        .iter()
                        .all(|value| image_available_at_site(*value, use_site.site))
                {
                    consumers.push(NestedPhiConsumer::Direct(*use_site));
                    continue;
                }
                if consumer.class.opcode != Op::SampledImage {
                    valid = false;
                    break;
                }
                let Some(sampled_id) = consumer.result_id else {
                    valid = false;
                    break;
                };
                let Some(sample_uses) = uses.get(&sampled_id).cloned() else {
                    valid = false;
                    break;
                };
                let sampler = consumer.operands.get(1).and_then(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                });
                if sample_uses.is_empty()
                    || sampler.is_none()
                    || sample_uses.iter().any(|sample_use| {
                        sample_use.operand != 0
                            || instruction_at(module, sample_use.site).is_none_or(|sample| {
                                !is_sampled_image_value_op(sample.class.opcode)
                                    || sample.result_id.is_none()
                                    || sample.result_type.is_none()
                                    || sample
                                        .result_type
                                        .is_some_and(|ty| is_opaque_type(&type_defs, ty))
                            })
                            || leaves
                                .iter()
                                .any(|value| !image_available_at_site(*value, sample_use.site))
                            || !value_dominates_site(
                                sampler.expect("sampled-image sampler was gated"),
                                sample_use.site,
                            )
                    })
                {
                    valid = false;
                    break;
                }
                consumers.push(NestedPhiConsumer::Sampled {
                    sampled_site: use_site.site,
                    sample_uses,
                });
            }
            if !valid {
                break;
            }
        }
        if valid && !consumers.is_empty() {
            processed.extend(node_ids);
            plans.push(NestedPhiPlan {
                image_ty,
                nodes,
                leaves,
                consumers,
            });
        }
    }
    if plans.is_empty() {
        return false;
    }

    let mut next_id = module.header.as_ref().map_or(1, |header| header.bound);
    let fresh = |next_id: &mut Word| {
        let id = *next_id;
        *next_id += 1;
        id
    };
    let max_leaves = plans
        .iter()
        .map(|plan| plan.leaves.len())
        .max()
        .unwrap_or(0);
    let mut constants = HashMap::default();
    for ordinal in 0..max_leaves as u32 {
        let id = module
            .types_global_values
            .iter()
            .find_map(|inst| {
                (inst.class.opcode == Op::Constant
                    && inst.result_type == Some(uint_ty)
                    && inst.operands.first() == Some(&Operand::LiteralBit32(ordinal)))
                .then_some(inst.result_id)
                .flatten()
            })
            .unwrap_or_else(|| fresh(&mut next_id));
        constants.insert(ordinal, id);
    }
    let existing_ids = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let first_variable = module
        .types_global_values
        .iter()
        .position(|inst| inst.class.opcode == Op::Variable)
        .unwrap_or(module.types_global_values.len());
    let mut new_constants = constants
        .iter()
        .filter(|(_, id)| !existing_ids.contains(id))
        .map(|(&value, &id)| {
            Instruction::new(
                Op::Constant,
                Some(uint_ty),
                Some(id),
                vec![Operand::LiteralBit32(value)],
            )
        })
        .collect::<Vec<_>>();
    new_constants.sort_by_key(|inst| inst.result_id);
    module
        .types_global_values
        .splice(first_variable..first_variable, new_constants);

    let mut sampled_type_by_image = type_defs
        .iter()
        .filter_map(|(&id, inst)| {
            (inst.class.opcode == Op::TypeSampledImage).then(|| {
                inst.operands.first().and_then(|operand| match operand {
                    Operand::IdRef(image) => Some((*image, id)),
                    _ => None,
                })
            })?
        })
        .collect::<HashMap<_, _>>();
    let mut replacements = HashMap::<Site, Vec<Instruction>>::default();
    let mut removed_ids = HashSet::default();
    for plan in plans {
        let tag_by_result = plan
            .nodes
            .iter()
            .map(|node| (node.result, fresh(&mut next_id)))
            .collect::<HashMap<_, _>>();
        for node in &plan.nodes {
            let operands = node
                .arms
                .iter()
                .flat_map(|arm| match arm {
                    NestedPhiArm::Image { label, ordinal, .. } => {
                        [Operand::IdRef(constants[ordinal]), Operand::IdRef(*label)]
                    }
                    NestedPhiArm::Phi { result, label } => [
                        Operand::IdRef(tag_by_result[result]),
                        Operand::IdRef(*label),
                    ],
                })
                .collect();
            replacements.insert(
                node.site,
                vec![Instruction::new(
                    Op::Phi,
                    Some(uint_ty),
                    Some(tag_by_result[&node.result]),
                    operands,
                )],
            );
            removed_ids.insert(node.result);
        }
        let root = plan
            .nodes
            .last()
            .expect("nested phi tree has a root")
            .result;
        let root_tag = tag_by_result[&root];
        for consumer_plan in plan.consumers {
            let (consumer, use_sites, sampled_template) = match consumer_plan {
                NestedPhiConsumer::Direct(use_site) => (
                    instruction_at(module, use_site.site)
                        .expect("nested image phi consumer disappeared")
                        .clone(),
                    vec![use_site],
                    None,
                ),
                NestedPhiConsumer::Sampled {
                    sampled_site,
                    sample_uses,
                } => {
                    let sampled = instruction_at(module, sampled_site)
                        .expect("nested image phi sampled-image consumer disappeared")
                        .clone();
                    replacements.insert(sampled_site, Vec::new());
                    if let Some(id) = sampled.result_id {
                        removed_ids.insert(id);
                    }
                    (sampled, sample_uses, Some(sampled_site))
                }
            };
            for use_site in use_sites {
                let value_consumer = if sampled_template.is_some() {
                    instruction_at(module, use_site.site)
                        .expect("nested image phi sample consumer disappeared")
                        .clone()
                } else {
                    consumer.clone()
                };
                let result_ty = value_consumer.result_type.expect("gated result type");
                let final_result = value_consumer.result_id.expect("gated result");
                let mut out = Vec::new();
                let mut arm_results = Vec::with_capacity(plan.leaves.len());
                for image in &plan.leaves {
                    let image = if value_dominates_site(*image, use_site.site) {
                        *image
                    } else {
                        let mut load = value_defs
                            .get(image)
                            .expect("image leaf was gated as a replayable load")
                            .clone();
                        let replayed = fresh(&mut next_id);
                        load.result_id = Some(replayed);
                        out.push(load);
                        replayed
                    };
                    let result = if plan.leaves.len() == 1 {
                        final_result
                    } else {
                        fresh(&mut next_id)
                    };
                    let mut cloned = value_consumer.clone();
                    cloned.result_id = Some(result);
                    if sampled_template.is_some() {
                        let sampled_ty = *sampled_type_by_image
                            .entry(plan.image_ty)
                            .or_insert_with(|| {
                                let id = fresh(&mut next_id);
                                module.types_global_values.push(Instruction::new(
                                    Op::TypeSampledImage,
                                    None,
                                    Some(id),
                                    vec![Operand::IdRef(plan.image_ty)],
                                ));
                                id
                            });
                        let sampled_result = fresh(&mut next_id);
                        let mut sampled = consumer.clone();
                        sampled.result_id = Some(sampled_result);
                        sampled.result_type = Some(sampled_ty);
                        sampled.operands[0] = Operand::IdRef(image);
                        out.push(sampled);
                        cloned.operands[0] = Operand::IdRef(sampled_result);
                    } else {
                        cloned.operands[0] = Operand::IdRef(image);
                    }
                    out.push(cloned);
                    arm_results.push(result);
                }
                let mut selected = arm_results[0];
                for ordinal in 1..arm_results.len() {
                    let condition = fresh(&mut next_id);
                    out.push(Instruction::new(
                        Op::IEqual,
                        Some(bool_ty),
                        Some(condition),
                        vec![
                            Operand::IdRef(root_tag),
                            Operand::IdRef(constants[&(ordinal as u32)]),
                        ],
                    ));
                    let result = if ordinal + 1 == arm_results.len() {
                        final_result
                    } else {
                        fresh(&mut next_id)
                    };
                    out.push(Instruction::new(
                        Op::Select,
                        Some(result_ty),
                        Some(result),
                        vec![
                            Operand::IdRef(condition),
                            Operand::IdRef(arm_results[ordinal]),
                            Operand::IdRef(selected),
                        ],
                    ));
                    selected = result;
                }
                replacements.insert(use_site.site, out);
            }
        }
    }
    for (function, func) in module.functions.iter_mut().enumerate() {
        for (block, blk) in func.blocks.iter_mut().enumerate() {
            let old = blk.instructions.clone();
            let mut rebuilt = Vec::with_capacity(old.len());
            for (instruction, inst) in old.into_iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(replacement) = replacements.remove(&site) {
                    rebuilt.extend(replacement);
                } else {
                    rebuilt.push(inst);
                }
            }
            blk.instructions = rebuilt;
        }
    }
    let targets_removed = |inst: &Instruction| matches!(inst.operands.first(), Some(Operand::IdRef(id)) if removed_ids.contains(id));
    module.debug_names.retain(|inst| !targets_removed(inst));
    module.annotations.retain(|inst| !targets_removed(inst));
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

fn rewrite_opaque_image_phis(module: &mut Module) -> bool {
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    let value_types: HashMap<Word, Word> = module
        .all_inst_iter()
        .filter_map(|inst| match (inst.result_id, inst.result_type) {
            (Some(id), Some(ty)) => Some((id, ty)),
            _ => None,
        })
        .collect();
    let value_defs: HashMap<Word, Instruction> = module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    let global_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id)
        .collect();
    let mut positions = HashMap::default();
    let mut uses: HashMap<Word, Vec<UseSite>> = HashMap::default();
    for (function, func) in module.functions.iter().enumerate() {
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(id) = inst.result_id {
                    positions.insert(id, site);
                }
                for (operand, value) in inst.operands.iter().enumerate() {
                    if let Operand::IdRef(id) = value {
                        uses.entry(*id).or_default().push(UseSite { site, operand });
                    }
                }
            }
        }
    }
    let dominators = module
        .functions
        .iter()
        .map(block_dominator_indices)
        .collect::<Vec<_>>();
    let value_dominates_site = |value: Word, site: Site| {
        if global_ids.contains(&value) {
            return true;
        }
        let Some(def) = positions.get(&value).copied() else {
            return false;
        };
        if def.function != site.function {
            return false;
        }
        if def.block == site.block {
            return def.instruction < site.instruction;
        }
        dominators
            .get(site.function)
            .and_then(|function| function.get(site.block))
            .is_some_and(|blocks| blocks.contains(&def.block))
    };
    let image_available_at_site = |value: Word, site: Site| {
        value_dominates_site(value, site)
            || value_defs.get(&value).is_some_and(|inst| {
                inst.class.opcode == Op::Load
                    && matches!(inst.operands.first(), Some(Operand::IdRef(root)) if global_ids.contains(root))
            })
    };

    let uint_ty = type_defs.iter().find_map(|(&id, inst)| {
        (inst.class.opcode == Op::TypeInt
            && matches!(
                inst.operands.as_slice(),
                [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
            ))
        .then_some(id)
    });
    let bool_ty = type_defs
        .iter()
        .find_map(|(&id, inst)| (inst.class.opcode == Op::TypeBool).then_some(id));
    let (Some(uint_ty), Some(bool_ty)) = (uint_ty, bool_ty) else {
        return false;
    };

    #[derive(Clone)]
    enum PhiConsumer {
        Direct(UseSite),
        Sampled {
            sampled_site: Site,
            sample_uses: Vec<UseSite>,
        },
    }

    #[derive(Clone)]
    struct PhiPlan {
        site: Site,
        result: Word,
        image_ty: Word,
        arms: Vec<(Word, Word)>,
        consumers: Vec<PhiConsumer>,
    }

    let mut plans = Vec::new();
    for (function, func) in module.functions.iter().enumerate() {
        for (block, blk) in func.blocks.iter().enumerate() {
            for (instruction, inst) in blk.instructions.iter().enumerate() {
                if inst.class.opcode != Op::Phi
                    || !inst
                        .result_type
                        .is_some_and(|ty| is_pointer_type(&type_defs, ty))
                {
                    continue;
                }
                let Some(result) = inst.result_id else {
                    continue;
                };
                let mut arms = Vec::new();
                let mut image_ty = None;
                let mut valid = inst.operands.len() >= 4 && inst.operands.len().is_multiple_of(2);
                for pair in inst.operands.chunks_exact(2) {
                    let (Operand::IdRef(value), Operand::IdRef(label)) = (&pair[0], &pair[1])
                    else {
                        valid = false;
                        break;
                    };
                    let Some(&ty) = value_types.get(value) else {
                        valid = false;
                        break;
                    };
                    if !is_image_type(&type_defs, ty)
                        || image_ty.is_some_and(|expected| expected != ty)
                    {
                        valid = false;
                        break;
                    }
                    image_ty = Some(ty);
                    arms.push((*value, *label));
                }
                if !valid {
                    continue;
                }
                let Some(result_uses) = uses.get(&result) else {
                    continue;
                };
                let mut consumers = Vec::new();
                let mut consumers_valid = !result_uses.is_empty();
                for use_site in result_uses {
                    let Some(consumer) = instruction_at(module, use_site.site) else {
                        consumers_valid = false;
                        break;
                    };
                    if use_site.operand != 0 {
                        consumers_valid = false;
                        break;
                    }
                    if matches!(
                        consumer.class.opcode,
                        Op::ImageQuerySizeLod | Op::ImageQuerySize | Op::ImageFetch | Op::ImageRead
                    ) && consumer.result_id.is_some()
                        && consumer.result_type.is_some()
                        && !consumer
                            .result_type
                            .is_some_and(|ty| is_opaque_type(&type_defs, ty))
                        && arms
                            .iter()
                            .all(|(value, _)| image_available_at_site(*value, use_site.site))
                    {
                        consumers.push(PhiConsumer::Direct(*use_site));
                        continue;
                    }
                    if consumer.class.opcode != Op::SampledImage {
                        consumers_valid = false;
                        break;
                    }
                    let Some(sampled_id) = consumer.result_id else {
                        consumers_valid = false;
                        break;
                    };
                    let Some(sample_uses) = uses.get(&sampled_id).cloned() else {
                        consumers_valid = false;
                        break;
                    };
                    let sampler = consumer.operands.get(1).and_then(|operand| match operand {
                        Operand::IdRef(id) => Some(*id),
                        _ => None,
                    });
                    if sample_uses.is_empty()
                        || sampler.is_none()
                        || sample_uses.iter().any(|sample_use| {
                            sample_use.operand != 0
                                || instruction_at(module, sample_use.site).is_none_or(|sample| {
                                    !is_sampled_image_value_op(sample.class.opcode)
                                        || sample.result_id.is_none()
                                        || sample.result_type.is_none()
                                        || sample
                                            .result_type
                                            .is_some_and(|ty| is_opaque_type(&type_defs, ty))
                                })
                                || arms.iter().any(|(value, _)| {
                                    !image_available_at_site(*value, sample_use.site)
                                })
                                || !value_dominates_site(
                                    sampler.expect("sampled-image sampler was gated"),
                                    sample_use.site,
                                )
                        })
                    {
                        consumers_valid = false;
                        break;
                    }
                    consumers.push(PhiConsumer::Sampled {
                        sampled_site: use_site.site,
                        sample_uses,
                    });
                }
                if !consumers_valid {
                    continue;
                }
                plans.push(PhiPlan {
                    site: Site {
                        function,
                        block,
                        instruction,
                    },
                    result,
                    image_ty: image_ty.expect("valid image phi has an image type"),
                    arms,
                    consumers,
                });
            }
        }
    }
    if plans.is_empty() {
        return false;
    }

    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(1);
    let fresh = |next_id: &mut Word| {
        let id = *next_id;
        *next_id += 1;
        id
    };
    let mut sampled_type_by_image = type_defs
        .iter()
        .filter_map(|(&id, inst)| {
            (inst.class.opcode == Op::TypeSampledImage)
                .then(|| match inst.operands.first() {
                    Some(Operand::IdRef(image)) => Some((*image, id)),
                    _ => None,
                })
                .flatten()
        })
        .collect::<HashMap<_, _>>();
    let mut new_sampled_types = HashMap::default();
    for image_ty in plans.iter().map(|plan| plan.image_ty) {
        sampled_type_by_image.entry(image_ty).or_insert_with(|| {
            let id = fresh(&mut next_id);
            new_sampled_types.insert(
                image_ty,
                Instruction::new(
                    Op::TypeSampledImage,
                    None,
                    Some(id),
                    vec![Operand::IdRef(image_ty)],
                ),
            );
            id
        });
    }
    let mut new_sampled_types = new_sampled_types.into_values().collect::<Vec<_>>();
    new_sampled_types.sort_by_key(|instruction| instruction.result_id);
    module.types_global_values.extend(new_sampled_types);
    let mut constants = HashMap::default();
    for plan in &plans {
        for ordinal in 0..plan.arms.len() as u32 {
            constants.entry(ordinal).or_insert_with(|| {
                module
                    .types_global_values
                    .iter()
                    .find_map(|inst| {
                        (inst.class.opcode == Op::Constant
                            && inst.result_type == Some(uint_ty)
                            && inst.operands.first() == Some(&Operand::LiteralBit32(ordinal)))
                        .then_some(inst.result_id)
                        .flatten()
                    })
                    .unwrap_or_else(|| fresh(&mut next_id))
            });
        }
    }
    let existing_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id)
        .collect();
    let first_variable = module
        .types_global_values
        .iter()
        .position(|inst| inst.class.opcode == Op::Variable)
        .unwrap_or(module.types_global_values.len());
    let mut new_constants = constants
        .iter()
        .filter(|(_, id)| !existing_ids.contains(id))
        .map(|(&value, &id)| {
            Instruction::new(
                Op::Constant,
                Some(uint_ty),
                Some(id),
                vec![Operand::LiteralBit32(value)],
            )
        })
        .collect::<Vec<_>>();
    new_constants.sort_by_key(|inst| inst.result_id);
    module
        .types_global_values
        .splice(first_variable..first_variable, new_constants);

    let mut replacements = HashMap::<Site, Vec<Instruction>>::default();
    let mut removed_ids = HashSet::default();
    for plan in plans {
        let tag = fresh(&mut next_id);
        let mut phi_operands = Vec::with_capacity(plan.arms.len() * 2);
        for (ordinal, (_, label)) in plan.arms.iter().enumerate() {
            phi_operands.push(Operand::IdRef(constants[&(ordinal as u32)]));
            phi_operands.push(Operand::IdRef(*label));
        }
        replacements.insert(
            plan.site,
            vec![Instruction::new(
                Op::Phi,
                Some(uint_ty),
                Some(tag),
                phi_operands,
            )],
        );
        removed_ids.insert(plan.result);

        for consumer_plan in plan.consumers {
            let (consumer, use_sites, sampled_template) = match consumer_plan {
                PhiConsumer::Direct(use_site) => (
                    instruction_at(module, use_site.site)
                        .expect("opaque image phi consumer disappeared")
                        .clone(),
                    vec![use_site],
                    None,
                ),
                PhiConsumer::Sampled {
                    sampled_site,
                    sample_uses,
                } => {
                    let sampled = instruction_at(module, sampled_site)
                        .expect("opaque image phi sampled-image consumer disappeared")
                        .clone();
                    replacements.insert(sampled_site, Vec::new());
                    if let Some(id) = sampled.result_id {
                        removed_ids.insert(id);
                    }
                    (sampled, sample_uses, Some(sampled_site))
                }
            };
            for use_site in use_sites {
                let value_consumer = if sampled_template.is_some() {
                    instruction_at(module, use_site.site)
                        .expect("opaque image phi sample consumer disappeared")
                        .clone()
                } else {
                    consumer.clone()
                };
                let result_ty = value_consumer
                    .result_type
                    .expect("gated image consumer result type");
                let final_result = value_consumer
                    .result_id
                    .expect("gated image consumer result");
                let mut out = Vec::new();
                let mut arm_results = Vec::with_capacity(plan.arms.len());
                for (image, _) in &plan.arms {
                    let image = if value_dominates_site(*image, use_site.site) {
                        *image
                    } else {
                        let mut load = value_defs
                            .get(image)
                            .expect("non-dominating image arm was gated as a replayable load")
                            .clone();
                        let replayed = fresh(&mut next_id);
                        load.result_id = Some(replayed);
                        out.push(load);
                        replayed
                    };
                    let result = fresh(&mut next_id);
                    let mut cloned = value_consumer.clone();
                    cloned.result_id = Some(result);
                    if sampled_template.is_some() {
                        let sampled_result = fresh(&mut next_id);
                        let mut sampled = consumer.clone();
                        sampled.result_id = Some(sampled_result);
                        sampled.result_type = Some(sampled_type_by_image[&plan.image_ty]);
                        sampled.operands[0] = Operand::IdRef(image);
                        out.push(sampled);
                        cloned.operands[0] = Operand::IdRef(sampled_result);
                    } else {
                        cloned.operands[0] = Operand::IdRef(image);
                    }
                    out.push(cloned);
                    arm_results.push(result);
                }
                let mut selected = arm_results[0];
                for ordinal in 1..arm_results.len() {
                    let condition = fresh(&mut next_id);
                    out.push(Instruction::new(
                        Op::IEqual,
                        Some(bool_ty),
                        Some(condition),
                        vec![
                            Operand::IdRef(tag),
                            Operand::IdRef(constants[&(ordinal as u32)]),
                        ],
                    ));
                    let result = if ordinal + 1 == arm_results.len() {
                        final_result
                    } else {
                        fresh(&mut next_id)
                    };
                    out.push(Instruction::new(
                        Op::Select,
                        Some(result_ty),
                        Some(result),
                        vec![
                            Operand::IdRef(condition),
                            Operand::IdRef(arm_results[ordinal]),
                            Operand::IdRef(selected),
                        ],
                    ));
                    selected = result;
                }
                replacements.insert(use_site.site, out);
            }
        }
    }

    for (function, func) in module.functions.iter_mut().enumerate() {
        for (block, blk) in func.blocks.iter_mut().enumerate() {
            let old = blk.instructions.clone();
            let mut rebuilt = Vec::with_capacity(old.len());
            for (instruction, inst) in old.into_iter().enumerate() {
                let site = Site {
                    function,
                    block,
                    instruction,
                };
                if let Some(replacement) = replacements.remove(&site) {
                    rebuilt.extend(replacement);
                } else {
                    rebuilt.push(inst);
                }
            }
            blk.instructions = rebuilt;
        }
    }
    let targets_removed = |inst: &Instruction| matches!(inst.operands.first(), Some(Operand::IdRef(id)) if removed_ids.contains(id));
    module.debug_names.retain(|inst| !targets_removed(inst));
    module.annotations.retain(|inst| !targets_removed(inst));
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

fn block_dominator_indices(function: &crate::spirv_module::Function) -> Vec<HashSet<usize>> {
    let labels = function
        .blocks
        .iter()
        .map(|block| block.label.as_ref().and_then(|label| label.result_id))
        .collect::<Vec<_>>();
    let by_label = labels
        .iter()
        .enumerate()
        .filter_map(|(index, label)| label.map(|label| (label, index)))
        .collect::<HashMap<_, _>>();
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for (index, block) in function.blocks.iter().enumerate() {
        let Some(terminator) = block.instructions.last() else {
            continue;
        };
        let successors = match terminator.class.opcode {
            Op::Branch => terminator.operands.first().into_iter().collect::<Vec<_>>(),
            Op::BranchConditional => terminator
                .operands
                .get(1..3)
                .unwrap_or_default()
                .iter()
                .collect(),
            Op::Switch => terminator
                .operands
                .iter()
                .skip(1)
                .enumerate()
                .filter_map(|(operand, value)| operand.is_multiple_of(2).then_some(value))
                .collect(),
            _ => Vec::new(),
        };
        for successor in successors {
            if let Operand::IdRef(label) = successor {
                if let Some(&target) = by_label.get(label) {
                    predecessors[target].push(index);
                }
            }
        }
    }
    let all = (0..function.blocks.len()).collect::<HashSet<_>>();
    let mut dominators = vec![all; function.blocks.len()];
    if dominators.is_empty() {
        return dominators;
    }
    dominators[0] = crate::fxhash::set_from([0]);
    loop {
        let mut changed = false;
        for block in 1..function.blocks.len() {
            if predecessors[block].is_empty() {
                continue;
            }
            let mut next = dominators[predecessors[block][0]].clone();
            for predecessor in predecessors[block].iter().skip(1) {
                next.retain(|candidate| dominators[*predecessor].contains(candidate));
            }
            next.insert(block);
            if dominators[block] != next {
                dominators[block] = next;
                changed = true;
            }
        }
        if !changed {
            return dominators;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_tree(
    module: &Module,
    root: Word,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    positions: &HashMap<Word, Site>,
    parameter_function: &HashMap<Word, usize>,
    global_ids: &HashSet<Word>,
    uses: &HashMap<Word, Vec<UseSite>>,
    next_id: &mut Word,
) -> Option<TreePlan> {
    let root_sampled_image = uses.get(&root)?.iter().find_map(|use_site| {
        let inst = instruction_at(module, use_site.site)?;
        (inst.class.opcode == Op::SampledImage && use_site.operand == 0).then_some(inst)
    })?;
    let sampled_image_ty = root_sampled_image.result_type?;
    let image_ty = sampled_image_image_type(type_defs, sampled_image_ty)?;
    if !is_image_type(type_defs, image_ty) {
        return None;
    }

    let mut select_ids = BTreeSet::new();
    let mut visiting = HashSet::default();
    collect_select_tree(
        root,
        image_ty,
        type_defs,
        value_defs,
        value_types,
        &mut select_ids,
        &mut visiting,
    )?;

    let mut sampled_images: BTreeMap<Word, (Word, Word, Word)> = BTreeMap::new();
    for &select in &select_ids {
        for use_site in uses.get(&select).into_iter().flatten() {
            let inst = instruction_at(module, use_site.site)?;
            let is_tree_parent = inst.class.opcode == Op::Select
                && inst.result_id.is_some_and(|id| select_ids.contains(&id))
                && use_site.operand >= 1;
            if is_tree_parent {
                continue;
            }
            if inst.class.opcode != Op::SampledImage || use_site.operand != 0 {
                return None;
            }
            let sampled_id = inst.result_id?;
            let sampled_ty = inst.result_type?;
            if sampled_image_image_type(type_defs, sampled_ty)? != image_ty {
                return None;
            }
            let sampler = match inst.operands.get(1)? {
                Operand::IdRef(id) => *id,
                _ => return None,
            };
            sampled_images
                .entry(sampled_id)
                .or_insert((sampled_ty, sampler, select));
        }
    }
    if sampled_images.is_empty() {
        return None;
    }

    let mut replacements = HashMap::default();
    for (&sampled_id, &(sampled_ty, sampler, selected_image)) in &sampled_images {
        let sample_uses = uses.get(&sampled_id)?;
        if sample_uses.is_empty() {
            return None;
        }
        for use_site in sample_uses {
            let sample = instruction_at(module, use_site.site)?;
            if !is_sampled_image_value_op(sample.class.opcode) || use_site.operand != 0 {
                return None;
            }
            let sample_result_ty = sample.result_type?;
            let sample_result = sample.result_id?;
            if is_opaque_type(type_defs, sample_result_ty)
                || !tree_values_available_at(
                    &select_ids,
                    use_site.site,
                    value_defs,
                    positions,
                    parameter_function,
                    global_ids,
                )
                || !value_available_at(
                    sampler,
                    use_site.site,
                    positions,
                    parameter_function,
                    global_ids,
                )
            {
                return None;
            }
            let mut replacement = Vec::new();
            let mut replay_visiting = HashSet::default();
            replay_selected_image(
                selected_image,
                image_ty,
                sampled_ty,
                sampler,
                sample,
                Some(sample_result),
                type_defs,
                value_defs,
                value_types,
                &mut replay_visiting,
                next_id,
                &mut replacement,
            )?;
            if replacements.insert(use_site.site, replacement).is_some() {
                return None;
            }
        }
    }

    Some(TreePlan {
        select_ids,
        sampled_image_ids: sampled_images.into_keys().collect(),
        replacements,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_select_tree(
    value: Word,
    image_ty: Word,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    select_ids: &mut BTreeSet<Word>,
    visiting: &mut HashSet<Word>,
) -> Option<()> {
    let def = value_defs.get(&value)?;
    if def.class.opcode != Op::Select {
        return (value_types.get(&value) == Some(&image_ty) && is_image_type(type_defs, image_ty))
            .then_some(());
    }
    if !def
        .result_type
        .is_some_and(|ty| is_pointer_type(type_defs, ty) || ty == image_ty)
    {
        return None;
    }
    if !visiting.insert(value) {
        return None;
    }
    let true_value = match def.operands.get(1)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let false_value = match def.operands.get(2)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    select_ids.insert(value);
    collect_select_tree(
        true_value,
        image_ty,
        type_defs,
        value_defs,
        value_types,
        select_ids,
        visiting,
    )?;
    collect_select_tree(
        false_value,
        image_ty,
        type_defs,
        value_defs,
        value_types,
        select_ids,
        visiting,
    )?;
    visiting.remove(&value);
    Some(())
}

fn is_direct_image_value_op(op: Op) -> bool {
    matches!(
        op,
        Op::ImageQuerySizeLod | Op::ImageQuerySize | Op::ImageFetch | Op::ImageRead
    )
}

fn is_sampled_image_value_op(op: Op) -> bool {
    matches!(op, Op::ImageSampleExplicitLod | Op::ImageQueryLod)
}

fn select_tree_image_type(
    value: Word,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    visiting: &mut HashSet<Word>,
) -> Option<Word> {
    let definition = value_defs.get(&value)?;
    if definition.class.opcode != Op::Select {
        let ty = *value_types.get(&value)?;
        return is_image_type(type_defs, ty).then_some(ty);
    }
    if !definition
        .result_type
        .is_some_and(|ty| is_pointer_type(type_defs, ty) || is_image_type(type_defs, ty))
        || !visiting.insert(value)
    {
        return None;
    }
    let true_value = match definition.operands.get(1)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let false_value = match definition.operands.get(2)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let true_ty = select_tree_image_type(true_value, type_defs, value_defs, value_types, visiting)?;
    let false_ty =
        select_tree_image_type(false_value, type_defs, value_defs, value_types, visiting)?;
    visiting.remove(&value);
    (true_ty == false_ty).then_some(true_ty)
}

#[allow(clippy::too_many_arguments)]
fn replay_selected_direct_image(
    value: Word,
    image_ty: Word,
    consumer: &Instruction,
    final_result: Option<Word>,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    visiting: &mut HashSet<Word>,
    next_id: &mut Word,
    out: &mut Vec<Instruction>,
) -> Option<Word> {
    let definition = value_defs.get(&value)?;
    if definition.class.opcode != Op::Select {
        if value_types.get(&value) != Some(&image_ty) || !is_image_type(type_defs, image_ty) {
            return None;
        }
        let result = final_result.unwrap_or_else(|| fresh(next_id));
        let mut replay = consumer.clone();
        replay.result_id = Some(result);
        *replay.operands.first_mut()? = Operand::IdRef(value);
        out.push(replay);
        return Some(result);
    }
    if !visiting.insert(value) {
        return None;
    }
    let condition = match definition.operands.first()? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let true_image = match definition.operands.get(1)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let false_image = match definition.operands.get(2)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let true_value = replay_selected_direct_image(
        true_image,
        image_ty,
        consumer,
        None,
        type_defs,
        value_defs,
        value_types,
        visiting,
        next_id,
        out,
    )?;
    let false_value = replay_selected_direct_image(
        false_image,
        image_ty,
        consumer,
        None,
        type_defs,
        value_defs,
        value_types,
        visiting,
        next_id,
        out,
    )?;
    visiting.remove(&value);
    let result = final_result.unwrap_or_else(|| fresh(next_id));
    out.push(Instruction::new(
        Op::Select,
        consumer.result_type,
        Some(result),
        vec![
            Operand::IdRef(condition),
            Operand::IdRef(true_value),
            Operand::IdRef(false_value),
        ],
    ));
    Some(result)
}

#[allow(clippy::too_many_arguments)]
fn replay_selected_image(
    value: Word,
    image_ty: Word,
    sampled_image_ty: Word,
    sampler: Word,
    sample: &Instruction,
    final_result: Option<Word>,
    type_defs: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    visiting: &mut HashSet<Word>,
    next_id: &mut Word,
    out: &mut Vec<Instruction>,
) -> Option<Word> {
    let def = value_defs.get(&value)?;
    if def.class.opcode != Op::Select {
        if value_types.get(&value) != Some(&image_ty) || !is_image_type(type_defs, image_ty) {
            return None;
        }
        let sampled = fresh(next_id);
        out.push(Instruction::new(
            Op::SampledImage,
            Some(sampled_image_ty),
            Some(sampled),
            vec![Operand::IdRef(value), Operand::IdRef(sampler)],
        ));
        let result = fresh(next_id);
        let mut cloned_sample = sample.clone();
        cloned_sample.result_id = Some(result);
        *cloned_sample.operands.first_mut()? = Operand::IdRef(sampled);
        out.push(cloned_sample);
        return Some(result);
    }
    if !visiting.insert(value) {
        return None;
    }
    let condition = match def.operands.first()? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let true_image = match def.operands.get(1)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let false_image = match def.operands.get(2)? {
        Operand::IdRef(id) => *id,
        _ => return None,
    };
    let true_value = replay_selected_image(
        true_image,
        image_ty,
        sampled_image_ty,
        sampler,
        sample,
        None,
        type_defs,
        value_defs,
        value_types,
        visiting,
        next_id,
        out,
    )?;
    let false_value = replay_selected_image(
        false_image,
        image_ty,
        sampled_image_ty,
        sampler,
        sample,
        None,
        type_defs,
        value_defs,
        value_types,
        visiting,
        next_id,
        out,
    )?;
    visiting.remove(&value);
    let result = final_result.unwrap_or_else(|| fresh(next_id));
    out.push(Instruction::new(
        Op::Select,
        sample.result_type,
        Some(result),
        vec![
            Operand::IdRef(condition),
            Operand::IdRef(true_value),
            Operand::IdRef(false_value),
        ],
    ));
    Some(result)
}

fn instruction_at(module: &Module, site: Site) -> Option<&Instruction> {
    module
        .functions
        .get(site.function)?
        .blocks
        .get(site.block)?
        .instructions
        .get(site.instruction)
}

fn sampled_image_image_type(
    type_defs: &HashMap<Word, Instruction>,
    sampled_image_ty: Word,
) -> Option<Word> {
    let def = type_defs.get(&sampled_image_ty)?;
    if def.class.opcode != Op::TypeSampledImage {
        return None;
    }
    match def.operands.first()? {
        Operand::IdRef(image_ty) => Some(*image_ty),
        _ => None,
    }
}

fn is_image_type(type_defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    type_defs
        .get(&ty)
        .is_some_and(|def| def.class.opcode == Op::TypeImage)
}

fn is_pointer_type(type_defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    type_defs
        .get(&ty)
        .is_some_and(|def| def.class.opcode == Op::TypePointer)
}

fn is_opaque_type(type_defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    type_defs.get(&ty).is_some_and(|def| {
        matches!(
            def.class.opcode,
            Op::TypeImage | Op::TypeSampler | Op::TypeSampledImage
        )
    })
}

fn tree_values_available_at(
    select_ids: &BTreeSet<Word>,
    site: Site,
    value_defs: &HashMap<Word, Instruction>,
    positions: &HashMap<Word, Site>,
    parameter_function: &HashMap<Word, usize>,
    global_ids: &HashSet<Word>,
) -> bool {
    select_ids.iter().all(|id| {
        let Some(def) = value_defs.get(id) else {
            return false;
        };
        def.operands.iter().all(|operand| match operand {
            Operand::IdRef(value) if !select_ids.contains(value) => {
                value_available_at(*value, site, positions, parameter_function, global_ids)
            }
            _ => true,
        })
    })
}

fn tree_values_dominate_site(
    select_ids: &BTreeSet<Word>,
    site: Site,
    value_defs: &HashMap<Word, Instruction>,
    positions: &HashMap<Word, Site>,
    parameter_function: &HashMap<Word, usize>,
    global_ids: &HashSet<Word>,
    dominators: &[Vec<HashSet<usize>>],
) -> bool {
    select_ids.iter().all(|id| {
        let Some(definition) = value_defs.get(id) else {
            return false;
        };
        definition.operands.iter().all(|operand| match operand {
            Operand::IdRef(value) if !select_ids.contains(value) => {
                global_ids.contains(value)
                    || parameter_function.get(value) == Some(&site.function)
                    || positions.get(value).is_some_and(|position| {
                        position.function == site.function
                            && if position.block == site.block {
                                position.instruction < site.instruction
                            } else {
                                dominators
                                    .get(site.function)
                                    .and_then(|function| function.get(site.block))
                                    .is_some_and(|blocks| blocks.contains(&position.block))
                            }
                    })
            }
            _ => true,
        })
    })
}

fn value_available_at(
    value: Word,
    site: Site,
    positions: &HashMap<Word, Site>,
    parameter_function: &HashMap<Word, usize>,
    global_ids: &HashSet<Word>,
) -> bool {
    global_ids.contains(&value)
        || parameter_function.get(&value) == Some(&site.function)
        || positions.get(&value).is_some_and(|position| {
            position.function == site.function
                && (position.block == 0
                    || (position.block == site.block && position.instruction < site.instruction))
        })
}

fn fresh(next_id: &mut Word) -> Word {
    let id = *next_id;
    *next_id += 1;
    id
}
