use crate::dominators::{build_predecessors, dominance, dominates_interval};
use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{is_block_terminator, Function, Instruction, Operand};
use spirv::{Op, Word};

pub(crate) enum OwnedModuleFailure {
    Invalid(String),
    CfgConstruction(String),
    TypeConstruction(String),
    RawBufferConstruction(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MergeKind {
    Selection,
    Loop { continue_target: usize },
}

#[derive(Clone, Copy, Debug)]
struct Construct {
    header: usize,
    merge: usize,
    kind: MergeKind,
    is_switch: bool,
}

#[derive(Clone, Copy, Debug)]
struct DefinitionSite {
    block: usize,
    instruction: usize,
}

#[derive(Debug)]
struct SwitchInfo {
    default: usize,
    targets: Vec<usize>,
}

struct OwnedCfg {
    labels: HashMap<Word, usize>,
    successors: Vec<Vec<usize>>,
    structural_successors: Vec<Vec<usize>>,
    predecessors: Vec<Vec<usize>>,
    flow_reachable: Vec<bool>,
    flow_dominance: Vec<Option<(usize, usize)>>,
    flow_idom: Vec<Option<usize>>,
    reachable: Vec<bool>,
    dominance: Vec<Option<(usize, usize)>>,
    idom: Vec<Option<usize>>,
    constructs: Vec<Construct>,
    switches: HashMap<usize, SwitchInfo>,
}

impl OwnedCfg {
    fn new(function: &Function) -> Result<Self, String> {
        let labels = function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                block
                    .label
                    .as_ref()
                    .and_then(|label| label.result_id)
                    .map(|label| (label, index))
                    .ok_or_else(|| "native emitter: owned CFG block has no label".to_string())
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        if labels.len() != function.blocks.len() {
            return Err("native emitter: owned CFG has duplicate block labels".to_string());
        }

        let mut successors = Vec::with_capacity(function.blocks.len());
        let mut structural_successors = Vec::with_capacity(function.blocks.len());
        let mut constructs = Vec::new();
        let mut switches = HashMap::default();
        let mut merge_owner = HashMap::<usize, usize>::default();
        for (header, block) in function.blocks.iter().enumerate() {
            let terminator = block
                .instructions
                .last()
                .ok_or_else(|| "native emitter: owned CFG block has no terminator".to_string())?;
            check_control_instruction_shape(terminator)?;
            let block_successors =
                successor_indices(terminator.class.opcode, &terminator.operands, &labels)?;
            if terminator.class.opcode == Op::Switch {
                switches.insert(header, switch_info(&terminator.operands, &labels)?);
            }
            let mut structural = block_successors.clone();
            if let Some(merge_instruction) = block.instructions.iter().rev().nth(1) {
                if matches!(
                    merge_instruction.class.opcode,
                    Op::SelectionMerge | Op::LoopMerge
                ) {
                    check_control_instruction_shape(merge_instruction)?;
                }
                let kind = match merge_instruction.class.opcode {
                    Op::SelectionMerge => {
                        let merge = operand_label(merge_instruction.operands.first(), &labels)?;
                        Some((merge, MergeKind::Selection))
                    }
                    Op::LoopMerge => {
                        let merge = operand_label(merge_instruction.operands.first(), &labels)?;
                        let continue_target =
                            operand_label(merge_instruction.operands.get(1), &labels)?;
                        Some((merge, MergeKind::Loop { continue_target }))
                    }
                    _ => None,
                };
                if let Some((merge, kind)) = kind {
                    if merge == header {
                        return Err(
                            "native emitter: owned construct header is its own merge".to_string()
                        );
                    }
                    if merge_owner.insert(merge, header).is_some() {
                        return Err("native emitter: owned merge is claimed by multiple headers"
                            .to_string());
                    }
                    structural.push(merge);
                    if let MergeKind::Loop { continue_target } = kind {
                        structural.push(continue_target);
                    }
                    constructs.push(Construct {
                        header,
                        merge,
                        kind,
                        is_switch: terminator.class.opcode == Op::Switch,
                    });
                }
            }
            structural.sort_unstable();
            structural.dedup();
            successors.push(block_successors);
            structural_successors.push(structural);
        }

        let predecessors = build_predecessors(&successors);
        let (flow_reachable, flow_dominance, flow_idom) = dominance(&successors, &predecessors);
        let structural_predecessors = build_predecessors(&structural_successors);
        let (reachable, dominance, idom) =
            dominance(&structural_successors, &structural_predecessors);
        Ok(Self {
            labels,
            successors,
            structural_successors,
            predecessors,
            flow_reachable,
            flow_dominance,
            flow_idom,
            reachable,
            dominance,
            idom,
            constructs,
            switches,
        })
    }

    fn block_label(&self, block: usize) -> String {
        self.labels
            .iter()
            .find_map(|(label, index)| (*index == block).then_some(format!("%{label}")))
            .unwrap_or_else(|| format!("block {block}"))
    }

    fn dominates(&self, dominator: usize, node: usize) -> bool {
        dominates_interval(&self.dominance, dominator, node)
    }

    fn flow_dominates(&self, dominator: usize, node: usize) -> bool {
        dominates_interval(&self.flow_dominance, dominator, node)
    }

    fn contains(&self, construct: Construct, block: usize) -> bool {
        if !self.dominates(construct.header, block) || self.dominates(construct.merge, block) {
            return false;
        }
        match construct.kind {
            MergeKind::Selection => true,
            MergeKind::Loop { continue_target } => !self.dominates(continue_target, block),
        }
    }

    fn check(
        &self,
        function: &Function,
        definitions: &HashMap<Word, &Instruction>,
        value_types: &HashMap<Word, Word>,
    ) -> Result<(), String> {
        self.check_block_layout(function, definitions, value_types)?;
        self.check_structure(function)
    }

    fn check_structure(&self, function: &Function) -> Result<(), String> {
        if !dominators_precede_blocks(&self.flow_reachable, &self.flow_idom) {
            return Err(
                "native emitter: owned block is serialized before one of its dominators"
                    .to_string(),
            );
        }
        for construct in &self.constructs {
            if !self.reachable[construct.header] {
                continue;
            }
            if !self.dominates(construct.header, construct.merge) {
                return Err(
                    "native emitter: owned construct header does not structurally dominate its merge"
                        .to_string(),
                );
            }
            if let MergeKind::Loop { continue_target } = construct.kind {
                if continue_target == construct.merge {
                    return Err(
                        "native emitter: owned loop continue target is its merge block".to_string(),
                    );
                }
                if !self.dominates(construct.header, continue_target) {
                    return Err(
                        "native emitter: owned loop header does not structurally dominate its continue target"
                            .to_string(),
                    );
                }
            }
            let members = (0..self.successors.len())
                .filter(|block| self.contains(*construct, *block))
                .collect::<HashSet<_>>();
            for &block in &members {
                if block != construct.header
                    && self.predecessors[block].iter().any(|predecessor| {
                        self.reachable[*predecessor] && !members.contains(predecessor)
                    })
                {
                    return Err(
                        "native emitter: owned edge enters a construct outside its header"
                            .to_string(),
                    );
                }
                for successor in &self.successors[block] {
                    if members.contains(successor)
                        || self.is_structured_exit(*construct, *successor)
                    {
                        continue;
                    }
                    return Err(
                        "native emitter: owned edge exits a construct without a structured target"
                            .to_string(),
                    );
                }
                for nested in &self.constructs {
                    if nested.header != block || nested.header == construct.header {
                        continue;
                    }
                    if self.reachable[nested.merge] && !members.contains(&nested.merge) {
                        return Err(
                            "native emitter: owned nested construct merge escapes its parent"
                                .to_string(),
                        );
                    }
                }
            }
        }
        self.check_switches()?;

        let loop_headers = self
            .constructs
            .iter()
            .filter(|construct| matches!(construct.kind, MergeKind::Loop { .. }))
            .map(|construct| construct.header)
            .collect::<HashSet<_>>();
        let mut backedges = HashMap::<usize, Vec<usize>>::default();
        for (source, targets) in self.successors.iter().enumerate() {
            if !self.reachable[source] {
                continue;
            }
            for target in targets {
                if self.dominates(*target, source) {
                    if !loop_headers.contains(target) {
                        return Err(
                            "native emitter: owned back-edge does not target a loop header"
                                .to_string(),
                        );
                    }
                    backedges.entry(*target).or_default().push(source);
                }
            }
        }
        if loop_headers.iter().any(|header| {
            self.reachable[*header]
                && backedges
                    .get(header)
                    .map(Vec::as_slice)
                    .is_none_or(|edges| edges.len() != 1)
        }) {
            return Err(
                "native emitter: owned loop does not have exactly one back-edge".to_string(),
            );
        }
        let post_dominance = needs_post_dominance(&self.constructs, &backedges)
            .then(|| post_dominance(&self.structural_successors, &self.successors));
        for construct in &self.constructs {
            let MergeKind::Loop { continue_target } = construct.kind else {
                continue;
            };
            let Some(backedge) = backedges
                .get(&construct.header)
                .and_then(|sources| sources.first())
            else {
                continue;
            };
            if !self.dominates(continue_target, *backedge) {
                return Err(
                    "native emitter: owned loop continue target does not structurally dominate its back-edge"
                        .to_string(),
                );
            }
            if *backedge != continue_target {
                let (reachable, dominance) = post_dominance
                    .as_ref()
                    .expect("distinct continue target requires post-dominance");
                if !structurally_post_dominates(reachable, dominance, *backedge, continue_target) {
                    return Err(
                        "native emitter: owned loop back-edge does not structurally post-dominate its continue target"
                        .to_string(),
                    );
                }
            }
            let single_block = construct.header == continue_target && continue_target == *backedge;
            if !single_block
                && self.predecessors[continue_target]
                    .iter()
                    .copied()
                    .any(|predecessor| {
                        self.reachable[predecessor]
                            && !self.dominates(continue_target, predecessor)
                            && !self.contains(*construct, predecessor)
                    })
            {
                return Err(
                    "native emitter: owned non-backedge continue entry originates outside its loop construct"
                        .to_string(),
                );
            }
            let continue_members = (0..self.successors.len())
                .map(|block| {
                    if continue_target == *backedge {
                        block == continue_target
                    } else {
                        let (reachable, dominance) = post_dominance
                            .as_ref()
                            .expect("distinct continue target requires post-dominance");
                        self.dominates(continue_target, block)
                            && structurally_post_dominates(reachable, dominance, *backedge, block)
                    }
                })
                .collect::<Vec<_>>();
            for (block, is_member) in continue_members.iter().copied().enumerate() {
                if !is_member {
                    continue;
                }
                if self.successors[block].iter().any(|successor| {
                    !continue_members[*successor]
                        && *successor != construct.header
                        && *successor != construct.merge
                }) {
                    return Err(
                        "native emitter: owned edge exits a continue construct without its loop header or merge"
                            .to_string(),
                    );
                }
            }
        }
        self.check_conditional_selections(function)?;
        Ok(())
    }

    fn check_block_layout(
        &self,
        function: &Function,
        definitions: &HashMap<Word, &Instruction>,
        value_types: &HashMap<Word, Word>,
    ) -> Result<(), String> {
        if self
            .predecessors
            .first()
            .is_some_and(|parents| !parents.is_empty())
        {
            return Err(
                "native emitter: owned function entry block is a branch target".to_string(),
            );
        }

        let definition_sites = definition_sites(function)?;
        let mut entry_value_seen = false;
        for (block_index, block) in function.blocks.iter().enumerate() {
            let mut non_phi_seen = false;
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let opcode = instruction.class.opcode;
                if is_block_terminator(opcode)
                    || matches!(opcode, Op::SelectionMerge | Op::LoopMerge)
                {
                    check_control_instruction_shape(instruction)?;
                }
                check_control_value_types(instruction, definitions, value_types)?;
                if is_block_terminator(opcode) && instruction_index + 1 != block.instructions.len()
                {
                    return Err(
                        "native emitter: owned block terminator is not its last instruction"
                            .to_string(),
                    );
                }
                match opcode {
                    Op::SelectionMerge => {
                        if instruction_index + 2 != block.instructions.len()
                            || !matches!(
                                block.instructions.last().map(|last| last.class.opcode),
                                Some(Op::BranchConditional | Op::Switch)
                            )
                        {
                            return Err(
                                "native emitter: owned selection merge does not immediately precede its branch"
                                    .to_string(),
                            );
                        }
                    }
                    Op::LoopMerge => {
                        if instruction_index + 2 != block.instructions.len()
                            || !matches!(
                                block.instructions.last().map(|last| last.class.opcode),
                                Some(Op::Branch | Op::BranchConditional)
                            )
                        {
                            return Err(
                                "native emitter: owned loop merge does not immediately precede its branch"
                                    .to_string(),
                            );
                        }
                    }
                    Op::Phi => {
                        if block_index == 0 {
                            return Err(
                                "native emitter: owned function entry block contains OpPhi"
                                    .to_string(),
                            );
                        }
                        if non_phi_seen {
                            return Err(
                                "native emitter: owned OpPhi follows a non-phi instruction"
                                    .to_string(),
                            );
                        }
                        self.check_phi(block_index, instruction, value_types, &definition_sites)?;
                    }
                    Op::Line | Op::NoLine => {}
                    _ => non_phi_seen = true,
                }

                if matches!(opcode, Op::Variable | Op::UntypedVariableKHR) {
                    if block_index != 0 || entry_value_seen {
                        return Err(
                            "native emitter: owned function variable is outside the entry prefix"
                                .to_string(),
                        );
                    }
                    if instruction.operands.first()
                        != Some(&Operand::StorageClass(spirv::StorageClass::Function))
                    {
                        return Err(
                            "native emitter: owned function variable has a non-Function storage class"
                                .to_string(),
                        );
                    }
                } else if block_index == 0 && !matches!(opcode, Op::Line | Op::NoLine) {
                    entry_value_seen = true;
                }
                if opcode != Op::Phi {
                    self.check_ordinary_uses(
                        block_index,
                        instruction_index,
                        instruction,
                        &definition_sites,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn check_phi(
        &self,
        block: usize,
        instruction: &Instruction,
        value_types: &HashMap<Word, Word>,
        definition_sites: &HashMap<Word, DefinitionSite>,
    ) -> Result<(), String> {
        let Some(result_type) = instruction.result_type else {
            return Err("native emitter: owned OpPhi has no result type".to_string());
        };
        if instruction.operands.is_empty()
            || !instruction.operands.chunks_exact(2).remainder().is_empty()
        {
            return Err("native emitter: owned OpPhi has malformed incoming pairs".to_string());
        }
        let mut parents = HashSet::default();
        for pair in instruction.operands.chunks_exact(2) {
            let (Operand::IdRef(value), Operand::IdRef(parent_label)) = (&pair[0], &pair[1]) else {
                return Err(
                    "native emitter: owned OpPhi has malformed incoming operands".to_string(),
                );
            };
            if value_types.get(value) != Some(&result_type) {
                return Err("native emitter: owned OpPhi incoming type does not match".to_string());
            }
            let Some(parent) = self.labels.get(parent_label).copied() else {
                return Err(
                    "native emitter: owned OpPhi names a parent outside its function".to_string(),
                );
            };
            if !parents.insert(parent) {
                return Err("native emitter: owned OpPhi repeats a parent block".to_string());
            }
            if self.flow_reachable[parent]
                && definition_sites
                    .get(value)
                    .is_some_and(|definition| !self.flow_dominates(definition.block, parent))
            {
                return Err(
                    "native emitter: owned OpPhi incoming value does not dominate its parent"
                        .to_string(),
                );
            }
        }
        let expected = self.predecessors[block]
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if parents != expected {
            return Err(
                "native emitter: owned OpPhi parents do not match block predecessors".to_string(),
            );
        }
        Ok(())
    }

    fn check_ordinary_uses(
        &self,
        block: usize,
        instruction_index: usize,
        instruction: &Instruction,
        definition_sites: &HashMap<Word, DefinitionSite>,
    ) -> Result<(), String> {
        for operand in &instruction.operands {
            let Operand::IdRef(id) = operand else {
                continue;
            };
            let Some(definition) = definition_sites.get(id) else {
                continue;
            };
            if definition.block == block {
                if definition.instruction >= instruction_index {
                    return Err(format!(
                        "native emitter: owned SSA value is used before its definition: {:?}{} \
                         reads %{id}, which block {} defines below it",
                        instruction.class.opcode,
                        describe_result(instruction),
                        self.block_label(block)
                    ));
                }
            } else if self.flow_reachable[block] && !self.flow_dominates(definition.block, block) {
                return Err(format!(
                    "native emitter: owned SSA definition does not dominate its use: {:?}{} in \
                     block {} reads %{id}, which is defined in block {}",
                    instruction.class.opcode,
                    describe_result(instruction),
                    self.block_label(block),
                    self.block_label(definition.block)
                ));
            }
        }
        Ok(())
    }

    fn check_switches(&self) -> Result<(), String> {
        for (&header, info) in &self.switches {
            let Some(construct) = self.constructs.iter().copied().find(|construct| {
                construct.header == header
                    && construct.is_switch
                    && construct.kind == MergeKind::Selection
            }) else {
                return Err("native emitter: owned OpSwitch has no selection merge".to_string());
            };
            if !self.reachable[header] {
                continue;
            }

            let mut case_headers = std::iter::once(info.default)
                .chain(info.targets.iter().copied())
                .filter(|case| *case != construct.merge)
                .collect::<Vec<_>>();
            case_headers.sort_unstable();
            case_headers.dedup();
            if case_headers
                .iter()
                .any(|case| !self.dominates(header, *case))
            {
                return Err(
                    "native emitter: owned switch header does not structurally dominate a case"
                        .to_string(),
                );
            }

            let case_owner =
                case_owners(&self.reachable, &self.idom, &case_headers, construct.merge);

            let outer_loop_roles = self
                .constructs
                .iter()
                .filter(|outer| {
                    outer.header != header
                        && matches!(outer.kind, MergeKind::Loop { .. })
                        && self.contains(**outer, header)
                })
                .flat_map(|outer| {
                    std::iter::once(outer.merge).chain(match outer.kind {
                        MergeKind::Loop { continue_target } => Some(continue_target),
                        MergeKind::Selection => None,
                    })
                })
                .collect::<HashSet<_>>();
            for (source, targets) in self.successors.iter().enumerate() {
                if case_owner[source].is_none() {
                    continue;
                }
                if targets.iter().any(|target| {
                    case_owner[*target].is_none()
                        && *target != construct.merge
                        && !outer_loop_roles.contains(target)
                }) {
                    return Err(
                        "native emitter: owned switch case exits to a non-structured target"
                            .to_string(),
                    );
                }
            }

            let mut closed_targets = HashSet::default();
            let mut previous = None;
            let mut positions = HashMap::<usize, (usize, usize)>::default();
            for (position, target) in info.targets.iter().copied().enumerate() {
                if previous.is_some_and(|previous| previous != target) {
                    closed_targets.insert(previous.expect("changed switch target exists"));
                    if closed_targets.contains(&target) {
                        return Err(
                            "native emitter: owned OpSwitch repeats a target nonconsecutively"
                                .to_string(),
                        );
                    }
                }
                positions
                    .entry(target)
                    .and_modify(|range| range.1 = position)
                    .or_insert((position, position));
                previous = Some(target);
            }

            let mut case_edges = HashSet::default();
            for (source, targets) in self.successors.iter().enumerate() {
                let Some(source_case) = case_owner[source] else {
                    continue;
                };
                for target in targets {
                    let Some(target_case) = case_owner[*target] else {
                        continue;
                    };
                    if source_case != target_case {
                        case_edges.insert((source_case, target_case));
                    }
                }
            }
            let mut outgoing = HashMap::<usize, HashSet<usize>>::default();
            let mut incoming = HashMap::<usize, HashSet<usize>>::default();
            for (source, target) in &case_edges {
                outgoing.entry(*source).or_default().insert(*target);
                incoming.entry(*target).or_default().insert(*source);
            }
            if outgoing.values().any(|targets| targets.len() > 1) {
                return Err(
                    "native emitter: owned switch case branches to multiple other cases"
                        .to_string(),
                );
            }
            if incoming.values().any(|sources| sources.len() > 1) {
                return Err(
                    "native emitter: owned switch case is entered by multiple other cases"
                        .to_string(),
                );
            }
            for (source, target) in &case_edges {
                let (Some((_, source_last)), Some((target_first, _))) =
                    (positions.get(source), positions.get(target))
                else {
                    continue;
                };
                if *source_last + 1 != *target_first {
                    return Err(
                        "native emitter: owned switch case fallthrough disagrees with target order"
                            .to_string(),
                    );
                }
            }
            if !positions.contains_key(&info.default) {
                let default_sources = incoming.get(&info.default).into_iter().flatten();
                let default_targets = outgoing.get(&info.default).into_iter().flatten();
                for source in default_sources {
                    for target in default_targets.clone() {
                        let (Some((_, source_last)), Some((target_first, _))) =
                            (positions.get(source), positions.get(target))
                        else {
                            continue;
                        };
                        if *source_last + 1 != *target_first {
                            return Err(
                                "native emitter: owned default-case bridge disagrees with target order"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn check_conditional_selections(&self, function: &Function) -> Result<(), String> {
        let declared_roles = self
            .constructs
            .iter()
            .flat_map(|construct| {
                std::iter::once(construct.merge).chain(match construct.kind {
                    MergeKind::Loop { continue_target } => Some(continue_target),
                    MergeKind::Selection => None,
                })
            })
            .collect::<HashSet<_>>();
        for block in &function.blocks {
            let Some(terminator) = block.instructions.last() else {
                continue;
            };
            if terminator.class.opcode != Op::BranchConditional {
                continue;
            }
            if !matches!(terminator.operands.len(), 3 | 5) {
                return Err(
                    "native emitter: owned OpBranchConditional has malformed operands".to_string(),
                );
            }
            let true_target = operand_label(terminator.operands.get(1), &self.labels)?;
            let false_target = operand_label(terminator.operands.get(2), &self.labels)?;
            if true_target == false_target
                || declared_roles.contains(&true_target)
                || declared_roles.contains(&false_target)
            {
                continue;
            }
            let has_merge = block
                .instructions
                .iter()
                .rev()
                .nth(1)
                .is_some_and(|instruction| {
                    matches!(instruction.class.opcode, Op::SelectionMerge | Op::LoopMerge)
                });
            if !has_merge {
                return Err(
                    "native emitter: owned divergent conditional has no merge declaration"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    fn is_structured_exit(&self, construct: Construct, target: usize) -> bool {
        match construct.kind {
            MergeKind::Loop { continue_target } => {
                target == construct.merge || target == continue_target
            }
            MergeKind::Selection => {
                if target == construct.merge {
                    return true;
                }
                let mut enclosing = self
                    .constructs
                    .iter()
                    .copied()
                    .filter(|outer| {
                        outer.header != construct.header && self.contains(*outer, construct.header)
                    })
                    .collect::<Vec<_>>();
                enclosing.sort_by_key(|outer| {
                    let (start, end) = self.dominance[outer.header].unwrap_or((0, usize::MAX));
                    end.saturating_sub(start)
                });
                for outer in enclosing {
                    match outer.kind {
                        MergeKind::Loop { continue_target } => {
                            return target == outer.merge || target == continue_target;
                        }
                        MergeKind::Selection if outer.is_switch => {
                            if target == outer.merge {
                                return true;
                            }
                        }
                        MergeKind::Selection => {}
                    }
                }
                false
            }
        }
    }
}

fn operand_label(
    operand: Option<&Operand>,
    labels: &HashMap<Word, usize>,
) -> Result<usize, String> {
    let Some(Operand::IdRef(label)) = operand else {
        return Err("native emitter: owned construct has a malformed target".to_string());
    };
    labels
        .get(label)
        .copied()
        .ok_or_else(|| "native emitter: owned construct targets an undefined block".to_string())
}

fn has_no_results(instruction: &Instruction) -> bool {
    instruction.result_type.is_none() && instruction.result_id.is_none()
}

fn check_control_instruction_shape(instruction: &Instruction) -> Result<(), String> {
    if !has_no_results(instruction) {
        return Err("native emitter: owned control instruction has a result".to_string());
    }
    let valid = match instruction.class.opcode {
        Op::Branch => matches!(instruction.operands.as_slice(), [Operand::IdRef(_)]),
        Op::BranchConditional => matches!(
            instruction.operands.as_slice(),
            [Operand::IdRef(_), Operand::IdRef(_), Operand::IdRef(_)]
                | [
                    Operand::IdRef(_),
                    Operand::IdRef(_),
                    Operand::IdRef(_),
                    Operand::LiteralBit32(_),
                    Operand::LiteralBit32(_)
                ]
        ),
        Op::Switch => {
            matches!(
                instruction.operands.as_slice(),
                [Operand::IdRef(_), Operand::IdRef(_), rest @ ..]
                    if rest.len().is_multiple_of(2) && rest.chunks_exact(2).all(|pair| matches!(pair,
                        [Operand::LiteralBit32(_) | Operand::LiteralBit64(_), Operand::IdRef(_)]))
            )
        }
        Op::SelectionMerge => matches!(
            instruction.operands.as_slice(),
            [Operand::IdRef(_), Operand::SelectionControl(_)]
        ),
        Op::LoopMerge => match instruction.operands.as_slice() {
            [Operand::IdRef(_), Operand::IdRef(_), Operand::LoopControl(_)] => true,
            [Operand::IdRef(_), Operand::IdRef(_), Operand::LoopControl(control), Operand::LiteralBit32(_)] => {
                *control == spirv::LoopControl::PARTIAL_COUNT
            }
            _ => false,
        },
        Op::Return
        | Op::Kill
        | Op::Unreachable
        | Op::TerminateInvocation
        | Op::TerminateRayKHR
        | Op::IgnoreIntersectionKHR => instruction.operands.is_empty(),
        Op::ReturnValue => matches!(instruction.operands.as_slice(), [Operand::IdRef(_)]),
        Op::EmitMeshTasksEXT => matches!(
            instruction.operands.as_slice(),
            [Operand::IdRef(_), Operand::IdRef(_), Operand::IdRef(_)]
                | [
                    Operand::IdRef(_),
                    Operand::IdRef(_),
                    Operand::IdRef(_),
                    Operand::IdRef(_)
                ]
        ),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "native emitter: owned {:?} has malformed operands",
            instruction.class.opcode
        ))
    }
}

fn check_control_value_types(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Result<(), String> {
    match instruction.class.opcode {
        Op::BranchConditional => {
            let Some(Operand::IdRef(condition)) = instruction.operands.first() else {
                return Ok(());
            };
            let is_bool = value_types
                .get(condition)
                .and_then(|type_id| definitions.get(type_id))
                .is_some_and(|definition| definition.class.opcode == Op::TypeBool);
            if !is_bool {
                return Err(
                    "native emitter: owned branch condition is not a boolean scalar".to_string(),
                );
            }
        }
        Op::Switch => {
            let Some(Operand::IdRef(selector)) = instruction.operands.first() else {
                return Ok(());
            };
            let width = value_types
                .get(selector)
                .and_then(|type_id| definitions.get(type_id))
                .and_then(|definition| {
                    if definition.class.opcode != Op::TypeInt {
                        return None;
                    }
                    match definition.operands.first() {
                        Some(Operand::LiteralBit32(width)) => Some(*width),
                        _ => None,
                    }
                });
            let literals_match = match width {
                Some(1..=32) => instruction.operands[2..]
                    .iter()
                    .step_by(2)
                    .all(|operand| matches!(operand, Operand::LiteralBit32(_))),
                Some(64) => instruction.operands[2..]
                    .iter()
                    .step_by(2)
                    .all(|operand| matches!(operand, Operand::LiteralBit64(_))),
                _ => false,
            };
            if !literals_match {
                return Err(
                    "native emitter: owned switch selector or literals have incompatible types"
                        .to_string(),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

fn successor_indices(
    opcode: Op,
    operands: &[Operand],
    labels: &HashMap<Word, usize>,
) -> Result<Vec<usize>, String> {
    let target_operands = match opcode {
        Op::Branch => operands.iter().take(1).collect::<Vec<_>>(),
        Op::BranchConditional => operands.iter().skip(1).take(2).collect(),
        Op::Switch => operands
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(index, _)| index % 2 == 1)
            .map(|(_, operand)| operand)
            .collect(),
        Op::Return
        | Op::ReturnValue
        | Op::Kill
        | Op::Unreachable
        | Op::TerminateInvocation
        | Op::TerminateRayKHR
        | Op::IgnoreIntersectionKHR
        | Op::EmitMeshTasksEXT => Vec::new(),
        _ => {
            return Err("native emitter: owned CFG block has an invalid terminator".to_string());
        }
    };
    let mut successors = target_operands
        .into_iter()
        .map(|operand| operand_label(Some(operand), labels))
        .collect::<Result<Vec<_>, _>>()?;
    successors.sort_unstable();
    successors.dedup();
    Ok(successors)
}

fn switch_info(operands: &[Operand], labels: &HashMap<Word, usize>) -> Result<SwitchInfo, String> {
    if operands.len() < 2 || !(operands.len() - 2).is_multiple_of(2) {
        return Err("native emitter: owned OpSwitch has malformed operands".to_string());
    }
    let default = operand_label(operands.get(1), labels)?;
    let targets = operands[2..]
        .chunks_exact(2)
        .map(|pair| operand_label(pair.get(1), labels))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SwitchInfo { default, targets })
}

fn definition_sites(function: &Function) -> Result<HashMap<Word, DefinitionSite>, String> {
    let mut sites = HashMap::default();
    for (block, instructions) in function.blocks.iter().enumerate() {
        for (instruction, definition) in instructions.instructions.iter().enumerate() {
            let Some(id) = definition.result_id else {
                continue;
            };
            if sites
                .insert(id, DefinitionSite { block, instruction })
                .is_some()
            {
                return Err(
                    "native emitter: owned function defines a result id more than once".to_string(),
                );
            }
        }
    }
    Ok(sites)
}

pub(super) fn rematerialize_nondominating_pure_uses(
    function: &mut Function,
    bound: &mut Word,
) -> usize {
    let Ok(cfg) = OwnedCfg::new(function) else {
        return 0;
    };
    let Ok(sites) = definition_sites(function) else {
        return 0;
    };
    let definitions: HashMap<Word, (usize, Instruction)> = sites
        .iter()
        .map(|(&id, site)| {
            let definition = function.blocks[site.block].instructions[site.instruction].clone();
            (id, (site.block, definition))
        })
        .collect();
    let mut total = 0;
    for block in 0..function.blocks.len() {
        if !cfg.flow_reachable[block] {
            continue;
        }
        let mut local = HashMap::<Word, Word>::default();
        let mut index = 0;
        while index < function.blocks[block].instructions.len() {
            if function.blocks[block].instructions[index].class.opcode == Op::Phi {
                index += 1;
                continue;
            }
            let reads = function.blocks[block].instructions[index]
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let mut emitted = Vec::<(Word, Instruction)>::new();
            for id in reads {
                remat_available(
                    id,
                    block,
                    &cfg,
                    &definitions,
                    &mut local,
                    &mut emitted,
                    bound,
                    0,
                );
            }
            let count = emitted.len();
            let at = if index > 0
                && matches!(
                    function.blocks[block].instructions[index - 1].class.opcode,
                    Op::SelectionMerge | Op::LoopMerge
                ) {
                index - 1
            } else {
                index
            };
            for (offset, (_, instruction)) in emitted.into_iter().enumerate() {
                function.blocks[block]
                    .instructions
                    .insert(at + offset, instruction);
            }
            index += count;
            total += count;
            for operand in &mut function.blocks[block].instructions[index].operands {
                if let Operand::IdRef(id) = operand {
                    if let Some(&copy) = local.get(id) {
                        *id = copy;
                    }
                }
            }
            index += 1;
        }
    }
    total
}

#[allow(clippy::too_many_arguments)]
fn remat_available(
    id: Word,
    block: usize,
    cfg: &OwnedCfg,
    definitions: &HashMap<Word, (usize, Instruction)>,
    local: &mut HashMap<Word, Word>,
    emitted: &mut Vec<(Word, Instruction)>,
    bound: &mut Word,
    depth: usize,
) -> bool {
    if local.contains_key(&id) {
        return true;
    }
    let Some((definer, definition)) = definitions.get(&id) else {
        return true;
    };
    if *definer == block || cfg.flow_dominates(*definer, block) {
        return true;
    }
    let pure = matches!(
        definition.class.opcode,
        Op::AccessChain
            | Op::InBoundsAccessChain
            | Op::PtrAccessChain
            | Op::InBoundsPtrAccessChain
            | Op::CopyObject
    );
    if !pure || depth >= 8 || definition.result_id.is_none() {
        return false;
    }
    let mark = emitted.len();
    for operand in &definition.operands {
        if let Operand::IdRef(read) = operand {
            if !remat_available(
                *read,
                block,
                cfg,
                definitions,
                local,
                emitted,
                bound,
                depth + 1,
            ) {
                for (origin, _) in emitted.drain(mark..) {
                    local.remove(&origin);
                }
                return false;
            }
        }
    }
    let mut copy = definition.clone();
    copy.result_id = Some(*bound);
    *bound += 1;
    for operand in &mut copy.operands {
        if let Operand::IdRef(read) = operand {
            if let Some(&renamed) = local.get(read) {
                *read = renamed;
            }
        }
    }
    local.insert(id, copy.result_id.unwrap_or_default());
    emitted.push((id, copy));
    true
}

pub(super) fn owned_function_construction_error(
    function: &Function,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let cfg = match OwnedCfg::new(function) {
        Ok(cfg) => cfg,
        Err(error) => return Some(error),
    };
    let sites = match definition_sites(function) {
        Ok(sites) => sites,
        Err(error) => return Some(error),
    };
    for (block, instructions) in function.blocks.iter().enumerate() {
        for (index, instruction) in instructions.instructions.iter().enumerate() {
            let result = if instruction.class.opcode == Op::Phi {
                cfg.check_phi(block, instruction, value_types, &sites)
            } else {
                cfg.check_ordinary_uses(block, index, instruction, &sites)
            };
            if let Err(error) = result {
                return Some(error);
            }
        }
    }
    cfg.check_structure(function).err()
}

fn post_dominance(
    structural_successors: &[Vec<usize>],
    branch_successors: &[Vec<usize>],
) -> (Vec<bool>, Vec<Option<(usize, usize)>>) {
    let mut reverse_successors = vec![Vec::new(); structural_successors.len() + 1];
    for (source, targets) in structural_successors.iter().enumerate() {
        for target in targets {
            reverse_successors[target + 1].push(source + 1);
        }
        if branch_successors[source].is_empty() {
            reverse_successors[0].push(source + 1);
        }
    }
    let reverse_predecessors = build_predecessors(&reverse_successors);
    let (reachable, dominance, _) = dominance(&reverse_successors, &reverse_predecessors);
    (reachable, dominance)
}

fn needs_post_dominance(constructs: &[Construct], backedges: &HashMap<usize, Vec<usize>>) -> bool {
    constructs.iter().any(|construct| {
        let MergeKind::Loop { continue_target } = construct.kind else {
            return false;
        };
        backedges
            .get(&construct.header)
            .and_then(|sources| sources.first())
            .is_some_and(|backedge| *backedge != continue_target)
    })
}

fn case_owners(
    reachable: &[bool],
    idom: &[Option<usize>],
    case_headers: &[usize],
    merge: usize,
) -> Vec<Option<usize>> {
    if reachable.is_empty() {
        return Vec::new();
    }
    let case_headers = case_headers.iter().copied().collect::<HashSet<_>>();
    let mut owners = vec![None; reachable.len()];
    let mut resolved = vec![false; reachable.len()];
    resolved[0] = true;
    if case_headers.contains(&0) && merge != 0 {
        owners[0] = Some(0);
    }
    for node in 1..reachable.len() {
        if !reachable[node] || resolved[node] {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor = node;
        while !resolved[cursor] {
            path.push(cursor);
            let Some(parent) = idom[cursor] else {
                break;
            };
            cursor = parent;
        }
        let mut owner = owners[cursor];
        while let Some(block) = path.pop() {
            owner = if block == merge {
                None
            } else if case_headers.contains(&block) {
                Some(block)
            } else {
                owner
            };
            owners[block] = owner;
            resolved[block] = true;
        }
    }
    owners
}

fn structurally_post_dominates(
    post_reachable: &[bool],
    post_dominance: &[Option<(usize, usize)>],
    post_dominator: usize,
    node: usize,
) -> bool {
    if post_dominator == node {
        return true;
    }
    let node = node + 1;
    if !post_reachable[node] {
        return true;
    }
    dominates_interval(post_dominance, post_dominator + 1, node)
}

fn dominators_precede_blocks(reachable: &[bool], idom: &[Option<usize>]) -> bool {
    if reachable.is_empty() {
        return true;
    }
    let mut max_dominator = vec![None; reachable.len()];
    max_dominator[0] = Some(0);
    for node in 1..reachable.len() {
        if !reachable[node] || max_dominator[node].is_some() {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor = node;
        while max_dominator[cursor].is_none() {
            path.push(cursor);
            let Some(parent) = idom[cursor] else {
                return false;
            };
            cursor = parent;
        }
        let mut maximum = max_dominator[cursor].expect("known dominator prefix");
        while let Some(block) = path.pop() {
            maximum = maximum.max(block);
            max_dominator[block] = Some(maximum);
        }
    }
    (1..reachable.len()).all(|block| {
        if !reachable[block] {
            return true;
        }
        let parent = idom[block].expect("reachable non-entry block has an immediate dominator");
        max_dominator[parent].is_some_and(|maximum| maximum < block)
    })
}

fn referenced_ids(instruction: &Instruction) -> impl Iterator<Item = Word> + '_ {
    instruction
        .result_type
        .iter()
        .copied()
        .chain(
            instruction
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => {
                        Some(*id)
                    }
                    _ => None,
                }),
        )
}

fn owned_type_operand_error(
    module: &crate::spirv_module::Module,
    definitions: &HashMap<Word, &Instruction>,
) -> Result<(), String> {
    let is_type = |operand: Option<&Operand>| {
        let Some(Operand::IdRef(id)) = operand else {
            return false;
        };
        definitions
            .get(id)
            .is_some_and(|definition| definition.class.opcode.is_type())
    };
    let operand_definition = |operand: Option<&Operand>| {
        let Some(Operand::IdRef(id)) = operand else {
            return None;
        };
        definitions.get(id).copied()
    };

    for instruction in module.all_inst_iter() {
        let type_operands_are_types = match instruction.class.opcode {
            Op::TypeVector
            | Op::TypeMatrix
            | Op::TypeImage
            | Op::TypeSampledImage
            | Op::TypeArray
            | Op::TypeRuntimeArray => is_type(instruction.operands.first()),
            Op::TypePointer => is_type(instruction.operands.get(1)),
            Op::TypeStruct | Op::TypeFunction => instruction
                .operands
                .iter()
                .all(|operand| is_type(Some(operand))),
            _ => true,
        };
        if !type_operands_are_types {
            return Err(format!(
                "native emitter: owned {:?} type operand is not a type declaration",
                instruction.class.opcode
            ));
        }

        match instruction.class.opcode {
            Op::TypeVector => {
                let component = operand_definition(instruction.operands.first())
                    .expect("type operand checked above");
                if !matches!(
                    component.class.opcode,
                    Op::TypeBool | Op::TypeInt | Op::TypeFloat
                ) {
                    return Err(
                        "native emitter: owned OpTypeVector component is not a scalar type"
                            .to_string(),
                    );
                }
            }
            Op::TypeMatrix => {
                let column = operand_definition(instruction.operands.first())
                    .expect("type operand checked above");
                let Some(component) = column
                    .operands
                    .first()
                    .and_then(|operand| operand_definition(Some(operand)))
                else {
                    return Err(
                        "native emitter: owned OpTypeMatrix column is not a float vector"
                            .to_string(),
                    );
                };
                if column.class.opcode != Op::TypeVector
                    || component.class.opcode != Op::TypeFloat
                    || !matches!(
                        instruction.operands.get(1),
                        Some(Operand::LiteralBit32(2..=4))
                    )
                {
                    return Err(
                        "native emitter: owned OpTypeMatrix column is not a 2-4 column float vector"
                            .to_string(),
                    );
                }
            }
            Op::TypeImage => {
                let sampled = operand_definition(instruction.operands.first())
                    .expect("type operand checked above");
                if !matches!(sampled.class.opcode, Op::TypeInt | Op::TypeFloat) {
                    return Err(
                        "native emitter: owned OpTypeImage sampled type is not scalar numeric"
                            .to_string(),
                    );
                }
            }
            Op::TypeSampledImage => {
                let image = operand_definition(instruction.operands.first())
                    .expect("type operand checked above");
                if image.class.opcode != Op::TypeImage {
                    return Err(
                        "native emitter: owned OpTypeSampledImage operand is not OpTypeImage"
                            .to_string(),
                    );
                }
            }
            Op::TypeArray => {
                let Some(length) = operand_definition(instruction.operands.get(1)) else {
                    return Err(
                        "native emitter: owned OpTypeArray length is not an integer constant"
                            .to_string(),
                    );
                };
                let length_is_integer = length
                    .result_type
                    .and_then(|result_type| definitions.get(&result_type))
                    .is_some_and(|ty| ty.class.opcode == Op::TypeInt);
                if !length.class.opcode.is_constant() || !length_is_integer {
                    return Err(
                        "native emitter: owned OpTypeArray length is not an integer constant"
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn owned_annotation_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
) -> Option<String> {
    let target_definition = |operand: Option<&Operand>| {
        let Some(Operand::IdRef(target)) = operand else {
            unreachable!("annotation operand grammar checked before target contracts");
        };
        definitions
            .get(target)
            .copied()
            .expect("annotation target existence checked before target contracts")
    };

    match instruction.class.opcode {
        Op::MemberDecorate => {
            let target = target_definition(instruction.operands.first());
            if target.class.opcode != Op::TypeStruct {
                return Some(
                    "native emitter: owned OpMemberDecorate target is not a structure type"
                        .to_string(),
                );
            }
            let Some(Operand::LiteralBit32(member)) = instruction.operands.get(1) else {
                unreachable!("member-decoration operand grammar checked before target contracts");
            };
            if (*member as usize) >= target.operands.len() {
                return Some(
                    "native emitter: owned OpMemberDecorate member index is out of bounds"
                        .to_string(),
                );
            }
            if instruction.operands.get(2)
                == Some(&Operand::Decoration(spirv::Decoration::MatrixStride))
                && instruction.operands.get(3) == Some(&Operand::LiteralBit32(0))
            {
                return Some(
                    "native emitter: owned MatrixStride decoration has zero stride".to_string(),
                );
            }
        }
        Op::Decorate => {
            let target = target_definition(instruction.operands.first());
            let Some(Operand::Decoration(decoration)) = instruction.operands.get(1) else {
                unreachable!("decoration operand grammar checked before target contracts");
            };
            let target_opcode = target.class.opcode;
            if *decoration == spirv::Decoration::ArrayStride
                && instruction.operands.get(2) == Some(&Operand::LiteralBit32(0))
            {
                return Some(
                    "native emitter: owned ArrayStride decoration has zero stride".to_string(),
                );
            }
            let error = match decoration {
                spirv::Decoration::ArrayStride
                    if !(matches!(target_opcode, Op::TypeArray | Op::TypeRuntimeArray)
                        || target_opcode == Op::TypePointer
                            && matches!(
                                target.operands.first(),
                                Some(Operand::StorageClass(
                                    spirv::StorageClass::StorageBuffer
                                        | spirv::StorageClass::PhysicalStorageBuffer
                                ))
                            )) =>
                {
                    Some(
                        "native emitter: owned ArrayStride target is not an array or stridable buffer pointer type",
                    )
                }
                spirv::Decoration::Block | spirv::Decoration::BufferBlock
                    if target_opcode != Op::TypeStruct =>
                {
                    Some("native emitter: owned block decoration target is not a structure type")
                }
                spirv::Decoration::SpecId
                    if !matches!(
                        target_opcode,
                        Op::SpecConstantTrue
                            | Op::SpecConstantFalse
                            | Op::SpecConstant
                            | Op::SpecConstantComposite
                            | Op::SpecConstantOp
                    ) =>
                {
                    Some("native emitter: owned SpecId target is not a specialization constant")
                }
                spirv::Decoration::DescriptorSet
                | spirv::Decoration::Binding
                | spirv::Decoration::InputAttachmentIndex
                | spirv::Decoration::Location
                | spirv::Decoration::Flat
                | spirv::Decoration::Patch
                    if target_opcode != Op::Variable =>
                {
                    Some("native emitter: owned interface decoration target is not a variable")
                }
                spirv::Decoration::BuiltIn => {
                    let workgroup_size_constant = instruction.operands.get(2)
                        == Some(&Operand::BuiltIn(spirv::BuiltIn::WorkgroupSize))
                        && matches!(
                            target_opcode,
                            Op::ConstantComposite | Op::SpecConstantComposite
                        );
                    if target_opcode == Op::Variable || workgroup_size_constant {
                        None
                    } else {
                        Some(
                            "native emitter: owned BuiltIn target does not match its built-in contract",
                        )
                    }
                }
                spirv::Decoration::Offset => {
                    Some("native emitter: owned Offset is not a member decoration")
                }
                _ => None,
            };
            if let Some(error) = error {
                return Some(error.to_string());
            }
        }
        _ => {}
    }
    None
}

fn owned_block_layout_error(
    module: &crate::spirv_module::Module,
    definitions: &HashMap<Word, &Instruction>,
) -> Option<String> {
    let mut block_roots = HashSet::default();
    let mut array_strides = HashSet::default();
    let mut member_offsets = HashSet::default();
    let mut matrix_strides = HashSet::default();
    let mut row_major = HashSet::default();
    let mut col_major = HashSet::default();
    for annotation in &module.annotations {
        match annotation.operands.as_slice() {
            [Operand::IdRef(target), Operand::Decoration(spirv::Decoration::Block | spirv::Decoration::BufferBlock)]
                if annotation.class.opcode == Op::Decorate =>
            {
                block_roots.insert(*target);
            }
            [Operand::IdRef(target), Operand::Decoration(spirv::Decoration::ArrayStride), Operand::LiteralBit32(_)]
                if annotation.class.opcode == Op::Decorate =>
            {
                array_strides.insert(*target);
            }
            [Operand::IdRef(target), Operand::LiteralBit32(member), Operand::Decoration(decoration), ..]
                if annotation.class.opcode == Op::MemberDecorate =>
            {
                let key = (*target, *member);
                match decoration {
                    spirv::Decoration::Offset => {
                        member_offsets.insert(key);
                    }
                    spirv::Decoration::MatrixStride => {
                        matrix_strides.insert(key);
                    }
                    spirv::Decoration::RowMajor => {
                        row_major.insert(key);
                    }
                    spirv::Decoration::ColMajor => {
                        col_major.insert(key);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn is_matrix_or_array_of_matrices(ty: Word, definitions: &HashMap<Word, &Instruction>) -> bool {
        let Some(definition) = definitions.get(&ty) else {
            return false;
        };
        match definition.class.opcode {
            Op::TypeMatrix => true,
            Op::TypeArray | Op::TypeRuntimeArray => definition
                .operands
                .first()
                .and_then(|operand| match operand {
                    Operand::IdRef(element) => Some(*element),
                    _ => None,
                })
                .is_some_and(|element| is_matrix_or_array_of_matrices(element, definitions)),
            _ => false,
        }
    }

    struct LayoutDecorations<'a> {
        array_strides: &'a HashSet<Word>,
        member_offsets: &'a HashSet<(Word, u32)>,
        matrix_strides: &'a HashSet<(Word, u32)>,
        row_major: &'a HashSet<(Word, u32)>,
        col_major: &'a HashSet<(Word, u32)>,
    }

    fn check_type(
        ty: Word,
        definitions: &HashMap<Word, &Instruction>,
        decorations: &LayoutDecorations<'_>,
        checked: &mut HashSet<Word>,
    ) -> Option<String> {
        if !checked.insert(ty) {
            return None;
        }
        let definition = definitions
            .get(&ty)
            .copied()
            .expect("owned type graph references checked before Block layout");
        match definition.class.opcode {
            Op::TypeStruct => {
                for (member, operand) in definition.operands.iter().enumerate() {
                    let member = member as u32;
                    let key = (ty, member);
                    if !decorations.member_offsets.contains(&key) {
                        return Some(
                            "native emitter: owned Block layout structure member lacks Offset"
                                .to_string(),
                        );
                    }
                    let Operand::IdRef(member_type) = operand else {
                        unreachable!("structure member grammar checked before Block layout");
                    };
                    if is_matrix_or_array_of_matrices(*member_type, definitions) {
                        if !decorations.matrix_strides.contains(&key) {
                            return Some(
                                "native emitter: owned Block layout matrix member lacks MatrixStride"
                                    .to_string(),
                            );
                        }
                        if decorations.row_major.contains(&key)
                            == decorations.col_major.contains(&key)
                        {
                            return Some(
                                "native emitter: owned Block layout matrix member does not have exactly one major order"
                                    .to_string(),
                            );
                        }
                    }
                    if let Some(error) = check_type(*member_type, definitions, decorations, checked)
                    {
                        return Some(error);
                    }
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray => {
                if !decorations.array_strides.contains(&ty) {
                    return Some(
                        "native emitter: owned Block layout array lacks ArrayStride".to_string(),
                    );
                }
                let Some(Operand::IdRef(element)) = definition.operands.first() else {
                    unreachable!("array operand grammar checked before Block layout");
                };
                return check_type(*element, definitions, decorations, checked);
            }
            _ => {}
        }
        None
    }

    let decorations = LayoutDecorations {
        array_strides: &array_strides,
        member_offsets: &member_offsets,
        matrix_strides: &matrix_strides,
        row_major: &row_major,
        col_major: &col_major,
    };
    let mut checked = HashSet::default();
    block_roots
        .into_iter()
        .find_map(|root| check_type(root, definitions, &decorations, &mut checked))
}

fn owned_function_contract_error(
    function: &Function,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Result<(), String> {
    let Some(definition) = function.def.as_ref() else {
        return Err("native emitter: owned function has no OpFunction".to_string());
    };
    let (Some(return_type), Some(_function_id)) = (definition.result_type, definition.result_id)
    else {
        return Err("native emitter: owned OpFunction has malformed results".to_string());
    };
    let [Operand::FunctionControl(_), Operand::IdRef(function_type)] =
        definition.operands.as_slice()
    else {
        return Err("native emitter: owned OpFunction has malformed operands".to_string());
    };
    if definition.class.opcode != Op::Function {
        return Err("native emitter: owned function definition is not OpFunction".to_string());
    }
    let Some(type_definition) = definitions.get(function_type) else {
        return Err("native emitter: owned function type is undefined".to_string());
    };
    if type_definition.class.opcode != Op::TypeFunction
        || type_definition.result_type.is_some()
        || !type_definition
            .operands
            .iter()
            .all(|operand| matches!(operand, Operand::IdRef(_)))
    {
        return Err("native emitter: owned function type is malformed".to_string());
    }
    let Some(Operand::IdRef(signature_return_type)) = type_definition.operands.first() else {
        return Err("native emitter: owned function type has no return type".to_string());
    };
    if return_type != *signature_return_type {
        return Err(
            "native emitter: owned OpFunction return type disagrees with its function type"
                .to_string(),
        );
    }
    let parameter_types = &type_definition.operands[1..];
    if parameter_types.len() != function.parameters.len() {
        return Err(
            "native emitter: owned function parameter count disagrees with its function type"
                .to_string(),
        );
    }
    for (parameter, expected_type) in function.parameters.iter().zip(parameter_types) {
        let Operand::IdRef(expected_type) = expected_type else {
            unreachable!("function type operands were checked above");
        };
        if parameter.class.opcode != Op::FunctionParameter
            || parameter.result_type != Some(*expected_type)
            || parameter.result_id.is_none()
            || !parameter.operands.is_empty()
        {
            return Err(
                "native emitter: owned function parameter disagrees with its function type"
                    .to_string(),
            );
        }
    }
    let Some(end) = function.end.as_ref() else {
        return Err("native emitter: owned function has no OpFunctionEnd".to_string());
    };
    if end.class.opcode != Op::FunctionEnd
        || end.result_type.is_some()
        || end.result_id.is_some()
        || !end.operands.is_empty()
    {
        return Err("native emitter: owned function has malformed OpFunctionEnd".to_string());
    }

    let return_is_void = definitions
        .get(&return_type)
        .is_some_and(|instruction| instruction.class.opcode == Op::TypeVoid);
    for terminator in function
        .blocks
        .iter()
        .filter_map(|block| block.instructions.last())
    {
        match terminator.class.opcode {
            Op::Return if !return_is_void => {
                return Err(
                    "native emitter: owned OpReturn terminates a non-void function".to_string(),
                );
            }
            Op::ReturnValue if return_is_void => {
                return Err(
                    "native emitter: owned OpReturnValue terminates a void function".to_string(),
                );
            }
            Op::ReturnValue => {
                let [Operand::IdRef(value)] = terminator.operands.as_slice() else {
                    return Err(
                        "native emitter: owned OpReturnValue has malformed operands".to_string()
                    );
                };
                if value_types.get(value) != Some(&return_type) {
                    return Err(
                        "native emitter: owned return value disagrees with the function return type"
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn owned_function_call_contract_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Result<(), String> {
    if instruction.class.opcode != Op::FunctionCall {
        return Ok(());
    }
    let (Some(result_type), Some(_result_id)) = (instruction.result_type, instruction.result_id)
    else {
        return Err("native emitter: owned OpFunctionCall has malformed results".to_string());
    };
    let [Operand::IdRef(callee), arguments @ ..] = instruction.operands.as_slice() else {
        return Err("native emitter: owned OpFunctionCall has malformed operands".to_string());
    };
    if !arguments
        .iter()
        .all(|argument| matches!(argument, Operand::IdRef(_)))
    {
        return Err("native emitter: owned OpFunctionCall has malformed operands".to_string());
    }
    let Some(callee_definition) = definitions.get(callee) else {
        return Err("native emitter: owned OpFunctionCall callee is undefined".to_string());
    };
    let [Operand::FunctionControl(_), Operand::IdRef(function_type)] =
        callee_definition.operands.as_slice()
    else {
        return Err("native emitter: owned OpFunctionCall callee is malformed".to_string());
    };
    if callee_definition.class.opcode != Op::Function {
        return Err("native emitter: owned OpFunctionCall target is not a function".to_string());
    }
    let Some(type_definition) = definitions.get(function_type) else {
        return Err("native emitter: owned OpFunctionCall function type is undefined".to_string());
    };
    let Some((Operand::IdRef(return_type), parameter_types)) =
        type_definition.operands.split_first()
    else {
        return Err("native emitter: owned OpFunctionCall function type is malformed".to_string());
    };
    if type_definition.class.opcode != Op::TypeFunction
        || result_type != *return_type
        || parameter_types.len() != arguments.len()
    {
        return Err(
            "native emitter: owned OpFunctionCall disagrees with its function type".to_string(),
        );
    }
    for (position, (argument, parameter_type)) in arguments.iter().zip(parameter_types).enumerate()
    {
        let (Operand::IdRef(argument), Operand::IdRef(parameter_type)) = (argument, parameter_type)
        else {
            return Err("native emitter: owned OpFunctionCall has malformed operands".to_string());
        };
        if value_types.get(argument) != Some(parameter_type) {
            let described = match value_types.get(argument) {
                Some(argument_type) => format!(
                    "%{argument}, of type {}",
                    describe_owned_id(*argument_type, definitions)
                ),
                None => format!("%{argument}, which has no value type"),
            };
            return Err(format!(
                "native emitter: owned OpFunctionCall argument type disagrees with its function \
                 type: argument {position} is {described}, but the parameter type is {}",
                describe_owned_id(*parameter_type, definitions)
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SameTypeClass {
    Any,
    Bool,
    Integer,
    Float,
}

#[derive(Clone, Copy)]
enum ComparisonClass {
    Integer,
    Float,
}

#[derive(Clone, Copy)]
enum ConversionClass {
    UnsignedWidth,
    SignedWidth,
    FloatWidth,
    FloatToInteger,
    IntegerToFloat,
}

fn same_type_class(opcode: Op) -> Option<SameTypeClass> {
    match opcode {
        Op::CopyObject => Some(SameTypeClass::Any),
        Op::LogicalEqual
        | Op::LogicalNotEqual
        | Op::LogicalOr
        | Op::LogicalAnd
        | Op::LogicalNot => Some(SameTypeClass::Bool),
        Op::SNegate
        | Op::IAdd
        | Op::ISub
        | Op::IMul
        | Op::UDiv
        | Op::SDiv
        | Op::UMod
        | Op::SRem
        | Op::SMod
        | Op::BitwiseOr
        | Op::BitwiseXor
        | Op::BitwiseAnd
        | Op::Not
        | Op::BitReverse => Some(SameTypeClass::Integer),
        Op::FNegate | Op::FAdd | Op::FSub | Op::FMul | Op::FDiv | Op::FRem | Op::FMod => {
            Some(SameTypeClass::Float)
        }
        _ => None,
    }
}

fn comparison_class(opcode: Op) -> Option<ComparisonClass> {
    match opcode {
        Op::IEqual
        | Op::INotEqual
        | Op::UGreaterThan
        | Op::SGreaterThan
        | Op::UGreaterThanEqual
        | Op::SGreaterThanEqual
        | Op::ULessThan
        | Op::SLessThan
        | Op::ULessThanEqual
        | Op::SLessThanEqual => Some(ComparisonClass::Integer),
        Op::FOrdEqual
        | Op::FUnordEqual
        | Op::FOrdNotEqual
        | Op::FUnordNotEqual
        | Op::FOrdLessThan
        | Op::FUnordLessThan
        | Op::FOrdGreaterThan
        | Op::FUnordGreaterThan
        | Op::FOrdLessThanEqual
        | Op::FUnordLessThanEqual
        | Op::FOrdGreaterThanEqual
        | Op::FUnordGreaterThanEqual => Some(ComparisonClass::Float),
        _ => None,
    }
}

fn conversion_class(opcode: Op) -> Option<ConversionClass> {
    match opcode {
        Op::UConvert => Some(ConversionClass::UnsignedWidth),
        Op::SConvert => Some(ConversionClass::SignedWidth),
        Op::FConvert => Some(ConversionClass::FloatWidth),
        Op::ConvertFToU | Op::ConvertFToS => Some(ConversionClass::FloatToInteger),
        Op::ConvertSToF | Op::ConvertUToF => Some(ConversionClass::IntegerToFloat),
        _ => None,
    }
}

fn scalar_type_shape(ty: Word, definitions: &HashMap<Word, &Instruction>) -> Option<(Op, u32)> {
    let definition = definitions.get(&ty)?;
    match definition.class.opcode {
        Op::TypeBool | Op::TypeInt | Op::TypeFloat => Some((definition.class.opcode, 1)),
        Op::TypeVector => {
            let (component, lanes) = vector_type_shape(ty, definitions)?;
            definitions
                .get(&component)
                .map(|component| (component.class.opcode, lanes))
        }
        _ => None,
    }
}

fn numeric_type_shape(
    ty: Word,
    definitions: &HashMap<Word, &Instruction>,
) -> Option<(Op, u32, u32)> {
    let definition = definitions.get(&ty)?;
    match definition.class.opcode {
        Op::TypeInt | Op::TypeFloat => match definition.operands.first() {
            Some(Operand::LiteralBit32(width)) => Some((definition.class.opcode, *width, 1)),
            _ => None,
        },
        Op::TypeVector => {
            let (component, lanes) = vector_type_shape(ty, definitions)?;
            let (opcode, width, component_lanes) = numeric_type_shape(component, definitions)?;
            (component_lanes == 1).then_some((opcode, width, lanes))
        }
        _ => None,
    }
}

fn owned_constant_u32(
    id: Word,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<u32> {
    let definition = definitions.get(&id)?;
    let [Operand::LiteralBit32(value)] = definition.operands.as_slice() else {
        return None;
    };
    (definition.class.opcode == Op::Constant
        && value_types.get(&id).is_some_and(|ty| {
            matches!(
                numeric_type_shape(*ty, definitions),
                Some((Op::TypeInt, 32, 1))
            )
        }))
    .then_some(*value)
}

fn integer_type_signedness(ty: Word, definitions: &HashMap<Word, &Instruction>) -> Option<bool> {
    let component = scalar_component_type(ty, definitions)?;
    let definition = definitions.get(&component)?;
    let [Operand::LiteralBit32(_), Operand::LiteralBit32(signedness)] =
        definition.operands.as_slice()
    else {
        return None;
    };
    (definition.class.opcode == Op::TypeInt && matches!(*signedness, 0..=1))
        .then_some(*signedness == 1)
}

#[derive(Clone, Copy)]
struct ImageTypeShape {
    sampled_type: Word,
    dim: spirv::Dim,
    arrayed: bool,
    multisampled: bool,
    sampled: u32,
    format: spirv::ImageFormat,
}

fn image_type_shape(ty: Word, definitions: &HashMap<Word, &Instruction>) -> Option<ImageTypeShape> {
    let definition = definitions.get(&ty)?;
    let [Operand::IdRef(sampled_type), Operand::Dim(dim), Operand::LiteralBit32(depth), Operand::LiteralBit32(arrayed), Operand::LiteralBit32(multisampled), Operand::LiteralBit32(sampled), Operand::ImageFormat(format), ..] =
        definition.operands.as_slice()
    else {
        return None;
    };
    (definition.class.opcode == Op::TypeImage
        && matches!(*depth, 0..=2)
        && matches!(*arrayed, 0..=1)
        && matches!(*multisampled, 0..=1)
        && matches!(*sampled, 0..=2))
    .then_some(ImageTypeShape {
        sampled_type: *sampled_type,
        dim: *dim,
        arrayed: *arrayed == 1,
        multisampled: *multisampled == 1,
        sampled: *sampled,
        format: *format,
    })
}

fn image_operand_shape(
    operand: Option<&Operand>,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<ImageTypeShape> {
    let Some(Operand::IdRef(image)) = operand else {
        return None;
    };
    image_type_shape(*value_types.get(image)?, definitions)
}

fn sampled_image_operand_shape(
    operand: Option<&Operand>,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<ImageTypeShape> {
    let Some(Operand::IdRef(sampled_image)) = operand else {
        return None;
    };
    let sampled_type = definitions.get(value_types.get(sampled_image)?)?;
    let [Operand::IdRef(image_type)] = sampled_type.operands.as_slice() else {
        return None;
    };
    (sampled_type.class.opcode == Op::TypeSampledImage)
        .then(|| image_type_shape(*image_type, definitions))?
}

fn atomic_value_contract_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let (scalar_opcode, value_indices): (Op, &[usize]) = match instruction.class.opcode {
        Op::AtomicLoad | Op::AtomicIIncrement | Op::AtomicIDecrement => (Op::TypeInt, &[]),
        Op::AtomicStore
        | Op::AtomicExchange
        | Op::AtomicIAdd
        | Op::AtomicISub
        | Op::AtomicSMin
        | Op::AtomicUMin
        | Op::AtomicSMax
        | Op::AtomicUMax
        | Op::AtomicAnd
        | Op::AtomicOr
        | Op::AtomicXor => (Op::TypeInt, &[3]),
        Op::AtomicCompareExchange | Op::AtomicCompareExchangeWeak => (Op::TypeInt, &[4, 5]),
        Op::AtomicFAddEXT | Op::AtomicFMinEXT | Op::AtomicFMaxEXT => (Op::TypeFloat, &[3]),
        _ => return None,
    };
    let operand_id = |operand: &Operand| match operand {
        Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => Some(*id),
        _ => None,
    };
    let operand_type = |index| {
        instruction
            .operands
            .get(index)
            .and_then(operand_id)
            .and_then(|id| value_types.get(&id).copied())
    };
    let pointer_type = operand_type(0)?;
    let pointee = definitions.get(&pointer_type).and_then(|definition| {
        let [Operand::StorageClass(_), Operand::IdRef(pointee)] = definition.operands.as_slice()
        else {
            return None;
        };
        (definition.class.opcode == Op::TypePointer).then_some(*pointee)
    });
    let Some(pointee) = pointee else {
        return Some(format!(
            "native emitter: owned {:?} pointer operand is not a pointer",
            instruction.class.opcode
        ));
    };
    if !matches!(
        numeric_type_shape(pointee, definitions),
        Some((opcode, _, 1)) if opcode == scalar_opcode
    ) {
        return Some(format!(
            "native emitter: owned {:?} pointer does not target the required scalar type class",
            instruction.class.opcode
        ));
    }
    if instruction.class.opcode != Op::AtomicStore && instruction.result_type != Some(pointee) {
        return Some(format!(
            "native emitter: owned {:?} result type does not match its pointer pointee",
            instruction.class.opcode
        ));
    }
    if value_indices
        .iter()
        .any(|index| operand_type(*index) != Some(pointee))
    {
        return Some(format!(
            "native emitter: owned {:?} value operands do not match its pointer pointee",
            instruction.class.opcode
        ));
    }
    let semantics_indices: &[usize] = if matches!(
        instruction.class.opcode,
        Op::AtomicCompareExchange | Op::AtomicCompareExchangeWeak
    ) {
        &[2, 3]
    } else {
        &[2]
    };
    if std::iter::once(1)
        .chain(semantics_indices.iter().copied())
        .any(|index| {
            !matches!(
                operand_type(index).and_then(|ty| numeric_type_shape(ty, definitions)),
                Some((Op::TypeInt, 32, 1))
            )
        })
    {
        return Some(format!(
            "native emitter: owned {:?} scope and memory semantics are not 32-bit integer scalars",
            instruction.class.opcode
        ));
    }
    None
}

#[derive(Clone, Copy)]
enum GlslExtInstContract {
    FloatSame { arity: usize, width_16_or_32: bool },
    IntegerShapeSame { arity: usize, width_32: bool },
    Ldexp,
    Pack { lanes: u32 },
    Unpack { lanes: u32 },
}

fn glsl_ext_inst_contract(number: u32) -> Option<GlslExtInstContract> {
    use spirv::GlslStd450Op as Glsl;
    use GlslExtInstContract::{FloatSame, IntegerShapeSame, Ldexp, Pack, Unpack};

    match Glsl::from_u32(number)? {
        Glsl::Round
        | Glsl::RoundEven
        | Glsl::Trunc
        | Glsl::FAbs
        | Glsl::FSign
        | Glsl::Floor
        | Glsl::Ceil
        | Glsl::Fract => Some(FloatSame {
            arity: 1,
            width_16_or_32: false,
        }),
        Glsl::Sin
        | Glsl::Cos
        | Glsl::Tan
        | Glsl::Asin
        | Glsl::Acos
        | Glsl::Atan
        | Glsl::Sinh
        | Glsl::Cosh
        | Glsl::Tanh
        | Glsl::Asinh
        | Glsl::Acosh
        | Glsl::Atanh
        | Glsl::Exp
        | Glsl::Log
        | Glsl::Exp2
        | Glsl::Log2
        | Glsl::Sqrt
        | Glsl::InverseSqrt => Some(FloatSame {
            arity: 1,
            width_16_or_32: true,
        }),
        Glsl::Atan2 | Glsl::Pow => Some(FloatSame {
            arity: 2,
            width_16_or_32: true,
        }),
        Glsl::FMin | Glsl::FMax | Glsl::NMin | Glsl::NMax => Some(FloatSame {
            arity: 2,
            width_16_or_32: false,
        }),
        Glsl::FClamp | Glsl::FMix | Glsl::Fma | Glsl::NClamp => Some(FloatSame {
            arity: 3,
            width_16_or_32: false,
        }),
        Glsl::SAbs => Some(IntegerShapeSame {
            arity: 1,
            width_32: false,
        }),
        Glsl::UMin | Glsl::SMin | Glsl::UMax | Glsl::SMax => Some(IntegerShapeSame {
            arity: 2,
            width_32: false,
        }),
        Glsl::UClamp | Glsl::SClamp => Some(IntegerShapeSame {
            arity: 3,
            width_32: false,
        }),
        Glsl::Ldexp => Some(Ldexp),
        Glsl::PackSnorm4x8 | Glsl::PackUnorm4x8 => Some(Pack { lanes: 4 }),
        Glsl::PackSnorm2x16 | Glsl::PackUnorm2x16 | Glsl::PackHalf2x16 => Some(Pack { lanes: 2 }),
        Glsl::UnpackSnorm2x16 | Glsl::UnpackUnorm2x16 | Glsl::UnpackHalf2x16 => {
            Some(Unpack { lanes: 2 })
        }
        Glsl::UnpackSnorm4x8 | Glsl::UnpackUnorm4x8 => Some(Unpack { lanes: 4 }),
        Glsl::FindILsb => Some(IntegerShapeSame {
            arity: 1,
            width_32: true,
        }),
        _ => None,
    }
}

fn owned_glsl_ext_inst_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    if instruction.class.opcode != Op::ExtInst {
        return None;
    }
    let [Operand::IdRef(set), Operand::LiteralExtInstInteger(number), arguments @ ..] =
        instruction.operands.as_slice()
    else {
        return Some("native emitter: owned OpExtInst has malformed operands".to_string());
    };
    let import = definitions.get(set);
    if import.is_none_or(|definition| definition.class.opcode != Op::ExtInstImport) {
        return Some("native emitter: owned OpExtInst set is not an OpExtInstImport".to_string());
    }
    if !matches!(
        import.and_then(|definition| definition.operands.first()),
        Some(Operand::LiteralString(name)) if name == "GLSL.std.450"
    ) {
        return Some(
            "native emitter: owned OpExtInst uses an unsupported extended instruction set"
                .to_string(),
        );
    }
    let Some(contract) = glsl_ext_inst_contract(*number) else {
        return Some(
            "native emitter: owned GLSL.std.450 instruction is outside the emitted contract"
                .to_string(),
        );
    };
    let Some(result_type) = instruction.result_type else {
        return Some("native emitter: owned OpExtInst has no result type".to_string());
    };
    let argument_types = arguments
        .iter()
        .map(|operand| match operand {
            Operand::IdRef(id) => value_types.get(id).copied(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(argument_types) = argument_types else {
        return Some("native emitter: owned OpExtInst has an untyped argument".to_string());
    };
    let valid = match contract {
        GlslExtInstContract::FloatSame {
            arity,
            width_16_or_32,
        } => {
            arguments.len() == arity
                && argument_types.iter().all(|ty| *ty == result_type)
                && matches!(
                    numeric_type_shape(result_type, definitions),
                    Some((Op::TypeFloat, width, _))
                        if !width_16_or_32 || matches!(width, 16 | 32)
                )
        }
        GlslExtInstContract::IntegerShapeSame { arity, width_32 } => {
            let result_shape = numeric_type_shape(result_type, definitions);
            arguments.len() == arity
                && matches!(
                    result_shape,
                    Some((Op::TypeInt, width, _)) if !width_32 || width == 32
                )
                && argument_types
                    .iter()
                    .all(|ty| numeric_type_shape(*ty, definitions) == result_shape)
        }
        GlslExtInstContract::Ldexp => {
            arguments.len() == 2
                && argument_types.first() == Some(&result_type)
                && matches!(
                    numeric_type_shape(result_type, definitions),
                    Some((Op::TypeFloat, _, result_lanes))
                        if matches!(
                            numeric_type_shape(argument_types[1], definitions),
                            Some((Op::TypeInt, _, exponent_lanes))
                                if exponent_lanes == result_lanes
                        )
                )
        }
        GlslExtInstContract::Pack { lanes } => {
            arguments.len() == 1
                && matches!(
                    numeric_type_shape(result_type, definitions),
                    Some((Op::TypeInt, 32, 1))
                )
                && matches!(
                    numeric_type_shape(argument_types[0], definitions),
                    Some((Op::TypeFloat, 32, argument_lanes)) if argument_lanes == lanes
                )
        }
        GlslExtInstContract::Unpack { lanes } => {
            arguments.len() == 1
                && matches!(
                    numeric_type_shape(argument_types[0], definitions),
                    Some((Op::TypeInt, 32, 1))
                )
                && matches!(
                    numeric_type_shape(result_type, definitions),
                    Some((Op::TypeFloat, 32, result_lanes)) if result_lanes == lanes
                )
        }
    };
    (!valid).then(|| {
        format!(
            "native emitter: owned GLSL.std.450 instruction {number} violates its operand and result contract"
        )
    })
}

fn owned_sampled_image_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let operand_type = |operand: Option<&Operand>| {
        let Some(Operand::IdRef(value)) = operand else {
            return None;
        };
        value_types.get(value).copied()
    };
    let type_operand = |ty: Word, opcode: Op| {
        let definition = definitions.get(&ty)?;
        (definition.class.opcode == opcode).then(|| match definition.operands.first() {
            Some(Operand::IdRef(operand)) => Some(*operand),
            _ => None,
        })?
    };

    let valid = match instruction.class.opcode {
        Op::SampledImage => {
            let [image, sampler] = instruction.operands.as_slice() else {
                return Some(
                    "native emitter: owned OpSampledImage has malformed operands".to_string(),
                );
            };
            let Some(result_type) = instruction.result_type else {
                return Some("native emitter: owned OpSampledImage has no result type".to_string());
            };
            let image_type = type_operand(result_type, Op::TypeSampledImage);
            image_type.is_some()
                && operand_type(Some(image)) == image_type
                && operand_type(Some(sampler)).is_some_and(|sampler_type| {
                    definitions
                        .get(&sampler_type)
                        .is_some_and(|definition| definition.class.opcode == Op::TypeSampler)
                })
        }
        Op::Image => {
            let [sampled_image] = instruction.operands.as_slice() else {
                return Some("native emitter: owned OpImage has malformed operands".to_string());
            };
            let Some(result_type) = instruction.result_type else {
                return Some("native emitter: owned OpImage has no result type".to_string());
            };
            definitions
                .get(&result_type)
                .is_some_and(|definition| definition.class.opcode == Op::TypeImage)
                && operand_type(Some(sampled_image))
                    .and_then(|sampled_type| type_operand(sampled_type, Op::TypeSampledImage))
                    == Some(result_type)
        }
        _ => return None,
    };
    (!valid).then(|| {
        format!(
            "native emitter: owned {:?} image, sampler, and result types are inconsistent",
            instruction.class.opcode
        )
    })
}

fn owned_image_query_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let result_shape = instruction
        .result_type
        .and_then(|ty| numeric_type_shape(ty, definitions));
    let operand_shape = |index| {
        instruction
            .operands
            .get(index)
            .and_then(|operand| match operand {
                Operand::IdRef(value) => value_types.get(value).copied(),
                _ => None,
            })
            .and_then(|ty| numeric_type_shape(ty, definitions))
    };
    let query_size_lanes = |image: ImageTypeShape, lod: bool| {
        let spatial = match image.dim {
            spirv::Dim::Dim1D | spirv::Dim::DimBuffer => 1,
            spirv::Dim::Dim2D | spirv::Dim::DimCube | spirv::Dim::DimRect => 2,
            spirv::Dim::Dim3D => 3,
            _ => return None,
        };
        if lod
            && (!matches!(
                image.dim,
                spirv::Dim::Dim1D | spirv::Dim::Dim2D | spirv::Dim::Dim3D | spirv::Dim::DimCube
            ) || image.multisampled)
        {
            return None;
        }
        if !lod
            && matches!(
                image.dim,
                spirv::Dim::Dim1D | spirv::Dim::Dim2D | spirv::Dim::Dim3D | spirv::Dim::DimCube
            )
            && !image.multisampled
            && !matches!(image.sampled, 0 | 2)
        {
            return None;
        }
        Some(spatial + u32::from(image.arrayed))
    };

    let valid = match instruction.class.opcode {
        Op::ImageQuerySize | Op::ImageQuerySizeLod => {
            let lod = instruction.class.opcode == Op::ImageQuerySizeLod;
            let expected_operands = if lod { 2 } else { 1 };
            let image = image_operand_shape(instruction.operands.first(), definitions, value_types);
            instruction.operands.len() == expected_operands
                && matches!(
                    (image.and_then(|image| query_size_lanes(image, lod)), result_shape),
                    (Some(expected_lanes), Some((Op::TypeInt, _, result_lanes)))
                        if expected_lanes == result_lanes
                )
                && (!lod || matches!(operand_shape(1), Some((Op::TypeInt, 32, 1))))
        }
        Op::ImageQueryLevels => {
            let image = image_operand_shape(instruction.operands.first(), definitions, value_types);
            instruction.operands.len() == 1
                && matches!(result_shape, Some((Op::TypeInt, _, 1)))
                && image.is_some_and(|image| {
                    matches!(
                        image.dim,
                        spirv::Dim::Dim1D
                            | spirv::Dim::Dim2D
                            | spirv::Dim::Dim3D
                            | spirv::Dim::DimCube
                    ) && !image.multisampled
                })
        }
        Op::ImageQuerySamples => {
            let image = image_operand_shape(instruction.operands.first(), definitions, value_types);
            instruction.operands.len() == 1
                && matches!(result_shape, Some((Op::TypeInt, _, 1)))
                && image.is_some_and(|image| image.dim == spirv::Dim::Dim2D && image.multisampled)
        }
        Op::ImageQueryLod => {
            let image =
                sampled_image_operand_shape(instruction.operands.first(), definitions, value_types);
            let coordinate_lanes = match image.map(|image| image.dim) {
                Some(spirv::Dim::Dim1D) => Some(1),
                Some(spirv::Dim::Dim2D) => Some(2),
                Some(spirv::Dim::Dim3D | spirv::Dim::DimCube) => Some(3),
                _ => None,
            };
            instruction.operands.len() == 2
                && matches!(result_shape, Some((Op::TypeFloat, _, 2)))
                && image.is_some_and(|image| !image.multisampled)
                && matches!(
                    (coordinate_lanes, operand_shape(1)),
                    (Some(expected_lanes), Some((Op::TypeFloat, 32, actual_lanes)))
                        if expected_lanes == actual_lanes
                )
        }
        _ => return None,
    };
    (!valid).then(|| {
        format!(
            "native emitter: owned {:?} violates its image-query type and dimensionality contract",
            instruction.class.opcode
        )
    })
}

fn scalar_component_type(ty: Word, definitions: &HashMap<Word, &Instruction>) -> Option<Word> {
    let definition = definitions.get(&ty)?;
    match definition.class.opcode {
        Op::TypeInt | Op::TypeFloat => Some(ty),
        Op::TypeVector => match definition.operands.as_slice() {
            [Operand::IdRef(component), Operand::LiteralBit32(_)] => Some(*component),
            _ => None,
        },
        _ => None,
    }
}

fn pointer_type_shape(
    ty: Word,
    definitions: &HashMap<Word, &Instruction>,
) -> Option<(spirv::StorageClass, Word)> {
    let definition = definitions.get(&ty)?;
    let [Operand::StorageClass(storage), Operand::IdRef(pointee)] = definition.operands.as_slice()
    else {
        return None;
    };
    (definition.class.opcode == Op::TypePointer).then_some((*storage, *pointee))
}

fn owned_memory_type_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let pointer_shape = |index| {
        let Operand::IdRef(pointer) = instruction.operands.get(index)? else {
            return None;
        };
        let pointer_type = *value_types.get(pointer)?;
        pointer_type_shape(pointer_type, definitions)
    };
    let pointer_pointee = |index| pointer_shape(index).map(|(_, pointee)| pointee);
    let operand_type = |index| {
        let Operand::IdRef(value) = instruction.operands.get(index)? else {
            return None;
        };
        value_types.get(value).copied()
    };
    let valid = match instruction.class.opcode {
        Op::Load => pointer_pointee(0).is_some_and(|pointee| {
            instruction.result_type == Some(pointee) && instruction.result_id.is_some()
        }),
        Op::Store => {
            pointer_pointee(0).is_some_and(|pointee| operand_type(1) == Some(pointee))
                && instruction.result_type.is_none()
                && instruction.result_id.is_none()
        }
        Op::CopyMemory => {
            matches!((pointer_pointee(0), pointer_pointee(1)), (Some(target), Some(source)) if target == source)
                && instruction.result_type.is_none()
                && instruction.result_id.is_none()
        }
        _ => return None,
    };
    if !valid {
        let describe_operand = |index: usize| match instruction.operands.get(index) {
            Some(Operand::IdRef(id)) => match value_types.get(id) {
                Some(ty) => format!("%{id}, of type {}", describe_owned_id(*ty, definitions)),
                None => format!("%{id}, which has no value type"),
            },
            Some(operand) => format!("{operand:?}, which is not a value"),
            None => "nothing -- the operand is missing".to_string(),
        };
        let describe_pointee = |index: usize| match pointer_pointee(index) {
            Some(pointee) => describe_owned_id(pointee, definitions),
            None => "no pointee -- it is not a pointer".to_string(),
        };
        let detail = match instruction.class.opcode {
            Op::Load => format!(
                "pointer {}, points at {}, but the result type is {}",
                describe_operand(0),
                describe_pointee(0),
                match instruction.result_type {
                    Some(result_type) => describe_owned_id(result_type, definitions),
                    None => "absent".to_string(),
                }
            ),
            Op::Store => format!(
                "pointer {}, points at {}, but the stored value is {}",
                describe_operand(0),
                describe_pointee(0),
                describe_operand(1)
            ),
            _ => format!(
                "the target {}, points at {}, and the source {}, points at {}",
                describe_operand(0),
                describe_pointee(0),
                describe_operand(1),
                describe_pointee(1)
            ),
        };
        return Some(format!(
            "native emitter: owned {:?} violates its pointer-pointee and value-type contract: \
             {detail}",
            instruction.class.opcode
        ));
    }

    let memory_access_words = |index| {
        let Operand::MemoryAccess(mask) = instruction.operands.get(index)? else {
            return None;
        };
        Some(
            1 + usize::from(mask.contains(spirv::MemoryAccess::ALIGNED))
                + usize::from(mask.contains(spirv::MemoryAccess::MAKE_POINTER_AVAILABLE))
                + usize::from(mask.contains(spirv::MemoryAccess::MAKE_POINTER_VISIBLE))
                + usize::from(mask.contains(spirv::MemoryAccess::ALIAS_SCOPE_INTEL_MASK))
                + usize::from(mask.contains(spirv::MemoryAccess::NO_ALIAS_INTEL_MASK)),
        )
    };
    let aligned_access = |index| {
        let Some(Operand::MemoryAccess(mask)) = instruction.operands.get(index) else {
            return false;
        };
        if !mask.contains(spirv::MemoryAccess::ALIGNED) {
            return false;
        }
        matches!(instruction.operands.get(index + 1), Some(Operand::LiteralBit32(alignment)) if alignment.is_power_of_two())
    };
    let first_access = match instruction.class.opcode {
        Op::Load => 1,
        Op::Store | Op::CopyMemory => 2,
        _ => unreachable!(),
    };
    let second_access = (instruction.class.opcode == Op::CopyMemory)
        .then(|| first_access + memory_access_words(first_access).unwrap_or(0));
    let malformed_alignment = [Some(first_access), second_access]
        .into_iter()
        .flatten()
        .any(|index| {
            matches!(instruction.operands.get(index), Some(Operand::MemoryAccess(mask)) if mask.contains(spirv::MemoryAccess::ALIGNED))
                && !aligned_access(index)
        });
    let physical_without_alignment = match instruction.class.opcode {
        Op::Load | Op::Store => pointer_shape(0).is_some_and(|(storage, _)| {
            storage == spirv::StorageClass::PhysicalStorageBuffer && !aligned_access(first_access)
        }),
        Op::CopyMemory => {
            pointer_shape(0).is_some_and(|(storage, _)| {
                storage == spirv::StorageClass::PhysicalStorageBuffer
                    && !aligned_access(first_access)
            }) || pointer_shape(1).is_some_and(|(storage, _)| {
                storage == spirv::StorageClass::PhysicalStorageBuffer
                    && second_access.is_none_or(|index| !aligned_access(index))
            })
        }
        _ => false,
    };
    (malformed_alignment || physical_without_alignment).then(|| {
        format!(
            "native emitter: owned {:?} violates its aligned memory-access contract",
            instruction.class.opcode
        )
    })
}

fn owned_variable_type_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
    module_scope: bool,
) -> Option<String> {
    if instruction.class.opcode != Op::Variable {
        return None;
    }
    let result_type = instruction.result_type?;
    let pointer = pointer_type_shape(result_type, definitions);
    let declared_storage = match instruction.operands.first() {
        Some(Operand::StorageClass(storage)) => Some(*storage),
        _ => None,
    };
    let storage_valid = matches!(
        (pointer, declared_storage),
        (Some((pointer_storage, _)), Some(declared_storage))
            if pointer_storage == declared_storage
                && if module_scope {
                    declared_storage != spirv::StorageClass::Function
                } else {
                    declared_storage == spirv::StorageClass::Function
                }
    );
    let initializer_valid = match (pointer, instruction.operands.get(1)) {
        (_, None) => true,
        (Some((_, pointee)), Some(Operand::IdRef(initializer))) => {
            value_types.get(initializer) == Some(&pointee)
        }
        _ => false,
    };
    (!storage_valid || !initializer_valid).then(|| {
        "native emitter: owned OpVariable violates its scope, pointer storage, or initializer-type contract"
            .to_string()
    })
}

fn type_contains_pointer(
    ty: Word,
    definitions: &HashMap<Word, &Instruction>,
    visiting: &mut HashSet<Word>,
) -> bool {
    if !visiting.insert(ty) {
        return false;
    }
    let contains = definitions
        .get(&ty)
        .is_some_and(|definition| match definition.class.opcode {
            Op::TypePointer => true,
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => definition
                .operands
                .first()
                .is_some_and(|operand| match operand {
                    Operand::IdRef(element) => {
                        type_contains_pointer(*element, definitions, visiting)
                    }
                    _ => false,
                }),
            Op::TypeStruct => definition.operands.iter().any(|operand| match operand {
                Operand::IdRef(member) => type_contains_pointer(*member, definitions, visiting),
                _ => false,
            }),
            _ => false,
        });
    visiting.remove(&ty);
    contains
}

fn pointer_root(mut id: Word, definitions: &HashMap<Word, &Instruction>) -> Option<Word> {
    let mut visited = HashSet::default();
    loop {
        if !visited.insert(id) {
            return None;
        }
        let definition = definitions.get(&id)?;
        match definition.class.opcode {
            Op::Variable => return Some(id),
            Op::AccessChain
            | Op::InBoundsAccessChain
            | Op::PtrAccessChain
            | Op::InBoundsPtrAccessChain
            | Op::CopyObject => match definition.operands.first()? {
                Operand::IdRef(base) => id = *base,
                _ => return None,
            },
            _ => return None,
        }
    }
}

fn owned_pointer_construction_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
    logical: bool,
    variable_pointers: bool,
    variable_pointers_storage_buffer: bool,
) -> Option<String> {
    let operand_id = |index| match instruction.operands.get(index) {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    };
    let operand_type = |index| operand_id(index).and_then(|id| value_types.get(&id).copied());
    let integer_scalar = |ty| {
        matches!(
            numeric_type_shape(ty, definitions),
            Some((Op::TypeInt, _, 1))
        )
    };
    match instruction.class.opcode {
        Op::Select => {
            let result_type = instruction.result_type?;
            let (storage, _) = pointer_type_shape(result_type, definitions)?;
            if operand_type(1) != Some(result_type) || operand_type(2) != Some(result_type) {
                return Some(
                    "native emitter: cannot reinterpret pointer for owned OpSelect type mismatch"
                        .to_string(),
                );
            }
            if logical
                && storage != spirv::StorageClass::Workgroup
                && matches!(
                    (operand_id(1).and_then(|id| pointer_root(id, definitions)), operand_id(2).and_then(|id| pointer_root(id, definitions))),
                    (Some(left), Some(right)) if left != right
                )
            {
                return Some(
                    "native emitter: cannot retain cross-root pointer OpSelect under Logical addressing"
                        .to_string(),
                );
            }
        }
        Op::ConvertUToPtr => {
            if instruction
                .result_type
                .and_then(|ty| pointer_type_shape(ty, definitions))
                .is_none()
                || operand_type(0).is_none_or(|ty| !integer_scalar(ty))
            {
                return Some(
                    "native emitter: owned OpConvertUToPtr requires a pointer result and integer-scalar input"
                        .to_string(),
                );
            }
        }
        Op::ConvertPtrToU => {
            if instruction.result_type.is_none_or(|ty| !integer_scalar(ty))
                || operand_type(0)
                    .and_then(|ty| pointer_type_shape(ty, definitions))
                    .is_none()
            {
                return Some(
                    "native emitter: owned OpConvertPtrToU requires an integer-scalar result and pointer input"
                        .to_string(),
                );
            }
        }
        Op::AtomicLoad
        | Op::AtomicStore
        | Op::AtomicExchange
        | Op::AtomicCompareExchange
        | Op::AtomicCompareExchangeWeak
        | Op::AtomicIIncrement
        | Op::AtomicIDecrement
        | Op::AtomicIAdd
        | Op::AtomicISub
        | Op::AtomicSMin
        | Op::AtomicUMin
        | Op::AtomicSMax
        | Op::AtomicUMax
        | Op::AtomicAnd
        | Op::AtomicOr
        | Op::AtomicXor
        | Op::AtomicFAddEXT
        | Op::AtomicFMinEXT
        | Op::AtomicFMaxEXT => {
            if let Some((
                storage @ (spirv::StorageClass::Private | spirv::StorageClass::Function),
                _,
            )) = operand_type(0).and_then(|ty| pointer_type_shape(ty, definitions))
            {
                let pointer = match instruction.operands.first() {
                    Some(Operand::IdRef(pointer)) => describe_owned_id(*pointer, definitions),
                    _ => "an operand that is not a value".to_string(),
                };
                return Some(format!(
                    "native emitter: owned {:?} pointer {pointer} has the non-atomic storage \
                     class {storage:?}",
                    instruction.class.opcode
                ));
            }
        }
        Op::ConstantNull if logical => {
            let storage = instruction
                .result_type
                .and_then(|ty| pointer_type_shape(ty, definitions))
                .map(|(storage, _)| storage);
            let admitted_pointer_null = storage.is_some_and(|storage| {
                (variable_pointers
                    && matches!(
                        storage,
                        spirv::StorageClass::StorageBuffer | spirv::StorageClass::Workgroup
                    ))
                    || (variable_pointers_storage_buffer
                        && storage == spirv::StorageClass::StorageBuffer)
            });
            if storage.is_some() && !admitted_pointer_null {
                return Some(
                    "native emitter: cannot retain OpConstantNull pointer under Logical addressing"
                        .to_string(),
                );
            }
        }
        Op::Variable if logical => {
            let pointee = instruction
                .result_type
                .and_then(|ty| pointer_type_shape(ty, definitions))
                .map(|(_, pointee)| pointee);
            if pointee.is_some_and(|pointee| {
                type_contains_pointer(pointee, definitions, &mut HashSet::default())
            }) {
                return Some(
                    "native emitter: missing pointer storage for pointer-valued Logical variable"
                        .to_string(),
                );
            }
        }
        _ => {}
    }
    None
}

struct OwnedAccessChainFailure {
    error: String,
    raw_buffer_eligible: bool,
}

fn describe_result(instruction: &Instruction) -> String {
    instruction
        .result_id
        .map(|result| format!(" %{result}"))
        .unwrap_or_default()
}

fn describe_owned_id(id: Word, definitions: &HashMap<Word, &Instruction>) -> String {
    const SHOWN_OPERANDS: usize = 8;

    let Some(definition) = definitions.get(&id) else {
        return format!("%{id} (undefined)");
    };
    let mut text = format!("%{id} ({:?}", definition.class.opcode);
    for operand in definition.operands.iter().take(SHOWN_OPERANDS) {
        match operand {
            Operand::IdRef(id) => text.push_str(&format!(" %{id}")),
            Operand::LiteralBit32(value) => text.push_str(&format!(" {value}")),
            Operand::StorageClass(storage) => text.push_str(&format!(" {storage:?}")),
            Operand::Dim(dim) => text.push_str(&format!(" {dim:?}")),
            _ => {}
        }
    }
    if let Some(elided) = definition.operands.len().checked_sub(SHOWN_OPERANDS) {
        if elided > 0 {
            text.push_str(&format!(" +{elided} more"));
        }
    }
    text.push(')');
    text
}

fn describe_element_view(
    struct_ty: Word,
    result_type: Option<Word>,
    definitions: &HashMap<Word, &Instruction>,
) -> String {
    let Some(definition) = definitions.get(&struct_ty) else {
        return String::new();
    };
    if definition.class.opcode != Op::TypeStruct || definition.operands.len() != 1 {
        return String::new();
    }
    let Some(Operand::IdRef(member)) = definition.operands.first() else {
        return String::new();
    };
    let Some(member_definition) = definitions.get(member) else {
        return String::new();
    };
    if !matches!(
        member_definition.class.opcode,
        Op::TypeArray | Op::TypeRuntimeArray
    ) {
        return String::new();
    }
    let Some(Operand::IdRef(element)) = member_definition.operands.first() else {
        return String::new();
    };
    let view = result_type
        .and_then(|ty| pointer_type_shape(ty, definitions))
        .map(|(_, pointee)| describe_owned_id(pointee, definitions))
        .unwrap_or_else(|| "an unknown pointee".to_string());
    format!(
        ". The struct is a single-member block over {}, so this is not a member select but a {} \
         view of that array which no dynamic-struct-index rewrite repaired",
        describe_owned_id(*element, definitions),
        view
    )
}

fn owned_access_chain_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<OwnedAccessChainFailure> {
    if !matches!(
        instruction.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::InBoundsPtrAccessChain
    ) {
        return None;
    }
    let Operand::IdRef(base) = instruction.operands.first()? else {
        return Some(OwnedAccessChainFailure {
            error: format!(
                "native emitter: owned {:?} has a malformed base pointer",
                instruction.class.opcode
            ),
            raw_buffer_eligible: false,
        });
    };
    let base_pointer = value_types
        .get(base)
        .and_then(|ty| pointer_type_shape(*ty, definitions));
    let result_pointer = instruction
        .result_type
        .and_then(|ty| pointer_type_shape(ty, definitions));
    let ptr_form = matches!(
        instruction.class.opcode,
        Op::PtrAccessChain | Op::InBoundsPtrAccessChain
    );
    let first_index = usize::from(ptr_form) + 1;
    let element_valid = !ptr_form
        || instruction
            .operands
            .get(1)
            .and_then(|operand| match operand {
                Operand::IdRef(id) => value_types.get(id),
                _ => None,
            })
            .is_some_and(|ty| {
                matches!(
                    numeric_type_shape(*ty, definitions),
                    Some((Op::TypeInt, _, 1))
                )
            });
    let index_type = |operand: &Operand| {
        let Operand::IdRef(id) = operand else {
            return None;
        };
        value_types.get(id).copied()
    };
    let walk_index_path = |base_pointee: Word| -> Result<Word, String> {
        let mut selected = base_pointee;
        let indices = instruction
            .operands
            .get(first_index..)
            .map(<[Operand]>::iter)
            .into_iter()
            .flatten();
        for (position, index) in indices.enumerate() {
            let Some(index_ty) = index_type(index) else {
                return Err(format!("index {position} is not an id operand"));
            };
            if !matches!(
                numeric_type_shape(index_ty, definitions),
                Some((Op::TypeInt, _, 1))
            ) {
                return Err(format!(
                    "index {position} has type {}, which is not a scalar integer",
                    describe_owned_id(index_ty, definitions)
                ));
            }
            let Some(definition) = definitions.get(&selected) else {
                return Err(format!(
                    "index {position} descends into %{selected}, which this module never declares"
                ));
            };
            selected = match definition.class.opcode {
                Op::TypeStruct => {
                    let Operand::IdRef(index) = index else {
                        return Err(format!("index {position} is not an id operand"));
                    };
                    let Some(member) = owned_constant_u32(*index, definitions, value_types) else {
                        return Err(format!(
                            "index {position} into {} is {}, and a struct member index must be an \
                             OpConstant of 32-bit integer type{}",
                            describe_owned_id(selected, definitions),
                            describe_owned_id(*index, definitions),
                            describe_element_view(selected, instruction.result_type, definitions)
                        ));
                    };
                    let Some(Operand::IdRef(member_type)) =
                        definition.operands.get(member as usize)
                    else {
                        return Err(format!(
                            "index {position} selects member {member} of {}, which has {} members",
                            describe_owned_id(selected, definitions),
                            definition.operands.len()
                        ));
                    };
                    *member_type
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    let Some(Operand::IdRef(element)) = definition.operands.first() else {
                        return Err(format!(
                            "index {position} descends into {}, which declares no element type",
                            describe_owned_id(selected, definitions)
                        ));
                    };
                    *element
                }
                Op::TypeVector => match vector_type_shape(selected, definitions) {
                    Some((component, _)) => component,
                    None => {
                        return Err(format!(
                            "index {position} descends into {}, which declares no component type",
                            describe_owned_id(selected, definitions)
                        ))
                    }
                },
                Op::TypeMatrix => {
                    let Some(Operand::IdRef(column)) = definition.operands.first() else {
                        return Err(format!(
                            "index {position} descends into {}, which declares no column type",
                            describe_owned_id(selected, definitions)
                        ));
                    };
                    *column
                }
                _ => {
                    return Err(format!(
                        "index {position} descends into {}, which is not a composite type",
                        describe_owned_id(selected, definitions)
                    ))
                }
            };
        }
        Ok(selected)
    };

    let reason = (|| {
        let Some((base_storage, base_pointee)) = base_pointer else {
            return Some(match value_types.get(base) {
                Some(&base_ty) => format!(
                    "base %{base} has type {}, which is not a pointer type",
                    describe_owned_id(base_ty, definitions)
                ),
                None => format!("base %{base} has no type in this module"),
            });
        };
        let Some((result_storage, result_pointee)) = result_pointer else {
            return Some(match instruction.result_type {
                Some(result_ty) => format!(
                    "result type {} is not a pointer type",
                    describe_owned_id(result_ty, definitions)
                ),
                None => "the instruction has no result type".to_string(),
            });
        };
        if !element_valid {
            return Some(match instruction.operands.get(1).and_then(index_type) {
                Some(element_ty) => format!(
                    "the element operand has type {}, which is not a scalar integer",
                    describe_owned_id(element_ty, definitions)
                ),
                None => "the element operand is not a value with a type in this module".to_string(),
            });
        }
        let selected = match walk_index_path(base_pointee) {
            Ok(selected) => selected,
            Err(reason) => return Some(reason),
        };
        if base_storage != result_storage {
            return Some(format!(
                "base %{base} is in storage class {base_storage:?} but the result pointer is in \
                 {result_storage:?}"
            ));
        }
        (result_pointee != selected).then(|| {
            format!(
                "the index path selects {}, but the result pointer points to {}",
                describe_owned_id(selected, definitions),
                describe_owned_id(result_pointee, definitions)
            )
        })
    })();
    reason.map(|reason| OwnedAccessChainFailure {
        error: format!(
            "native emitter: owned {:?} violates its access-chain contract: {reason}",
            instruction.class.opcode
        ),
        raw_buffer_eligible: base_pointer.is_some_and(|(storage, _)| {
            matches!(
                storage,
                spirv::StorageClass::StorageBuffer | spirv::StorageClass::Uniform
            )
        }),
    })
}

fn owned_texel_access_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    if !matches!(
        instruction.class.opcode,
        Op::ImageFetch | Op::ImageRead | Op::ImageWrite
    ) {
        return None;
    }
    let operand_type = |index| {
        let Operand::IdRef(value) = instruction.operands.get(index)? else {
            return None;
        };
        value_types.get(value).copied()
    };
    let image = image_operand_shape(instruction.operands.first(), definitions, value_types);
    let coordinate_lanes = |image: ImageTypeShape| {
        match image.dim {
            spirv::Dim::Dim1D | spirv::Dim::DimBuffer => Some(1),
            spirv::Dim::Dim2D | spirv::Dim::DimRect | spirv::Dim::DimSubpassData => Some(2),
            spirv::Dim::Dim3D | spirv::Dim::DimCube => Some(3),
            _ => None,
        }
        .map(|lanes| lanes + u32::from(image.arrayed))
    };
    let coordinate_valid = image.is_some_and(|image| {
        matches!(
            (coordinate_lanes(image), operand_type(1).and_then(|ty| numeric_type_shape(ty, definitions))),
            (Some(expected_lanes), Some((Op::TypeInt | Op::TypeFloat, 32, actual_lanes)))
                if expected_lanes == actual_lanes
        )
    });
    let value_type = if instruction.class.opcode == Op::ImageWrite {
        operand_type(2)
    } else {
        instruction.result_type
    };
    let value_valid = matches!(
        (image, value_type),
        (Some(image), Some(value_type))
            if scalar_component_type(value_type, definitions) == Some(image.sampled_type)
                && (instruction.class.opcode != Op::ImageFetch
                    || matches!(numeric_type_shape(value_type, definitions), Some((_, _, 4))))
    );
    let fixed_operands = if instruction.class.opcode == Op::ImageWrite {
        3
    } else {
        2
    };
    let image_operands = match instruction.operands.get(fixed_operands..) {
        Some([]) => Some(None),
        Some([Operand::ImageOperands(mask), Operand::IdRef(value)])
            if matches!(
                *mask,
                spirv::ImageOperands::LOD | spirv::ImageOperands::SAMPLE
            ) =>
        {
            Some(Some((*mask, *value)))
        }
        _ => None,
    };
    let image_operands_valid = matches!(
        (image, image_operands),
        (Some(image), Some(None)) if !image.multisampled
    ) || matches!(
        (image, image_operands),
        (Some(image), Some(Some((spirv::ImageOperands::SAMPLE, value))))
            if image.multisampled
                && value_types.get(&value).is_some_and(|ty| {
                    matches!(numeric_type_shape(*ty, definitions), Some((Op::TypeInt, 32, 1)))
                })
    ) || matches!(
        (image, image_operands),
        (Some(image), Some(Some((spirv::ImageOperands::LOD, value))))
            if !image.multisampled
                && instruction.class.opcode == Op::ImageFetch
                && matches!(
                    image.dim,
                    spirv::Dim::Dim1D | spirv::Dim::Dim2D | spirv::Dim::Dim3D
                )
                && value_types.get(&value).is_some_and(|ty| {
                    matches!(numeric_type_shape(*ty, definitions), Some((Op::TypeInt, 32, 1)))
                })
    );
    let image_mode_valid = image.is_some_and(|image| match instruction.class.opcode {
        Op::ImageFetch => image.sampled == 1 && image.dim != spirv::Dim::DimCube,
        Op::ImageRead | Op::ImageWrite => {
            matches!(image.sampled, 0 | 2)
                && (instruction.class.opcode != Op::ImageWrite
                    || image.dim != spirv::Dim::DimSubpassData)
        }
        _ => false,
    });
    let valid = coordinate_valid
        && value_valid
        && image_operands_valid
        && image_mode_valid
        && (instruction.class.opcode != Op::ImageWrite
            || instruction.result_type.is_none() && instruction.result_id.is_none());
    (!valid).then(|| {
        format!(
            "native emitter: owned {:?} violates its image, coordinate, texel, or image-operands contract \
             [coordinate {} texel {} operands {} mode {}; image {:?}, coordinate lanes {:?}]",
            instruction.class.opcode,
            coordinate_valid,
            value_valid,
            image_operands_valid,
            image_mode_valid,
            image.map(|i| (i.dim, i.arrayed, i.sampled, i.multisampled)),
            operand_type(1).and_then(|ty| numeric_type_shape(ty, definitions)),
        )
    })
}

fn owned_image_texel_pointer_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    if instruction.class.opcode != Op::ImageTexelPointer {
        return None;
    }
    let [Operand::IdRef(image), Operand::IdRef(coordinate), Operand::IdRef(sample)] =
        instruction.operands.as_slice()
    else {
        return Some(
            "native emitter: owned OpImageTexelPointer has malformed operands".to_string(),
        );
    };
    let pointer_shape = |ty: Word| {
        let definition = definitions.get(&ty)?;
        let [Operand::StorageClass(storage), Operand::IdRef(pointee)] =
            definition.operands.as_slice()
        else {
            return None;
        };
        (definition.class.opcode == Op::TypePointer).then_some((*storage, *pointee))
    };
    let result_pointer = instruction.result_type.and_then(pointer_shape);
    let image_type = value_types
        .get(image)
        .and_then(|ty| pointer_shape(*ty))
        .and_then(|(_, pointee)| image_type_shape(pointee, definitions));
    let coordinate_shape = value_types
        .get(coordinate)
        .and_then(|ty| numeric_type_shape(*ty, definitions));
    let sample_shape = value_types
        .get(sample)
        .and_then(|ty| numeric_type_shape(*ty, definitions));
    let coordinate_lanes = image_type.and_then(|image| match (image.dim, image.arrayed) {
        (spirv::Dim::Dim1D | spirv::Dim::DimBuffer, false) => Some(1),
        (spirv::Dim::Dim2D | spirv::Dim::DimRect, false) => Some(2),
        (spirv::Dim::Dim3D | spirv::Dim::DimCube, false) => Some(3),
        (spirv::Dim::Dim1D, true) => Some(2),
        (spirv::Dim::Dim2D | spirv::Dim::DimCube, true) => Some(3),
        _ => None,
    });
    let sample_is_zero = definitions.get(sample).is_some_and(|definition| {
        definition.class.opcode == Op::ConstantNull
            || (definition.class.opcode == Op::Constant
                && !definition.operands.is_empty()
                && definition
                    .operands
                    .iter()
                    .all(|operand| matches!(operand, Operand::LiteralBit32(0))))
    });
    let result_valid = matches!(
        (result_pointer, image_type),
        (Some((spirv::StorageClass::Image, pointee)), Some(image))
            if pointee == image.sampled_type
                && matches!(
                    definitions.get(&pointee).map(|definition| definition.class.opcode),
                    Some(Op::TypeInt | Op::TypeFloat | Op::TypeVoid)
                )
    );
    let coordinate_valid = matches!(
        (coordinate_lanes, coordinate_shape),
        (Some(expected_lanes), Some((Op::TypeInt, 32, actual_lanes)))
            if expected_lanes == actual_lanes
    );
    let sample_valid = matches!(sample_shape, Some((Op::TypeInt, 32, 1)))
        && image_type.is_some_and(|image| image.multisampled || sample_is_zero);
    let image_valid = image_type.is_some_and(|image| {
        let sampled_definition = definitions.get(&image.sampled_type);
        let format_matches_sampled_type = match image.format {
            spirv::ImageFormat::R32f => sampled_definition.is_some_and(|definition| {
                definition.class.opcode == Op::TypeFloat
                    && matches!(definition.operands.as_slice(), [Operand::LiteralBit32(32)])
            }),
            spirv::ImageFormat::R32i | spirv::ImageFormat::R64i => {
                let width = if image.format == spirv::ImageFormat::R32i {
                    32
                } else {
                    64
                };
                sampled_definition.is_some_and(|definition| {
                    definition.class.opcode == Op::TypeInt
                        && matches!(
                            definition.operands.as_slice(),
                            [Operand::LiteralBit32(actual_width), Operand::LiteralBit32(1)]
                                if *actual_width == width
                        )
                })
            }
            spirv::ImageFormat::R32ui | spirv::ImageFormat::R64ui => {
                let width = if image.format == spirv::ImageFormat::R32ui {
                    32
                } else {
                    64
                };
                sampled_definition.is_some_and(|definition| {
                    definition.class.opcode == Op::TypeInt
                        && matches!(
                            definition.operands.as_slice(),
                            [Operand::LiteralBit32(actual_width), Operand::LiteralBit32(0)]
                                if *actual_width == width
                        )
                })
            }
            _ => false,
        };
        image.dim != spirv::Dim::DimSubpassData && image.sampled == 2 && format_matches_sampled_type
    });
    (!(result_valid && coordinate_valid && sample_valid && image_valid)).then(|| {
        "native emitter: owned OpImageTexelPointer violates its result, image, coordinate, sample, or atomic-format contract".to_string()
    })
}

fn owned_sample_operation_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    if !matches!(
        instruction.class.opcode,
        Op::ImageSampleImplicitLod | Op::ImageSampleExplicitLod | Op::ImageGather
    ) {
        return None;
    }
    let operand_type = |index| {
        let Operand::IdRef(value) = instruction.operands.get(index)? else {
            return None;
        };
        value_types.get(value).copied()
    };
    let image = sampled_image_operand_shape(instruction.operands.first(), definitions, value_types);
    let spatial_lanes = image.and_then(|image| match image.dim {
        spirv::Dim::Dim1D => Some(1),
        spirv::Dim::Dim2D | spirv::Dim::DimRect => Some(2),
        spirv::Dim::Dim3D | spirv::Dim::DimCube => Some(3),
        _ => None,
    });
    let coordinate_valid = matches!(
        (image, spatial_lanes, operand_type(1).and_then(|ty| numeric_type_shape(ty, definitions))),
        (Some(image), Some(spatial_lanes), Some((Op::TypeFloat, 32, coordinate_lanes)))
            if coordinate_lanes >= spatial_lanes + u32::from(image.arrayed)
    );
    let result_valid = matches!(
        (image, instruction.result_type),
        (Some(image), Some(result_type))
            if scalar_component_type(result_type, definitions) == Some(image.sampled_type)
                && matches!(numeric_type_shape(result_type, definitions), Some((_, _, 4)))
    );
    let offset_valid = |value: Word| {
        let shape = value_types
            .get(&value)
            .and_then(|ty| numeric_type_shape(*ty, definitions));
        let constant = definitions
            .get(&value)
            .is_some_and(|definition| definition.class.opcode.is_constant());
        constant
            && matches!(
                (image, spatial_lanes, shape),
                (Some(image), Some(spatial_lanes), Some((Op::TypeInt, 32, lanes)))
                    if image.dim != spirv::Dim::DimCube && lanes == spatial_lanes
            )
    };
    let lod_valid = |value: Word| {
        value_types.get(&value).is_some_and(|ty| {
            matches!(
                numeric_type_shape(*ty, definitions),
                Some((Op::TypeFloat, 32, 1))
            )
        })
    };
    let grad_valid = |dx: Word, dy: Word| {
        let shape = |value: Word| {
            value_types
                .get(&value)
                .and_then(|ty| numeric_type_shape(*ty, definitions))
        };
        matches!(
            (spatial_lanes, shape(dx), shape(dy)),
            (
                Some(spatial_lanes),
                Some((Op::TypeFloat, 32, dx_lanes)),
                Some((Op::TypeFloat, 32, dy_lanes)),
            ) if dx_lanes == spatial_lanes && dy_lanes == spatial_lanes
        )
    };
    let tail = if instruction.class.opcode == Op::ImageGather {
        instruction.operands.get(3..)
    } else {
        instruction.operands.get(2..)
    };
    let image_operands_valid = match (instruction.class.opcode, tail) {
        (Op::ImageSampleImplicitLod, Some([])) | (Op::ImageGather, Some([])) => true,
        (
            Op::ImageSampleImplicitLod | Op::ImageGather,
            Some(
                [Operand::ImageOperands(spirv::ImageOperands::CONST_OFFSET), Operand::IdRef(offset)],
            ),
        ) => offset_valid(*offset),
        (
            Op::ImageSampleImplicitLod,
            Some([Operand::ImageOperands(spirv::ImageOperands::BIAS), Operand::IdRef(bias)]),
        ) => lod_valid(*bias),
        (
            Op::ImageSampleImplicitLod,
            Some([Operand::ImageOperands(mask), Operand::IdRef(bias), Operand::IdRef(offset)]),
        ) if *mask == (spirv::ImageOperands::BIAS | spirv::ImageOperands::CONST_OFFSET) => {
            lod_valid(*bias) && offset_valid(*offset)
        }
        (
            Op::ImageSampleExplicitLod,
            Some([Operand::ImageOperands(spirv::ImageOperands::LOD), Operand::IdRef(lod)]),
        ) => lod_valid(*lod),
        (
            Op::ImageSampleExplicitLod,
            Some([Operand::ImageOperands(mask), Operand::IdRef(lod), Operand::IdRef(offset)]),
        ) if *mask == (spirv::ImageOperands::LOD | spirv::ImageOperands::CONST_OFFSET) => {
            lod_valid(*lod) && offset_valid(*offset)
        }
        (
            Op::ImageSampleExplicitLod,
            Some(
                [Operand::ImageOperands(spirv::ImageOperands::GRAD), Operand::IdRef(dx), Operand::IdRef(dy)],
            ),
        ) => grad_valid(*dx, *dy),
        (
            Op::ImageSampleExplicitLod,
            Some(
                [Operand::ImageOperands(mask), Operand::IdRef(dx), Operand::IdRef(dy), Operand::IdRef(offset)],
            ),
        ) if *mask == (spirv::ImageOperands::GRAD | spirv::ImageOperands::CONST_OFFSET) => {
            grad_valid(*dx, *dy) && offset_valid(*offset)
        }
        (
            Op::ImageSampleExplicitLod,
            Some(
                [Operand::ImageOperands(mask), Operand::IdRef(dx), Operand::IdRef(dy), Operand::IdRef(min_lod)],
            ),
        ) if *mask == (spirv::ImageOperands::GRAD | spirv::ImageOperands::MIN_LOD) => {
            grad_valid(*dx, *dy) && lod_valid(*min_lod)
        }
        (
            Op::ImageSampleExplicitLod,
            Some(
                [Operand::ImageOperands(mask), Operand::IdRef(dx), Operand::IdRef(dy), Operand::IdRef(offset), Operand::IdRef(min_lod)],
            ),
        ) if *mask
            == (spirv::ImageOperands::GRAD
                | spirv::ImageOperands::CONST_OFFSET
                | spirv::ImageOperands::MIN_LOD) =>
        {
            grad_valid(*dx, *dy) && offset_valid(*offset) && lod_valid(*min_lod)
        }
        _ => false,
    };
    let operation_valid = image.is_some_and(|image| {
        !image.multisampled
            && image.sampled == 1
            && (instruction.class.opcode != Op::ImageGather
                || matches!(
                    image.dim,
                    spirv::Dim::Dim2D | spirv::Dim::DimCube | spirv::Dim::DimRect
                ))
    }) && (instruction.class.opcode != Op::ImageGather
        || matches!(operand_type(2), Some(component) if matches!(
            numeric_type_shape(component, definitions),
            Some((Op::TypeInt, 32, 1))
        )));
    let valid = coordinate_valid && result_valid && image_operands_valid && operation_valid;
    (!valid).then(|| {
        format!(
            "native emitter: owned {:?} violates its sampled image, result, coordinate, component, or image-operands contract",
            instruction.class.opcode
        )
    })
}

fn owned_group_non_uniform_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    let opcode = instruction.class.opcode;
    if !matches!(
        opcode,
        Op::GroupNonUniformElect
            | Op::GroupNonUniformAll
            | Op::GroupNonUniformAny
            | Op::GroupNonUniformAllEqual
            | Op::GroupNonUniformBallot
            | Op::GroupNonUniformBroadcastFirst
            | Op::GroupNonUniformShuffle
            | Op::GroupNonUniformShuffleXor
            | Op::GroupNonUniformShuffleDown
            | Op::GroupNonUniformIAdd
            | Op::GroupNonUniformFAdd
            | Op::GroupNonUniformFMin
            | Op::GroupNonUniformFMax
            | Op::GroupNonUniformSMin
            | Op::GroupNonUniformSMax
            | Op::GroupNonUniformUMin
            | Op::GroupNonUniformUMax
            | Op::GroupNonUniformBitwiseAnd
            | Op::GroupNonUniformBitwiseOr
            | Op::GroupNonUniformBitwiseXor
    ) {
        return None;
    }

    let operand_id = |index| {
        let Operand::IdRef(id) = instruction.operands.get(index)? else {
            return None;
        };
        Some(*id)
    };
    let operand_type = |index| operand_id(index).and_then(|id| value_types.get(&id).copied());
    let scalar_or_vector = |ty| {
        matches!(
            scalar_type_shape(ty, definitions),
            Some((Op::TypeBool | Op::TypeInt | Op::TypeFloat, _))
        )
    };
    let bool_scalar = |ty| scalar_type_shape(ty, definitions) == Some((Op::TypeBool, 1));
    let uint32_scalar = |ty| {
        numeric_type_shape(ty, definitions) == Some((Op::TypeInt, 32, 1))
            && integer_type_signedness(ty, definitions) == Some(false)
    };
    let scope_valid = matches!(instruction.operands.first(), Some(Operand::IdScope(id))
        if owned_constant_u32(*id, definitions, value_types)
            == Some(spirv::Scope::Subgroup as u32));
    let result_type = instruction.result_type;

    let valid = scope_valid
        && match opcode {
            Op::GroupNonUniformElect => {
                instruction.operands.len() == 1 && result_type.is_some_and(bool_scalar)
            }
            Op::GroupNonUniformAll | Op::GroupNonUniformAny => {
                instruction.operands.len() == 2
                    && result_type.is_some_and(bool_scalar)
                    && operand_type(1).is_some_and(bool_scalar)
            }
            Op::GroupNonUniformAllEqual => {
                instruction.operands.len() == 2
                    && result_type.is_some_and(bool_scalar)
                    && operand_type(1).is_some_and(scalar_or_vector)
            }
            Op::GroupNonUniformBallot => {
                instruction.operands.len() == 2
                    && result_type.is_some_and(|ty| {
                        numeric_type_shape(ty, definitions) == Some((Op::TypeInt, 32, 4))
                            && integer_type_signedness(ty, definitions) == Some(false)
                    })
                    && operand_type(1).is_some_and(bool_scalar)
            }
            Op::GroupNonUniformBroadcastFirst => {
                instruction.operands.len() == 2
                    && matches!((result_type, operand_type(1)), (Some(result), Some(value))
                        if result == value && scalar_or_vector(result))
            }
            Op::GroupNonUniformShuffle
            | Op::GroupNonUniformShuffleXor
            | Op::GroupNonUniformShuffleDown => {
                instruction.operands.len() == 3
                    && matches!((result_type, operand_type(1)), (Some(result), Some(value))
                        if result == value && scalar_or_vector(result))
                    && operand_type(2).is_some_and(uint32_scalar)
            }
            Op::GroupNonUniformIAdd
            | Op::GroupNonUniformFAdd
            | Op::GroupNonUniformFMin
            | Op::GroupNonUniformFMax
            | Op::GroupNonUniformSMin
            | Op::GroupNonUniformSMax
            | Op::GroupNonUniformUMin
            | Op::GroupNonUniformUMax
            | Op::GroupNonUniformBitwiseAnd
            | Op::GroupNonUniformBitwiseOr
            | Op::GroupNonUniformBitwiseXor => {
                let value_type = operand_type(2);
                let type_valid = matches!((result_type, value_type), (Some(result), Some(value))
                if result == value
                    && match opcode {
                        Op::GroupNonUniformFAdd
                        | Op::GroupNonUniformFMin
                        | Op::GroupNonUniformFMax => {
                            matches!(scalar_type_shape(result, definitions), Some((Op::TypeFloat, _)))
                        }
                        Op::GroupNonUniformSMin | Op::GroupNonUniformSMax => {
                            integer_type_signedness(result, definitions) == Some(true)
                        }
                        Op::GroupNonUniformUMin | Op::GroupNonUniformUMax => {
                            integer_type_signedness(result, definitions) == Some(false)
                        }
                        _ => integer_type_signedness(result, definitions).is_some(),
                    });
                let operation_valid = match instruction.operands.get(1) {
                    Some(Operand::GroupOperation(
                        spirv::GroupOperation::Reduce
                        | spirv::GroupOperation::InclusiveScan
                        | spirv::GroupOperation::ExclusiveScan,
                    )) => instruction.operands.len() == 3,
                    Some(Operand::GroupOperation(spirv::GroupOperation::ClusteredReduce)) => {
                        instruction.operands.len() == 4
                            && operand_id(3)
                                .and_then(|id| owned_constant_u32(id, definitions, value_types))
                                .is_some_and(|size| size.is_power_of_two())
                            && operand_type(3).is_some_and(uint32_scalar)
                    }
                    _ => false,
                };
                type_valid && operation_valid
            }
            _ => unreachable!("group opcode filtered above"),
        };

    (!valid).then(|| {
        format!(
            "native emitter: owned {opcode:?} violates its subgroup scope, operand, result, or group-operation contract"
        )
    })
}

fn owned_barrier_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Option<String> {
    if !matches!(
        instruction.class.opcode,
        Op::ControlBarrier | Op::MemoryBarrier
    ) {
        return None;
    }
    let scope_word = |operand: &Operand| match operand {
        Operand::IdScope(id) => owned_constant_u32(*id, definitions, value_types),
        _ => None,
    };
    let semantics_word = |operand: &Operand| match operand {
        Operand::IdMemorySemantics(id) => owned_constant_u32(*id, definitions, value_types),
        _ => None,
    };
    let (execution_scope, memory_scope, semantics) = match instruction.operands.as_slice() {
        [execution, memory, semantics] if instruction.class.opcode == Op::ControlBarrier => (
            scope_word(execution),
            scope_word(memory),
            semantics_word(semantics),
        ),
        [memory, semantics] if instruction.class.opcode == Op::MemoryBarrier => {
            (None, scope_word(memory), semantics_word(semantics))
        }
        _ => {
            return Some(format!(
                "native emitter: owned {:?} has malformed operands",
                instruction.class.opcode
            ));
        }
    };
    let execution_scope_valid = instruction.class.opcode == Op::MemoryBarrier
        || execution_scope.is_some_and(|scope| {
            matches!(
                spirv::Scope::from_u32(scope),
                Some(spirv::Scope::Workgroup | spirv::Scope::Subgroup)
            )
        });
    let memory_scope_valid = memory_scope.is_some_and(|scope| {
        matches!(
            spirv::Scope::from_u32(scope),
            Some(
                spirv::Scope::Device
                    | spirv::Scope::Workgroup
                    | spirv::Scope::Subgroup
                    | spirv::Scope::Invocation
            )
        )
    });
    let order_mask = (spirv::MemorySemantics::ACQUIRE
        | spirv::MemorySemantics::RELEASE
        | spirv::MemorySemantics::ACQUIRE_RELEASE
        | spirv::MemorySemantics::SEQUENTIALLY_CONSISTENT)
        .bits();
    let memory_mask = (spirv::MemorySemantics::UNIFORM_MEMORY
        | spirv::MemorySemantics::SUBGROUP_MEMORY
        | spirv::MemorySemantics::WORKGROUP_MEMORY
        | spirv::MemorySemantics::CROSS_WORKGROUP_MEMORY
        | spirv::MemorySemantics::ATOMIC_COUNTER_MEMORY
        | spirv::MemorySemantics::IMAGE_MEMORY
        | spirv::MemorySemantics::OUTPUT_MEMORY)
        .bits();
    let semantics_valid = semantics.is_some_and(|semantics| {
        (semantics & order_mask).count_ones() == 1
            && semantics & memory_mask != 0
            && semantics & !(order_mask | memory_mask) == 0
    });
    let valid = instruction.result_type.is_none()
        && instruction.result_id.is_none()
        && execution_scope_valid
        && memory_scope_valid
        && semantics_valid;
    (!valid).then(|| {
        format!(
            "native emitter: owned {:?} violates its constant scope and memory-semantics contract",
            instruction.class.opcode
        )
    })
}

fn owned_value_instruction_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Result<(), String> {
    let operand_type = |operand: &Operand| {
        let Operand::IdRef(value) = operand else {
            return None;
        };
        value_types.get(value).copied()
    };
    if let Some(error) = atomic_value_contract_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_glsl_ext_inst_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_sampled_image_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_image_query_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_texel_access_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_image_texel_pointer_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_sample_operation_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_group_non_uniform_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(error) = owned_barrier_error(instruction, definitions, value_types) {
        return Err(error);
    }
    if let Some(class) = same_type_class(instruction.class.opcode) {
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        if let Some((position, operand)) = instruction
            .operands
            .iter()
            .enumerate()
            .find(|(_, operand)| operand_type(operand) != Some(result_type))
        {
            let described = match operand {
                Operand::IdRef(value) => match value_types.get(value) {
                    Some(operand_type) => format!(
                        "%{value}, of type {}",
                        describe_owned_id(*operand_type, definitions)
                    ),
                    None => format!("%{value}, which has no value type"),
                },
                operand => format!("{operand:?}, which is not a value"),
            };
            return Err(format!(
                "native emitter: owned {:?} operands do not match its result type: operand \
                 {position} is {described}, but the result type is {}",
                instruction.class.opcode,
                describe_owned_id(result_type, definitions)
            ));
        }
        let scalar_opcode = scalar_type_shape(result_type, definitions).map(|(opcode, _)| opcode);
        let class_matches = match class {
            SameTypeClass::Any => true,
            SameTypeClass::Bool => scalar_opcode == Some(Op::TypeBool),
            SameTypeClass::Integer => scalar_opcode == Some(Op::TypeInt),
            SameTypeClass::Float => scalar_opcode == Some(Op::TypeFloat),
        };
        if !class_matches {
            return Err(format!(
                "native emitter: owned {:?} has an invalid result type class",
                instruction.class.opcode
            ));
        }
    }
    if let Some(class) = comparison_class(instruction.class.opcode) {
        let [left, right] = instruction.operands.as_slice() else {
            return Ok(());
        };
        let (Some(left_type), Some(right_type)) = (operand_type(left), operand_type(right)) else {
            return Ok(());
        };
        if left_type != right_type {
            return Err(format!(
                "native emitter: owned {:?} operands have different types",
                instruction.class.opcode
            ));
        }
        let Some((operand_scalar, operand_lanes)) = scalar_type_shape(left_type, definitions)
        else {
            return Err(format!(
                "native emitter: owned {:?} has an invalid operand type class",
                instruction.class.opcode
            ));
        };
        let operand_class_matches = match class {
            ComparisonClass::Integer => operand_scalar == Op::TypeInt,
            ComparisonClass::Float => operand_scalar == Op::TypeFloat,
        };
        if !operand_class_matches {
            return Err(format!(
                "native emitter: owned {:?} has an invalid operand type class",
                instruction.class.opcode
            ));
        }
        if instruction
            .result_type
            .and_then(|ty| scalar_type_shape(ty, definitions))
            != Some((Op::TypeBool, operand_lanes))
        {
            return Err(format!(
                "native emitter: owned {:?} result does not preserve its Boolean lane shape",
                instruction.class.opcode
            ));
        }
    }
    if matches!(
        instruction.class.opcode,
        Op::SatConvertSToU | Op::SatConvertUToS
    ) {
        return Err(format!(
            "native emitter: owned {:?} is not available in Vulkan 1.2",
            instruction.class.opcode
        ));
    }
    if let Some(class) = conversion_class(instruction.class.opcode) {
        let Some(source_type) = instruction.operands.first().and_then(operand_type) else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let source_shape = numeric_type_shape(source_type, definitions);
        let result_shape = numeric_type_shape(result_type, definitions);
        let matches = match (class, source_shape, result_shape) {
            (
                ConversionClass::UnsignedWidth | ConversionClass::SignedWidth,
                Some((Op::TypeInt, source_width, source_lanes)),
                Some((Op::TypeInt, result_width, result_lanes)),
            )
            | (
                ConversionClass::FloatWidth,
                Some((Op::TypeFloat, source_width, source_lanes)),
                Some((Op::TypeFloat, result_width, result_lanes)),
            ) => source_lanes == result_lanes && source_width != result_width,
            (
                ConversionClass::FloatToInteger,
                Some((Op::TypeFloat, _, source_lanes)),
                Some((Op::TypeInt, _, result_lanes)),
            )
            | (
                ConversionClass::IntegerToFloat,
                Some((Op::TypeInt, _, source_lanes)),
                Some((Op::TypeFloat, _, result_lanes)),
            ) => source_lanes == result_lanes,
            _ => false,
        };
        if !matches {
            return Err(format!(
                "native emitter: owned {:?} source and result shapes are inconsistent",
                instruction.class.opcode
            ));
        }
        if matches!(class, ConversionClass::UnsignedWidth)
            && integer_type_signedness(result_type, definitions) != Some(false)
        {
            return Err(
                "native emitter: owned OpUConvert result type is not an unsigned integer"
                    .to_string(),
            );
        }
    }
    if matches!(
        instruction.class.opcode,
        Op::ShiftLeftLogical | Op::ShiftRightLogical | Op::ShiftRightArithmetic
    ) {
        let [base, shift] = instruction.operands.as_slice() else {
            return Ok(());
        };
        let (Some(result_type), Some(base_type), Some(shift_type)) = (
            instruction.result_type,
            operand_type(base),
            operand_type(shift),
        ) else {
            return Ok(());
        };
        let result_shape = numeric_type_shape(result_type, definitions);
        let base_shape = numeric_type_shape(base_type, definitions);
        let shift_shape = numeric_type_shape(shift_type, definitions);
        let valid = matches!(
            (result_shape, base_shape, shift_shape),
            (
                Some((Op::TypeInt, result_width, result_lanes)),
                Some((Op::TypeInt, base_width, base_lanes)),
                Some((Op::TypeInt, _, shift_lanes)),
            ) if result_width == base_width
                && result_lanes == base_lanes
                && base_lanes == shift_lanes
        );
        if !valid {
            return Err(format!(
                "native emitter: owned {:?} operand and result shapes are inconsistent",
                instruction.class.opcode
            ));
        }
    }
    if instruction.class.opcode == Op::Bitcast {
        let Some(source_type) = instruction.operands.first().and_then(operand_type) else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let is_pointer = |ty| {
            definitions
                .get(&ty)
                .is_some_and(|definition| definition.class.opcode == Op::TypePointer)
        };
        if !is_pointer(source_type) && !is_pointer(result_type) {
            let source_shape = numeric_type_shape(source_type, definitions);
            let result_shape = numeric_type_shape(result_type, definitions);
            let valid = match (source_shape, result_shape) {
                (Some((_, source_width, source_lanes)), Some((_, result_width, result_lanes))) => {
                    if source_lanes == result_lanes {
                        source_width == result_width
                    } else {
                        u64::from(source_width) * u64::from(source_lanes)
                            == u64::from(result_width) * u64::from(result_lanes)
                            && source_lanes.max(result_lanes) % source_lanes.min(result_lanes) == 0
                    }
                }
                _ => false,
            };
            if !valid {
                return Err(
                    "native emitter: owned OpBitcast source and result shapes are inconsistent"
                        .to_string(),
                );
            }
        }
    }
    if instruction.class.opcode == Op::BitCount {
        let Some(base_type) = instruction.operands.first().and_then(operand_type) else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let valid = matches!(
            (
                numeric_type_shape(base_type, definitions),
                numeric_type_shape(result_type, definitions),
            ),
            (
                Some((Op::TypeInt, base_width, base_lanes)),
                Some((Op::TypeInt, result_width, result_lanes)),
            ) if base_width == 32
                && base_lanes == result_lanes
                && result_width >= u32::BITS - base_width.leading_zeros()
        );
        if !valid {
            return Err(
                "native emitter: owned OpBitCount operand and result shapes are inconsistent"
                    .to_string(),
            );
        }
    }
    if instruction.class.opcode == Op::BitReverse {
        let base_shape = instruction
            .operands
            .first()
            .and_then(operand_type)
            .and_then(|ty| numeric_type_shape(ty, definitions));
        if !matches!(base_shape, Some((Op::TypeInt, 32, _))) {
            return Err(
                "native emitter: owned OpBitReverse base is not a Vulkan 1.2 32-bit integer shape"
                    .to_string(),
            );
        }
    }
    if matches!(
        instruction.class.opcode,
        Op::BitFieldInsert | Op::BitFieldSExtract | Op::BitFieldUExtract
    ) {
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let Some(base_type) = instruction.operands.first().and_then(operand_type) else {
            return Ok(());
        };
        let insert_matches = instruction.class.opcode != Op::BitFieldInsert
            || instruction.operands.get(1).and_then(operand_type) == Some(result_type);
        let scalar_tail = if instruction.class.opcode == Op::BitFieldInsert {
            instruction.operands.get(2..4)
        } else {
            instruction.operands.get(1..3)
        };
        let scalar_tail_matches = scalar_tail.is_some_and(|operands| {
            operands.iter().all(|operand| {
                operand_type(operand)
                    .and_then(|ty| numeric_type_shape(ty, definitions))
                    .is_some_and(|(opcode, _, lanes)| opcode == Op::TypeInt && lanes == 1)
            })
        });
        if base_type != result_type
            || !insert_matches
            || !scalar_tail_matches
            || !matches!(
                numeric_type_shape(result_type, definitions),
                Some((Op::TypeInt, 32, _))
            )
        {
            return Err(format!(
                "native emitter: owned {:?} operands are not a Vulkan 1.2 bit-field shape",
                instruction.class.opcode
            ));
        }
    }
    if matches!(instruction.class.opcode, Op::Any | Op::All) {
        let operand_shape = instruction
            .operands
            .first()
            .and_then(operand_type)
            .and_then(|ty| scalar_type_shape(ty, definitions));
        let result_shape = instruction
            .result_type
            .and_then(|ty| scalar_type_shape(ty, definitions));
        if result_shape != Some((Op::TypeBool, 1))
            || !matches!(operand_shape, Some((Op::TypeBool, lanes)) if lanes > 1)
        {
            return Err(format!(
                "native emitter: owned {:?} requires a Boolean vector and scalar Boolean result",
                instruction.class.opcode
            ));
        }
    }
    if matches!(instruction.class.opcode, Op::IsNan | Op::IsInf) {
        let operand_shape = instruction
            .operands
            .first()
            .and_then(operand_type)
            .and_then(|ty| scalar_type_shape(ty, definitions));
        let result_shape = instruction
            .result_type
            .and_then(|ty| scalar_type_shape(ty, definitions));
        if !matches!(
            (operand_shape, result_shape),
            (Some((Op::TypeFloat, operand_lanes)), Some((Op::TypeBool, result_lanes)))
                if operand_lanes == result_lanes
        ) {
            return Err(format!(
                "native emitter: owned {:?} operand and result shapes are inconsistent",
                instruction.class.opcode
            ));
        }
    }
    if instruction.class.opcode == Op::VectorTimesScalar {
        let [vector, scalar] = instruction.operands.as_slice() else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let component_type =
            vector_type_shape(result_type, definitions).and_then(|(component, _)| {
                definitions
                    .get(&component)
                    .is_some_and(|definition| definition.class.opcode == Op::TypeFloat)
                    .then_some(component)
            });
        if operand_type(vector) != Some(result_type)
            || operand_type(scalar) != component_type
            || component_type.is_none()
        {
            return Err(
                "native emitter: owned OpVectorTimesScalar operands do not match its float-vector result"
                    .to_string(),
            );
        }
    }
    if instruction.class.opcode == Op::Dot {
        let [left, right] = instruction.operands.as_slice() else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let left_type = operand_type(left);
        let valid = definitions
            .get(&result_type)
            .is_some_and(|definition| definition.class.opcode == Op::TypeFloat)
            && left_type == operand_type(right)
            && left_type.is_some_and(|ty| {
                vector_type_shape(ty, definitions)
                    .is_some_and(|(component, _)| component == result_type)
            });
        if !valid {
            return Err(
                "native emitter: owned OpDot operands do not match its float-scalar result"
                    .to_string(),
            );
        }
    }
    if matches!(instruction.class.opcode, Op::DPdx | Op::DPdy | Op::Fwidth) {
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let operand_matches =
            instruction.operands.first().and_then(operand_type) == Some(result_type);
        if !operand_matches
            || !matches!(
                numeric_type_shape(result_type, definitions),
                Some((Op::TypeFloat, 32, _))
            )
        {
            return Err(format!(
                "native emitter: owned {:?} does not have an identical 32-bit float operand and result",
                instruction.class.opcode
            ));
        }
    }
    if instruction.class.opcode == Op::Select {
        let [condition, left, right] = instruction.operands.as_slice() else {
            return Ok(());
        };
        let Some(result_type) = instruction.result_type else {
            return Ok(());
        };
        let condition_shape =
            operand_type(condition).and_then(|ty| scalar_type_shape(ty, definitions));
        let result_lanes =
            vector_type_shape(result_type, definitions).map_or(1, |(_, lanes)| lanes);
        if !matches!(condition_shape, Some((Op::TypeBool, 1)))
            && condition_shape != Some((Op::TypeBool, result_lanes))
        {
            return Err(
                "native emitter: owned OpSelect condition does not match its result lane shape"
                    .to_string(),
            );
        }
        let result_is_pointer = definitions
            .get(&result_type)
            .is_some_and(|definition| definition.class.opcode == Op::TypePointer);
        if !result_is_pointer
            && (operand_type(left) != Some(result_type) || operand_type(right) != Some(result_type))
        {
            return Err(
                "native emitter: owned OpSelect objects do not match its result type".to_string(),
            );
        }
    }
    Ok(())
}

pub(crate) fn scalar_width_capability(opcode: Op, width: u32) -> Option<spirv::Capability> {
    match (opcode, width) {
        (Op::TypeInt, 8) => Some(spirv::Capability::Int8),
        (Op::TypeInt, 16) => Some(spirv::Capability::Int16),
        (Op::TypeInt, 64) => Some(spirv::Capability::Int64),
        (Op::TypeFloat, 16) => Some(spirv::Capability::Float16),
        (Op::TypeFloat, 64) => Some(spirv::Capability::Float64),
        _ => None,
    }
}

fn vector_type_shape(ty: Word, definitions: &HashMap<Word, &Instruction>) -> Option<(Word, u32)> {
    let definition = definitions.get(&ty)?;
    let [Operand::IdRef(component), Operand::LiteralBit32(count)] = definition.operands.as_slice()
    else {
        return None;
    };
    (definition.class.opcode == Op::TypeVector).then_some((*component, *count))
}

fn integer_constant_value(id: Word, definitions: &HashMap<Word, &Instruction>) -> Option<u64> {
    let definition = definitions.get(&id)?;
    if !matches!(definition.class.opcode, Op::Constant | Op::SpecConstant) {
        return None;
    }
    match definition.operands.as_slice() {
        [Operand::LiteralBit32(value)] => Some(u64::from(*value)),
        [Operand::LiteralBit64(value)] => Some(*value),
        _ => None,
    }
}

fn indexed_composite_type(
    mut ty: Word,
    indices: &[Operand],
    definitions: &HashMap<Word, &Instruction>,
) -> Result<Word, String> {
    if indices.is_empty() {
        return Err(
            "native emitter: owned composite operation has an empty index path".to_string(),
        );
    }
    for index in indices {
        let Operand::LiteralBit32(index) = index else {
            return Err(
                "native emitter: owned composite operation has a non-literal index".to_string(),
            );
        };
        let Some(composite) = definitions.get(&ty) else {
            return Err(
                "native emitter: owned composite operation indexes an undefined type".to_string(),
            );
        };
        ty = match composite.class.opcode {
            Op::TypeStruct => match composite.operands.get(*index as usize) {
                Some(Operand::IdRef(member)) => *member,
                _ => {
                    return Err(
                        "native emitter: owned composite operation index is out of bounds"
                            .to_string(),
                    );
                }
            },
            Op::TypeCooperativeMatrixKHR => match composite.operands.first() {
                Some(Operand::IdRef(component)) => *component,
                _ => {
                    return Err(
                        "native emitter: owned composite operation indexes a malformed type"
                            .to_string(),
                    );
                }
            },
            Op::TypeVector | Op::TypeMatrix => {
                let [Operand::IdRef(element), Operand::LiteralBit32(count)] =
                    composite.operands.as_slice()
                else {
                    return Err(
                        "native emitter: owned composite operation indexes a malformed type"
                            .to_string(),
                    );
                };
                if index >= count {
                    return Err(
                        "native emitter: owned composite operation index is out of bounds"
                            .to_string(),
                    );
                }
                *element
            }
            Op::TypeArray => {
                let [Operand::IdRef(element), Operand::IdRef(length)] =
                    composite.operands.as_slice()
                else {
                    return Err(
                        "native emitter: owned composite operation indexes a malformed type"
                            .to_string(),
                    );
                };
                if integer_constant_value(*length, definitions)
                    .is_none_or(|length| u64::from(*index) >= length)
                {
                    return Err(
                        "native emitter: owned composite operation index is out of bounds"
                            .to_string(),
                    );
                }
                *element
            }
            _ => {
                return Err(
                    "native emitter: owned composite operation indexes a non-composite type"
                        .to_string(),
                );
            }
        };
    }
    Ok(ty)
}

fn composite_constituents_match(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> bool {
    let Some(result_type) = instruction.result_type else {
        return false;
    };
    let Some(composite) = definitions.get(&result_type) else {
        return false;
    };
    let constituent_type = |operand: &Operand| {
        let Operand::IdRef(value) = operand else {
            return None;
        };
        value_types.get(value).copied()
    };
    match composite.class.opcode {
        Op::TypeStruct => {
            instruction.operands.len() == composite.operands.len()
                && instruction.operands.iter().zip(&composite.operands).all(
                    |(constituent, member)| match member {
                        Operand::IdRef(member) => constituent_type(constituent) == Some(*member),
                        _ => false,
                    },
                )
        }
        Op::TypeMatrix => {
            let [Operand::IdRef(column), Operand::LiteralBit32(count)] =
                composite.operands.as_slice()
            else {
                return false;
            };
            instruction.operands.len() == *count as usize
                && instruction
                    .operands
                    .iter()
                    .all(|constituent| constituent_type(constituent) == Some(*column))
        }
        Op::TypeArray => {
            let [Operand::IdRef(element), Operand::IdRef(length)] = composite.operands.as_slice()
            else {
                return false;
            };
            integer_constant_value(*length, definitions).is_some_and(|length| {
                usize::try_from(length).ok() == Some(instruction.operands.len())
                    && instruction
                        .operands
                        .iter()
                        .all(|constituent| constituent_type(constituent) == Some(*element))
            })
        }
        Op::TypeVector => {
            let [Operand::IdRef(component), Operand::LiteralBit32(count)] =
                composite.operands.as_slice()
            else {
                return false;
            };
            if instruction.class.opcode != Op::CompositeConstruct {
                return instruction.operands.len() == *count as usize
                    && instruction
                        .operands
                        .iter()
                        .all(|constituent| constituent_type(constituent) == Some(*component));
            }
            instruction
                .operands
                .iter()
                .try_fold(0_u32, |lanes, constituent| {
                    let constituent = constituent_type(constituent)?;
                    let added = if constituent == *component {
                        1
                    } else {
                        let (constituent_component, constituent_lanes) =
                            vector_type_shape(constituent, definitions)?;
                        (constituent_component == *component).then_some(constituent_lanes)?
                    };
                    lanes.checked_add(added)
                })
                == Some(*count)
        }
        Op::TypeCooperativeMatrixKHR => {
            let Some(Operand::IdRef(component)) = composite.operands.first() else {
                return false;
            };
            instruction.class.opcode == Op::CompositeConstruct
                && instruction.operands.len() == 1
                && constituent_type(&instruction.operands[0]) == Some(*component)
        }
        _ => false,
    }
}

fn owned_composite_instruction_error(
    instruction: &Instruction,
    definitions: &HashMap<Word, &Instruction>,
    value_types: &HashMap<Word, Word>,
) -> Result<(), String> {
    match instruction.class.opcode {
        Op::CompositeConstruct | Op::ConstantComposite | Op::SpecConstantComposite => {
            if !composite_constituents_match(instruction, definitions, value_types) {
                return Err(format!(
                    "native emitter: owned {:?} constituents do not match its result type",
                    instruction.class.opcode
                ));
            }
        }
        Op::CompositeExtract => {
            let [Operand::IdRef(composite), indices @ ..] = instruction.operands.as_slice() else {
                return Ok(());
            };
            let selected = value_types
                .get(composite)
                .copied()
                .ok_or_else(|| {
                    "native emitter: owned OpCompositeExtract operand has no value type".to_string()
                })
                .and_then(|ty| indexed_composite_type(ty, indices, definitions))?;
            if instruction.result_type != Some(selected) {
                return Err(
                    "native emitter: owned OpCompositeExtract result does not match its index path"
                        .to_string(),
                );
            }
        }
        Op::CompositeInsert => {
            let [Operand::IdRef(object), Operand::IdRef(composite), indices @ ..] =
                instruction.operands.as_slice()
            else {
                return Ok(());
            };
            let composite_type = value_types.get(composite).copied().ok_or_else(|| {
                "native emitter: owned OpCompositeInsert composite has no value type".to_string()
            })?;
            if instruction.result_type != Some(composite_type) {
                return Err(
                    "native emitter: owned OpCompositeInsert composite does not match its result type"
                        .to_string(),
                );
            }
            let selected = indexed_composite_type(composite_type, indices, definitions)?;
            if value_types.get(object) != Some(&selected) {
                return Err(
                    "native emitter: owned OpCompositeInsert object does not match its index path"
                        .to_string(),
                );
            }
        }
        Op::VectorExtractDynamic => {
            let [Operand::IdRef(vector), Operand::IdRef(index)] = instruction.operands.as_slice()
            else {
                return Ok(());
            };
            let vector_shape = value_types
                .get(vector)
                .and_then(|ty| vector_type_shape(*ty, definitions));
            let index_is_integer = value_types
                .get(index)
                .and_then(|ty| definitions.get(ty))
                .is_some_and(|ty| ty.class.opcode == Op::TypeInt);
            if vector_shape.map(|(component, _)| component) != instruction.result_type
                || !index_is_integer
            {
                return Err(
                    "native emitter: owned OpVectorExtractDynamic types are inconsistent"
                        .to_string(),
                );
            }
        }
        Op::VectorInsertDynamic => {
            let [Operand::IdRef(vector), Operand::IdRef(component), Operand::IdRef(index)] =
                instruction.operands.as_slice()
            else {
                return Ok(());
            };
            let vector_type = value_types.get(vector).copied();
            let vector_shape = vector_type.and_then(|ty| vector_type_shape(ty, definitions));
            let index_is_integer = value_types
                .get(index)
                .and_then(|ty| definitions.get(ty))
                .is_some_and(|ty| ty.class.opcode == Op::TypeInt);
            if vector_type != instruction.result_type
                || vector_shape.map(|(component, _)| component)
                    != value_types.get(component).copied()
                || !index_is_integer
            {
                return Err(
                    "native emitter: owned OpVectorInsertDynamic types are inconsistent"
                        .to_string(),
                );
            }
        }
        Op::VectorShuffle => {
            let [Operand::IdRef(left), Operand::IdRef(right), selectors @ ..] =
                instruction.operands.as_slice()
            else {
                return Ok(());
            };
            let left_shape = value_types
                .get(left)
                .and_then(|ty| vector_type_shape(*ty, definitions));
            let right_shape = value_types
                .get(right)
                .and_then(|ty| vector_type_shape(*ty, definitions));
            let result_shape = instruction
                .result_type
                .and_then(|ty| vector_type_shape(ty, definitions));
            let Some((component, left_lanes)) = left_shape else {
                return Err(
                    "native emitter: owned OpVectorShuffle types are inconsistent".to_string(),
                );
            };
            let Some((right_component, right_lanes)) = right_shape else {
                return Err(
                    "native emitter: owned OpVectorShuffle types are inconsistent".to_string(),
                );
            };
            let Some((result_component, result_lanes)) = result_shape else {
                return Err(
                    "native emitter: owned OpVectorShuffle types are inconsistent".to_string(),
                );
            };
            let lane_bound = left_lanes.saturating_add(right_lanes);
            if component != right_component
                || component != result_component
                || selectors.len() != result_lanes as usize
                || selectors.iter().any(|selector| {
                    !matches!(selector, Operand::LiteralBit32(lane) if *lane == u32::MAX || *lane < lane_bound)
                })
            {
                return Err("native emitter: owned OpVectorShuffle types are inconsistent".to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_type_global_value_opcode(opcode: Op) -> bool {
    opcode.is_type()
        || opcode.is_constant()
        || matches!(
            opcode,
            Op::Line | Op::NoLine | Op::Undef | Op::Variable | Op::UntypedVariableKHR
        )
}

fn is_module_only_opcode(opcode: Op) -> bool {
    matches!(
        opcode,
        Op::Capability
            | Op::Extension
            | Op::ExtInstImport
            | Op::MemoryModel
            | Op::EntryPoint
            | Op::ExecutionMode
            | Op::ExecutionModeId
            | Op::SourceContinued
            | Op::Source
            | Op::SourceExtension
            | Op::String
            | Op::Name
            | Op::MemberName
            | Op::ModuleProcessed
    ) || opcode.is_annotation()
        || opcode.is_type()
        || opcode.is_constant()
}

fn owned_module_layout_error(module: &crate::spirv_module::Module) -> Option<String> {
    let Some(header) = module.header.as_ref() else {
        return Some("native emitter: owned module has no SPIR-V header".to_string());
    };
    let (major, minor) = header.version();
    if header.magic_number != spirv::MAGIC_NUMBER
        || header.version & 0xff00_00ff != 0
        || major != 1
        || minor > 5
        || header.bound == 0
        || header.reserved_word != 0
    {
        return Some(
            "native emitter: owned module has an invalid Vulkan 1.2 SPIR-V header".to_string(),
        );
    }
    if module.capabilities.is_empty() {
        return Some("native emitter: owned module declares no capability".to_string());
    }

    let sections: &[(&str, &[Instruction], fn(Op) -> bool)] = &[
        ("capability", &module.capabilities, |opcode| {
            opcode == Op::Capability
        }),
        ("extension", &module.extensions, |opcode| {
            opcode == Op::Extension
        }),
        (
            "extended-instruction import",
            &module.ext_inst_imports,
            |opcode| opcode == Op::ExtInstImport,
        ),
        ("entry-point", &module.entry_points, |opcode| {
            opcode == Op::EntryPoint
        }),
        ("execution-mode", &module.execution_modes, |opcode| {
            matches!(opcode, Op::ExecutionMode | Op::ExecutionModeId)
        }),
        (
            "source/string debug",
            &module.debug_string_source,
            |opcode| {
                matches!(
                    opcode,
                    Op::SourceContinued | Op::Source | Op::SourceExtension | Op::String
                )
            },
        ),
        ("name debug", &module.debug_names, |opcode| {
            matches!(opcode, Op::Name | Op::MemberName)
        }),
        (
            "module-processed debug",
            &module.debug_module_processed,
            |opcode| opcode == Op::ModuleProcessed,
        ),
        ("annotation", &module.annotations, Op::is_annotation),
        (
            "type/constant/global",
            &module.types_global_values,
            is_type_global_value_opcode,
        ),
    ];
    for (name, instructions, accepts) in sections {
        if instructions
            .iter()
            .any(|instruction| !accepts(instruction.class.opcode))
        {
            return Some(format!(
                "native emitter: owned module {name} section contains an instruction from another logical section"
            ));
        }
    }

    let Some(memory_model) = module.memory_model.as_ref() else {
        return Some("native emitter: owned module has no OpMemoryModel".to_string());
    };
    if memory_model.class.opcode != Op::MemoryModel {
        return Some(
            "native emitter: owned module memory-model section contains an instruction from another logical section"
                .to_string(),
        );
    }

    for function in &module.functions {
        for block in &function.blocks {
            if block
                .label
                .as_ref()
                .is_some_and(|label| label.class.opcode != Op::Label)
            {
                return Some(
                    "native emitter: owned function block label is not OpLabel".to_string(),
                );
            }
            if let Some(instruction) = block.instructions.iter().find(|instruction| {
                is_module_only_opcode(instruction.class.opcode)
                    || matches!(
                        instruction.class.opcode,
                        Op::Function | Op::FunctionParameter | Op::FunctionEnd | Op::Label
                    )
            }) {
                return Some(format!(
                    "native emitter: owned function block contains {:?} from another logical section",
                    instruction.class.opcode
                ));
            }
        }
    }
    None
}

fn execution_mode_form_matches(instruction: &Instruction) -> bool {
    let [Operand::IdRef(_), Operand::ExecutionMode(_), parameters @ ..] =
        instruction.operands.as_slice()
    else {
        return false;
    };
    let id_parameters = !parameters.is_empty()
        && parameters
            .iter()
            .all(|parameter| matches!(parameter, Operand::IdRef(_)));
    match instruction.class.opcode {
        Op::ExecutionMode => !id_parameters,
        Op::ExecutionModeId => id_parameters,
        _ => false,
    }
}

fn owned_module_linkage_error(
    module: &crate::spirv_module::Module,
    definitions: &HashMap<Word, &Instruction>,
) -> Result<(), String> {
    let capabilities = module
        .capabilities
        .iter()
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [Operand::Capability(capability)] => Some(*capability),
            _ => None,
        })
        .collect::<HashSet<_>>();
    if module.entry_points.is_empty() {
        if !capabilities.contains(&spirv::Capability::Linkage) {
            return Err(
                "native emitter: owned module has no entry point or Linkage capability".to_string(),
            );
        }
    } else if !capabilities.contains(&spirv::Capability::Shader) {
        return Err("native emitter: owned entry points require Shader capability".to_string());
    }

    let functions = module
        .functions
        .iter()
        .filter_map(|function| {
            function
                .def
                .as_ref()
                .and_then(|definition| definition.result_id)
                .map(|id| (id, function))
        })
        .collect::<HashMap<_, _>>();
    let global_variables = module
        .types_global_values
        .iter()
        .filter(|instruction| {
            matches!(
                instruction.class.opcode,
                Op::Variable | Op::UntypedVariableKHR
            )
        })
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction)))
        .collect::<HashMap<_, _>>();
    let mut entry_ids = HashSet::default();
    let mut entry_names = HashSet::default();
    let version = module
        .header
        .as_ref()
        .expect("layout checked header")
        .version();

    for entry_point in &module.entry_points {
        let [Operand::ExecutionModel(model), Operand::IdRef(function_id), Operand::LiteralString(name), interface @ ..] =
            entry_point.operands.as_slice()
        else {
            return Err("native emitter: owned OpEntryPoint has malformed operands".to_string());
        };
        let Some(function) = functions.get(function_id) else {
            return Err(
                "native emitter: owned OpEntryPoint target is not an owned function".to_string(),
            );
        };
        if function.blocks.is_empty() {
            return Err("native emitter: owned OpEntryPoint targets a declaration".to_string());
        }
        let definition = function
            .def
            .as_ref()
            .expect("function map contains definitions");
        let return_is_void = definition
            .result_type
            .and_then(|return_type| definitions.get(&return_type))
            .is_some_and(|return_type| return_type.class.opcode == Op::TypeVoid);
        if !return_is_void
            || !function.parameters.is_empty()
            || definition.operands.first()
                != Some(&Operand::FunctionControl(spirv::FunctionControl::NONE))
        {
            return Err(
                "native emitter: owned OpEntryPoint function is not void, parameterless, and uncontrolled"
                    .to_string(),
            );
        }
        if !entry_names.insert((*model, name.as_str())) {
            return Err(
                "native emitter: owned module repeats an entry-point model and name".to_string(),
            );
        }
        entry_ids.insert(*function_id);

        let mut interface_ids = HashSet::default();
        for operand in interface {
            let Operand::IdRef(id) = operand else {
                return Err(
                    "native emitter: owned OpEntryPoint has malformed interface".to_string()
                );
            };
            let Some(variable) = global_variables.get(id) else {
                return Err(
                    "native emitter: owned OpEntryPoint interface id is not a global variable"
                        .to_string(),
                );
            };
            if !interface_ids.insert(*id) {
                return Err(
                    "native emitter: owned OpEntryPoint repeats an interface variable".to_string(),
                );
            }
            if version < (1, 4)
                && !matches!(
                    variable.operands.first(),
                    Some(Operand::StorageClass(
                        spirv::StorageClass::Input | spirv::StorageClass::Output
                    ))
                )
            {
                return Err(
                    "native emitter: pre-1.4 OpEntryPoint interface contains a non-IO variable"
                        .to_string(),
                );
            }
        }

        let mut pending = vec![*function_id];
        let mut visited = HashSet::default();
        let mut required_interface = HashSet::default();
        while let Some(current) = pending.pop() {
            if !visited.insert(current) {
                continue;
            }
            let Some(function) = functions.get(&current) else {
                continue;
            };
            for instruction in function.all_inst_iter() {
                if *model != spirv::ExecutionModel::Fragment
                    && matches!(
                        instruction.class.opcode,
                        Op::DPdx
                            | Op::DPdy
                            | Op::Fwidth
                            | Op::ImageQueryLod
                            | Op::ImageSampleImplicitLod
                    )
                {
                    return Err(
                        "native emitter: owned derivative instruction is reachable from a non-Fragment entry point"
                            .to_string(),
                    );
                }
                if instruction.class.opcode == Op::ControlBarrier
                    && !matches!(
                        *model,
                        spirv::ExecutionModel::TessellationControl
                            | spirv::ExecutionModel::GLCompute
                    )
                {
                    return Err(
                        "native emitter: owned OpControlBarrier is reachable from an unsupported execution model"
                            .to_string(),
                    );
                }
                for id in referenced_ids(instruction) {
                    if let Some(variable) = global_variables.get(&id) {
                        let required = version >= (1, 4)
                            || matches!(
                                variable.operands.first(),
                                Some(Operand::StorageClass(
                                    spirv::StorageClass::Input | spirv::StorageClass::Output
                                ))
                            );
                        if required {
                            required_interface.insert(id);
                        }
                    }
                }
                if instruction.class.opcode == Op::FunctionCall {
                    if let Some(Operand::IdRef(callee)) = instruction.operands.first() {
                        pending.push(*callee);
                    }
                }
            }
        }
        if !required_interface.is_subset(&interface_ids) {
            return Err(
                "native emitter: owned OpEntryPoint omits a global used by its static call tree"
                    .to_string(),
            );
        }
    }

    for execution_mode in &module.execution_modes {
        if !execution_mode_form_matches(execution_mode) {
            return Err(
                "native emitter: owned execution mode uses the wrong literal/id instruction form"
                    .to_string(),
            );
        }
        let Some(Operand::IdRef(entry_id)) = execution_mode.operands.first() else {
            unreachable!("execution-mode form was checked above");
        };
        if !entry_ids.contains(entry_id) {
            return Err(
                "native emitter: owned execution mode does not target an entry point".to_string(),
            );
        }
    }

    Ok(())
}

fn owned_module_environment_error(module: &crate::spirv_module::Module) -> Result<(), String> {
    let version = module
        .header
        .as_ref()
        .expect("layout checked header")
        .version();
    let declared_capabilities = module
        .capabilities
        .iter()
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [Operand::Capability(capability)] => Some(*capability),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let extensions = module
        .extensions
        .iter()
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [Operand::LiteralString(extension)] => Some(extension.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();

    let mut capabilities = declared_capabilities.clone();
    loop {
        let implied = capabilities
            .iter()
            .filter_map(|capability| {
                let operand = Operand::Capability(*capability);
                let requirement =
                    crate::spirv_binary::operand_declaration_requirements(&operand).next()?;
                match requirement.capabilities {
                    [requirement] => Some(*requirement),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        let previous_len = capabilities.len();
        capabilities.extend(implied);
        if capabilities.len() == previous_len {
            break;
        }
    }

    for instruction in module.all_inst_iter() {
        let Some((required_capabilities, required_extensions)) =
            crate::spirv_binary::instruction_declaration_requirements(instruction.class.opcode)
        else {
            return Err("native emitter: owned instruction has no declaration grammar".to_string());
        };
        if !required_capabilities.is_empty()
            && required_capabilities
                .iter()
                .all(|capability| !capabilities.contains(capability))
        {
            return Err(format!(
                "native emitter: owned {:?} lacks an enabling capability",
                instruction.class.opcode
            ));
        }
        if !required_extensions.is_empty()
            && required_extensions
                .iter()
                .all(|extension| !extensions.contains(extension))
        {
            return Err(format!(
                "native emitter: owned {:?} lacks an enabling extension",
                instruction.class.opcode
            ));
        }
        if instruction.class.opcode == Op::DemoteToHelperInvocation
            && !extensions.contains("SPV_EXT_demote_to_helper_invocation")
        {
            return Err(
                "native emitter: SPIR-V 1.5 OpDemoteToHelperInvocation lacks its extension"
                    .to_string(),
            );
        }

        for operand in &instruction.operands {
            if matches!(
                operand,
                Operand::AddressingModel(_) | Operand::MemoryModel(_)
            ) {
                continue;
            }
            for requirement in crate::spirv_binary::operand_declaration_requirements(operand) {
                if !requirement.capabilities.is_empty()
                    && requirement
                        .capabilities
                        .iter()
                        .all(|capability| !capabilities.contains(capability))
                {
                    return Err(format!(
                        "native emitter: owned {:?} operand lacks an enabling capability",
                        instruction.class.opcode
                    ));
                }
                if requirement
                    .min_core_version
                    .is_some_and(|min_version| version >= min_version)
                {
                    continue;
                }
                if requirement.extensions.is_empty() {
                    if !requirement.capabilities.is_empty() {
                        continue;
                    }
                    return Err(format!(
                        "native emitter: owned {:?} operand is unavailable in SPIR-V {}.{}",
                        instruction.class.opcode, version.0, version.1
                    ));
                }
                if requirement
                    .extensions
                    .iter()
                    .all(|extension| !extensions.contains(extension))
                {
                    return Err(format!(
                        "native emitter: owned {:?} operand lacks an enabling extension",
                        instruction.class.opcode
                    ));
                }
            }
        }

        if instruction.class.opcode == Op::TypeInt
            && !matches!(
                instruction.operands.get(1),
                Some(Operand::LiteralBit32(0 | 1))
            )
        {
            return Err("native emitter: owned OpTypeInt has invalid signedness".to_string());
        }
        let width = match (instruction.class.opcode, instruction.operands.as_slice()) {
            (Op::TypeInt, [Operand::LiteralBit32(width), Operand::LiteralBit32(_)])
            | (Op::TypeFloat, [Operand::LiteralBit32(width)]) => Some(*width),
            (Op::TypeInt, _) => {
                return Err(
                    "native emitter: owned OpTypeInt has an unsupported scalar width".to_string(),
                );
            }
            (Op::TypeFloat, _) => {
                return Err(
                    "native emitter: owned OpTypeFloat has an unsupported scalar width".to_string(),
                );
            }
            _ => None,
        };
        if let Some(width) = width {
            if !matches!(
                (instruction.class.opcode, width),
                (Op::TypeInt, 8 | 16 | 32 | 64) | (Op::TypeFloat, 16 | 32 | 64)
            ) {
                return Err(format!(
                    "native emitter: owned {:?} has an unsupported scalar width",
                    instruction.class.opcode
                ));
            }
        }
        let required_scalar_capability =
            width.and_then(|width| scalar_width_capability(instruction.class.opcode, width));
        if required_scalar_capability.is_some_and(|capability| !capabilities.contains(&capability))
        {
            return Err(format!(
                "native emitter: owned {:?} scalar width lacks its capability",
                instruction.class.opcode
            ));
        }

        if instruction.class.opcode == Op::TypeVector {
            let Some(Operand::LiteralBit32(component_count)) = instruction.operands.get(1) else {
                unreachable!("operand grammar checked before the environment");
            };
            if !matches!(*component_count, 2 | 3 | 4 | 8 | 16) {
                return Err(
                    "native emitter: owned OpTypeVector has an invalid component count".to_string(),
                );
            }
            if matches!(*component_count, 8 | 16)
                && !capabilities.contains(&spirv::Capability::Vector16)
            {
                return Err(
                    "native emitter: owned wide OpTypeVector lacks Vector16 capability".to_string(),
                );
            }
        }
    }

    let memory_model = module
        .memory_model
        .as_ref()
        .expect("layout checked memory model");
    let [Operand::AddressingModel(addressing), Operand::MemoryModel(memory)] =
        memory_model.operands.as_slice()
    else {
        return Err("native emitter: owned OpMemoryModel has malformed operands".to_string());
    };
    if !matches!(
        *addressing,
        spirv::AddressingModel::Logical | spirv::AddressingModel::PhysicalStorageBuffer64
    ) || !matches!(
        *memory,
        spirv::MemoryModel::GLSL450 | spirv::MemoryModel::Vulkan
    ) {
        return Err(
            "native emitter: owned memory model is not available in Vulkan 1.2".to_string(),
        );
    }
    if *addressing == spirv::AddressingModel::PhysicalStorageBuffer64
        && !capabilities.contains(&spirv::Capability::PhysicalStorageBufferAddresses)
    {
        return Err("native emitter: PhysicalStorageBuffer64 lacks its capability".to_string());
    }
    if *memory == spirv::MemoryModel::Vulkan
        && !capabilities.contains(&spirv::Capability::VulkanMemoryModel)
    {
        return Err("native emitter: Vulkan memory model lacks its capability".to_string());
    }
    Ok(())
}

pub(crate) fn owned_module_failure(
    module: &crate::spirv_module::Module,
) -> Option<OwnedModuleFailure> {
    owned_module_failures(module, 1).into_iter().next()
}

pub(crate) fn owned_module_failures(
    module: &crate::spirv_module::Module,
    limit: usize,
) -> Vec<OwnedModuleFailure> {
    let mut failures: Vec<OwnedModuleFailure> = Vec::new();
    macro_rules! record {
        ($failure:expr) => {{
            failures.push($failure);
            if failures.len() >= limit {
                return failures;
            }
        }};
    }
    macro_rules! record_invalid {
        ($error:expr) => {
            record!(OwnedModuleFailure::Invalid($error))
        };
    }
    let invalid = |error| vec![OwnedModuleFailure::Invalid(error)];
    if let Some(error) = owned_module_layout_error(module) {
        return invalid(error);
    }
    let mut definitions = HashMap::default();
    let mut value_types = HashMap::default();
    for instruction in module.all_inst_iter() {
        let Some((expects_result_type, expects_result_id)) =
            crate::spirv_binary::instruction_result_shape(instruction.class.opcode)
        else {
            return invalid(
                "native emitter: owned instruction has no core grammar entry".to_string(),
            );
        };
        if instruction.result_type.is_some() != expects_result_type
            || instruction.result_id.is_some() != expects_result_id
        {
            return invalid(format!(
                "native emitter: owned {:?} has malformed result fields",
                instruction.class.opcode
            ));
        }
        if !crate::spirv_binary::instruction_operands_match(
            instruction.class.opcode,
            &instruction.operands,
        ) {
            return invalid(format!(
                "native emitter: owned {:?} has operands outside the core grammar",
                instruction.class.opcode
            ));
        }
        let Some(id) = instruction.result_id else {
            continue;
        };
        if id == 0 {
            return invalid("native emitter: owned module defines result id zero".to_string());
        }
        if module
            .header
            .as_ref()
            .is_some_and(|header| id >= header.bound)
        {
            return invalid("native emitter: owned result id exceeds the module bound".to_string());
        }
        if let Some(first) = definitions.insert(id, instruction) {
            return invalid(format!(
                "native emitter: owned module defines a result id more than once: %{id} by {:?} and again by {:?}",
                first.class.opcode, instruction.class.opcode
            ));
        }
        if let Some(result_type) = instruction.result_type {
            value_types.insert(id, result_type);
        }
    }
    if let Some(error) = module.all_inst_iter().find_map(|instruction| {
        let id =
            referenced_ids(instruction).find(|id| *id == 0 || !definitions.contains_key(id))?;
        Some(format!(
            "native emitter: owned {:?}{} references undefined id %{id}",
            instruction.class.opcode,
            instruction
                .result_id
                .map(|result| format!(" %{result}"))
                .unwrap_or_default()
        ))
    }) {
        if let Some(path) = crate::env_vars::owned_dump() {
            let words = module.assemble();
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let _ = std::fs::write(&path, bytes);
        }
        record_invalid!(error);
    }
    if module.all_inst_iter().any(|instruction| {
        instruction.result_type.is_some_and(|result_type| {
            !definitions
                .get(&result_type)
                .is_some_and(|definition| definition.class.opcode.is_type())
        })
    }) {
        record_invalid!(
            "native emitter: owned instruction result type is not a type declaration".to_string()
        );
    }
    if let Err(error) = owned_type_operand_error(module, &definitions) {
        record_invalid!(error);
    }
    if let Some(error) = module
        .annotations
        .iter()
        .find_map(|instruction| owned_annotation_error(instruction, &definitions))
    {
        record_invalid!(error);
    }
    if let Some(error) = owned_block_layout_error(module, &definitions) {
        record_invalid!(error);
    }
    if let Some(failure) = module
        .all_inst_iter()
        .find_map(|instruction| owned_access_chain_error(instruction, &definitions, &value_types))
    {
        record!(if failure.raw_buffer_eligible {
            OwnedModuleFailure::RawBufferConstruction(failure.error)
        } else {
            OwnedModuleFailure::TypeConstruction(failure.error)
        });
    }
    if let Some(error) = module
        .all_inst_iter()
        .find_map(|instruction| owned_memory_type_error(instruction, &definitions, &value_types))
    {
        if let Some(path) = crate::env_vars::owned_dump() {
            let words = module.assemble();
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let _ = std::fs::write(&path, bytes);
        }

        record!(OwnedModuleFailure::TypeConstruction(error));
    }
    if let Some(error) = module
        .types_global_values
        .iter()
        .find_map(|instruction| {
            owned_variable_type_error(instruction, &definitions, &value_types, true)
        })
        .or_else(|| {
            module.functions.iter().find_map(|function| {
                function.blocks.iter().find_map(|block| {
                    block.instructions.iter().find_map(|instruction| {
                        owned_variable_type_error(instruction, &definitions, &value_types, false)
                    })
                })
            })
        })
    {
        record!(OwnedModuleFailure::TypeConstruction(error));
    }
    if let Some(error) = module.all_inst_iter().find_map(|instruction| {
        owned_value_instruction_error(instruction, &definitions, &value_types).err()
    }) {
        record_invalid!(error);
    }
    if let Some(error) = module.all_inst_iter().find_map(|instruction| {
        owned_composite_instruction_error(instruction, &definitions, &value_types).err()
    }) {
        record_invalid!(error);
    }
    let logical = module.memory_model.as_ref().is_some_and(|instruction| {
        instruction.operands.first()
            == Some(&Operand::AddressingModel(spirv::AddressingModel::Logical))
    });
    let has_capability = |capability| {
        module
            .capabilities
            .iter()
            .any(|instruction| instruction.operands.as_slice() == [Operand::Capability(capability)])
    };
    let variable_pointers = has_capability(spirv::Capability::VariablePointers);
    let variable_pointers_storage_buffer =
        has_capability(spirv::Capability::VariablePointersStorageBuffer);
    if logical {
        for instruction in module.all_inst_iter() {
            if instruction.class.opcode != Op::Phi
                || !matches!(
                    instruction
                        .result_type
                        .and_then(|ty| pointer_type_shape(ty, &definitions)),
                    Some((spirv::StorageClass::StorageBuffer, _))
                )
            {
                continue;
            }
            let roots = instruction
                .operands
                .chunks_exact(2)
                .filter_map(|pair| match pair.first() {
                    Some(Operand::IdRef(value)) => pointer_root(*value, &definitions),
                    _ => None,
                })
                .collect::<HashSet<_>>();
            if roots.len() > 1 {
                record!(OwnedModuleFailure::RawBufferConstruction(
                    "native emitter: cross-root StorageBuffer pointer phi requires address-domain construction"
                        .to_string(),
                ));
                break;
            }
        }
    }
    if let Some(error) = module.all_inst_iter().find_map(|instruction| {
        owned_pointer_construction_error(
            instruction,
            &definitions,
            &value_types,
            logical,
            variable_pointers,
            variable_pointers_storage_buffer,
        )
    }) {
        record!(OwnedModuleFailure::TypeConstruction(error));
    }
    if let Err(error) = owned_module_environment_error(module) {
        record_invalid!(error);
    }

    let mut local_owners = HashMap::default();
    for (function_index, function) in module.functions.iter().enumerate() {
        for id in function
            .parameters
            .iter()
            .filter_map(|parameter| parameter.result_id)
            .chain(function.blocks.iter().flat_map(|block| {
                block
                    .label
                    .iter()
                    .chain(&block.instructions)
                    .filter_map(|instruction| instruction.result_id)
            }))
        {
            local_owners.insert(id, function_index);
        }
    }
    for (function_index, function) in module.functions.iter().enumerate() {
        if function.all_inst_iter().any(|instruction| {
            referenced_ids(instruction).any(|id| {
                local_owners
                    .get(&id)
                    .is_some_and(|owner| *owner != function_index)
            })
        }) {
            record_invalid!(
                "native emitter: owned function references an id from another function".to_string()
            );
            break;
        }
    }

    if let Some(error) = module.functions.iter().find_map(|function| {
        if let Err(error) = owned_function_contract_error(function, &definitions, &value_types) {
            return Some(error);
        }
        None
    }) {
        record_invalid!(error);
    }
    if let Some(error) = module.all_inst_iter().find_map(|instruction| {
        owned_function_call_contract_error(instruction, &definitions, &value_types).err()
    }) {
        record_invalid!(error);
    }
    if let Err(error) = owned_module_linkage_error(module, &definitions) {
        record_invalid!(error);
    }

    if let Some(failure) = module.functions.iter().find_map(|function| {
        if function.blocks.is_empty() {
            return None;
        }
        OwnedCfg::new(function)
            .and_then(|cfg| cfg.check(function, &definitions, &value_types))
            .err()
            .map(OwnedModuleFailure::CfgConstruction)
    }) {
        record!(failure);
    }
    failures
}

#[cfg(test)]
fn owned_failure_message(failure: OwnedModuleFailure) -> String {
    match failure {
        OwnedModuleFailure::Invalid(message)
        | OwnedModuleFailure::CfgConstruction(message)
        | OwnedModuleFailure::TypeConstruction(message)
        | OwnedModuleFailure::RawBufferConstruction(message) => message,
    }
}

#[cfg(test)]
fn owned_module_cfg_error(module: &crate::spirv_module::Module) -> Option<String> {
    owned_module_failure(module).map(owned_failure_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Instruction, Module, ModuleHeader};

    fn inst(opcode: Op, operands: Vec<Operand>) -> Instruction {
        Instruction::new(opcode, None, None, operands)
    }

    fn selection_merge(target: Word) -> Instruction {
        inst(
            Op::SelectionMerge,
            vec![
                Operand::IdRef(target),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        )
    }

    fn block(label: Word, instructions: Vec<Instruction>) -> Block {
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions,
        }
    }

    fn module_with_blocks(blocks: Vec<Block>) -> Module {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(100);
        header.set_version(1, 5);
        module.header = Some(header);
        module.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::Shader)],
        ));
        module.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::Int16)],
        ));
        module.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::Float16)],
        ));
        module.memory_model = Some(inst(
            Op::MemoryModel,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(spirv::MemoryModel::GLSL450),
            ],
        ));
        module.entry_points.push(inst(
            Op::EntryPoint,
            vec![
                Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                Operand::IdRef(32),
                Operand::LiteralString("main".to_string()),
            ],
        ));
        module.execution_modes.push(inst(
            Op::ExecutionMode,
            vec![
                Operand::IdRef(32),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
            ],
        ));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(30), vec![]),
            Instruction::new(Op::TypeFunction, None, Some(31), vec![Operand::IdRef(30)]),
            Instruction::new(Op::TypeBool, None, Some(9), vec![]),
            Instruction::new(Op::ConstantTrue, Some(9), Some(10), vec![]),
            Instruction::new(Op::ConstantFalse, Some(9), Some(11), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(12),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(13),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(14),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(12),
                ],
            ),
        ];
        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(30),
            Some(32),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(31),
            ],
        ));
        function.end = Some(inst(Op::FunctionEnd, vec![]));
        function.blocks = blocks;
        module.functions.push(function);
        module
    }

    fn module_with_composite_instruction(instruction: Instruction) -> Module {
        let global_instruction = instruction.class.opcode.is_constant();
        let block_instructions = if global_instruction {
            vec![inst(Op::Return, vec![])]
        } else {
            vec![instruction.clone(), inst(Op::Return, vec![])]
        };
        let mut module = module_with_blocks(vec![block(1, block_instructions)]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(15),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(16),
                vec![Operand::IdRef(12), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(17),
                vec![Operand::IdRef(15), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(18),
                vec![Operand::IdRef(12), Operand::IdRef(15)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(19),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(20),
                vec![Operand::IdRef(12), Operand::IdRef(19)],
            ),
            Instruction::new(Op::Undef, Some(16), Some(21), vec![]),
            Instruction::new(Op::Undef, Some(16), Some(22), vec![]),
            Instruction::new(
                Op::Constant,
                Some(15),
                Some(23),
                vec![Operand::LiteralBit32(0x3f80_0000)],
            ),
            Instruction::new(Op::Undef, Some(18), Some(24), vec![]),
            Instruction::new(Op::Undef, Some(20), Some(25), vec![]),
            Instruction::new(Op::Undef, Some(17), Some(26), vec![]),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(27),
                vec![Operand::IdRef(9), Operand::LiteralBit32(2)],
            ),
            Instruction::new(Op::Undef, Some(27), Some(28), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(29),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::Undef, Some(29), Some(33), vec![]),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(34),
                vec![Operand::LiteralBit32(16)],
            ),
            Instruction::new(Op::Undef, Some(34), Some(35), vec![]),
        ]);
        if global_instruction {
            module.types_global_values.push(instruction);
        }
        module
    }

    fn module_with_atomic_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.types_global_values.extend([
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(41),
                vec![Operand::LiteralBit32(spirv::Scope::Workgroup as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(42),
                vec![Operand::LiteralBit32(
                    spirv::MemorySemantics::WORKGROUP_MEMORY.bits(),
                )],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(36),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Workgroup),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(38),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Workgroup),
                    Operand::IdRef(15),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(36),
                Some(37),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::Variable,
                Some(38),
                Some(39),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
        ]);
        module.entry_points[0]
            .operands
            .extend([Operand::IdRef(37), Operand::IdRef(39)]);
        module
    }

    fn module_with_glsl_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeVector,
                None,
                Some(43),
                vec![Operand::IdRef(15), Operand::LiteralBit32(4)],
            ),
            Instruction::new(Op::Undef, Some(43), Some(44), vec![]),
        ]);
        module.ext_inst_imports.push(Instruction::new(
            Op::ExtInstImport,
            None,
            Some(45),
            vec![Operand::LiteralString("GLSL.std.450".to_string())],
        ));
        module
    }

    fn module_with_sampled_image_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeImage,
                None,
                Some(43),
                vec![
                    Operand::IdRef(15),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            ),
            Instruction::new(Op::TypeSampler, None, Some(44), vec![]),
            Instruction::new(
                Op::TypeSampledImage,
                None,
                Some(45),
                vec![Operand::IdRef(43)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(46),
                vec![
                    Operand::StorageClass(spirv::StorageClass::UniformConstant),
                    Operand::IdRef(43),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(47),
                vec![
                    Operand::StorageClass(spirv::StorageClass::UniformConstant),
                    Operand::IdRef(44),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(46),
                Some(48),
                vec![Operand::StorageClass(spirv::StorageClass::UniformConstant)],
            ),
            Instruction::new(
                Op::Variable,
                Some(47),
                Some(49),
                vec![Operand::StorageClass(spirv::StorageClass::UniformConstant)],
            ),
        ]);
        module.entry_points[0]
            .operands
            .extend([Operand::IdRef(48), Operand::IdRef(49)]);
        let instructions = &mut module.functions[0].blocks[0].instructions;
        instructions.splice(
            0..0,
            [
                Instruction::new(Op::Load, Some(43), Some(50), vec![Operand::IdRef(48)]),
                Instruction::new(Op::Load, Some(44), Some(51), vec![Operand::IdRef(49)]),
                Instruction::new(
                    Op::SampledImage,
                    Some(45),
                    Some(52),
                    vec![Operand::IdRef(50), Operand::IdRef(51)],
                ),
            ],
        );
        module
    }

    fn module_with_image_query_lod(instruction: Instruction) -> Module {
        let mut module = module_with_sampled_image_instruction(instruction);
        module.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::ImageQuery)],
        ));
        module.entry_points[0].operands[0] =
            Operand::ExecutionModel(spirv::ExecutionModel::Fragment);
        module.execution_modes = vec![inst(
            Op::ExecutionMode,
            vec![
                Operand::IdRef(32),
                Operand::ExecutionMode(spirv::ExecutionMode::OriginUpperLeft),
            ],
        )];
        module
    }

    fn module_with_image_query_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::ImageQuery)],
        ));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeImage,
                None,
                Some(43),
                vec![
                    Operand::IdRef(15),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(46),
                vec![
                    Operand::StorageClass(spirv::StorageClass::UniformConstant),
                    Operand::IdRef(43),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(46),
                Some(48),
                vec![Operand::StorageClass(spirv::StorageClass::UniformConstant)],
            ),
        ]);
        module.entry_points[0].operands.push(Operand::IdRef(48));
        module.functions[0].blocks[0].instructions.insert(
            0,
            Instruction::new(Op::Load, Some(43), Some(50), vec![Operand::IdRef(48)]),
        );
        module
    }

    fn module_with_texel_access_instruction(
        instruction: Instruction,
        sampled: u32,
        multisampled: bool,
    ) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.capabilities.extend([
            inst(
                Op::Capability,
                vec![Operand::Capability(
                    spirv::Capability::StorageImageReadWithoutFormat,
                )],
            ),
            inst(
                Op::Capability,
                vec![Operand::Capability(
                    spirv::Capability::StorageImageWriteWithoutFormat,
                )],
            ),
        ]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeImage,
                None,
                Some(43),
                vec![
                    Operand::IdRef(15),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(u32::from(multisampled)),
                    Operand::LiteralBit32(sampled),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(46),
                vec![
                    Operand::StorageClass(spirv::StorageClass::UniformConstant),
                    Operand::IdRef(43),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(46),
                Some(48),
                vec![Operand::StorageClass(spirv::StorageClass::UniformConstant)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(53),
                vec![Operand::IdRef(15), Operand::LiteralBit32(4)],
            ),
            Instruction::new(Op::Undef, Some(53), Some(54), vec![]),
        ]);
        module.entry_points[0].operands.push(Operand::IdRef(48));
        module.functions[0].blocks[0].instructions.insert(
            0,
            Instruction::new(Op::Load, Some(43), Some(50), vec![Operand::IdRef(48)]),
        );
        module
    }

    fn module_with_image_texel_pointer_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeImage,
                None,
                Some(43),
                vec![
                    Operand::IdRef(12),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(2),
                    Operand::ImageFormat(spirv::ImageFormat::R32ui),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(46),
                vec![
                    Operand::StorageClass(spirv::StorageClass::UniformConstant),
                    Operand::IdRef(43),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(46),
                Some(48),
                vec![Operand::StorageClass(spirv::StorageClass::UniformConstant)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(56),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Image),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(57),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Image),
                    Operand::IdRef(15),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(58),
                vec![Operand::LiteralBit32(spirv::Scope::Device as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(59),
                vec![Operand::LiteralBit32(1)],
            ),
        ]);
        module.annotations.extend([
            inst(
                Op::Decorate,
                vec![
                    Operand::IdRef(48),
                    Operand::Decoration(spirv::Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                vec![
                    Operand::IdRef(48),
                    Operand::Decoration(spirv::Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
        ]);
        module.entry_points[0].operands.push(Operand::IdRef(48));
        module.functions[0].blocks[0].instructions.insert(
            1,
            Instruction::new(
                Op::AtomicUMax,
                Some(12),
                Some(60),
                vec![
                    Operand::IdRef(40),
                    Operand::IdScope(58),
                    Operand::IdMemorySemantics(13),
                    Operand::IdRef(13),
                ],
            ),
        );
        module
    }

    fn module_with_barrier_instruction(instruction: Instruction) -> Module {
        let mut module =
            module_with_blocks(vec![block(1, vec![instruction, inst(Op::Return, vec![])])]);
        module.types_global_values.extend([
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(41),
                vec![Operand::LiteralBit32(spirv::Scope::Workgroup as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(42),
                vec![Operand::LiteralBit32(spirv::Scope::Subgroup as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(43),
                vec![Operand::LiteralBit32(spirv::Scope::Device as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(44),
                vec![Operand::LiteralBit32(
                    (spirv::MemorySemantics::ACQUIRE_RELEASE
                        | spirv::MemorySemantics::WORKGROUP_MEMORY)
                        .bits(),
                )],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(45),
                vec![Operand::LiteralBit32(
                    (spirv::MemorySemantics::ACQUIRE_RELEASE
                        | spirv::MemorySemantics::IMAGE_MEMORY)
                        .bits(),
                )],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(46),
                vec![Operand::LiteralBit32(
                    (spirv::MemorySemantics::ACQUIRE
                        | spirv::MemorySemantics::RELEASE
                        | spirv::MemorySemantics::WORKGROUP_MEMORY)
                        .bits(),
                )],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(47),
                vec![Operand::LiteralBit32(
                    spirv::MemorySemantics::ACQUIRE_RELEASE.bits(),
                )],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(48),
                vec![Operand::LiteralBit32(99)],
            ),
            Instruction::new(Op::Undef, Some(12), Some(49), vec![]),
        ]);
        module
    }

    fn module_with_group_non_uniform_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.capabilities.extend(
            [
                spirv::Capability::GroupNonUniform,
                spirv::Capability::GroupNonUniformVote,
                spirv::Capability::GroupNonUniformBallot,
                spirv::Capability::GroupNonUniformShuffle,
                spirv::Capability::GroupNonUniformShuffleRelative,
                spirv::Capability::GroupNonUniformArithmetic,
                spirv::Capability::GroupNonUniformClustered,
            ]
            .into_iter()
            .map(|capability| inst(Op::Capability, vec![Operand::Capability(capability)])),
        );
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeInt,
                None,
                Some(41),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(1)],
            ),
            Instruction::new(Op::Undef, Some(41), Some(42), vec![]),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(43),
                vec![Operand::IdRef(12), Operand::LiteralBit32(4)],
            ),
            Instruction::new(Op::Undef, Some(43), Some(44), vec![]),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(45),
                vec![Operand::LiteralBit32(spirv::Scope::Subgroup as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(46),
                vec![Operand::LiteralBit32(spirv::Scope::Workgroup as u32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(47),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(48),
                vec![Operand::LiteralBit32(3)],
            ),
            Instruction::new(Op::Undef, Some(12), Some(49), vec![]),
        ]);
        module
    }

    fn module_with_access_chain_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_composite_instruction(instruction);
        module.capabilities.extend([
            inst(
                Op::Capability,
                vec![Operand::Capability(spirv::Capability::Matrix)],
            ),
            inst(
                Op::Capability,
                vec![Operand::Capability(spirv::Capability::VariablePointers)],
            ),
        ]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(36),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(20),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(38),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(18),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(41),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(15),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(42),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(16),
                ],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(44),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(44),
                Some(45),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::Undef, Some(12), Some(46), vec![]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(50),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Private),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::TypeMatrix,
                None,
                Some(51),
                vec![Operand::IdRef(17), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(52),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(51),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(54),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(17),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(55),
                vec![Operand::LiteralBit32(1)],
            ),
        ]);
        module.functions[0].blocks[0].instructions.splice(
            0..0,
            [
                Instruction::new(
                    Op::Variable,
                    Some(36),
                    Some(37),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(38),
                    Some(39),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(42),
                    Some(43),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(52),
                    Some(53),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
            ],
        );
        module
    }

    fn module_with_memory_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_blocks(vec![block(
            50,
            vec![
                Instruction::new(
                    Op::Variable,
                    Some(14),
                    Some(33),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(14),
                    Some(34),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(16),
                    Some(35),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                instruction,
                inst(Op::Return, vec![]),
            ],
        )]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(15),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(16),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Function),
                    Operand::IdRef(15),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(15),
                Some(36),
                vec![Operand::LiteralBit32(0)],
            ),
        ]);
        module
    }

    fn module_with_sample_operation_instruction(instruction: Instruction) -> Module {
        let mut module = module_with_sampled_image_instruction(instruction);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeVector,
                None,
                Some(53),
                vec![Operand::IdRef(15), Operand::LiteralBit32(4)],
            ),
            Instruction::new(Op::Undef, Some(53), Some(54), vec![]),
            Instruction::new(
                Op::ConstantComposite,
                Some(16),
                Some(55),
                vec![Operand::IdRef(13), Operand::IdRef(13)],
            ),
        ]);
        module.entry_points[0].operands[0] =
            Operand::ExecutionModel(spirv::ExecutionModel::Fragment);
        module.execution_modes = vec![inst(
            Op::ExecutionMode,
            vec![
                Operand::IdRef(32),
                Operand::ExecutionMode(spirv::ExecutionMode::OriginUpperLeft),
            ],
        )];
        module
    }

    fn glsl_instruction(
        number: spirv::GlslStd450Op,
        result_type: Word,
        arguments: &[Word],
    ) -> Instruction {
        Instruction::new(
            Op::ExtInst,
            Some(result_type),
            Some(40),
            std::iter::once(Operand::IdRef(45))
                .chain(std::iter::once(Operand::LiteralExtInstInteger(
                    number as u32,
                )))
                .chain(arguments.iter().copied().map(Operand::IdRef))
                .collect(),
        )
    }

    fn assert_owned_invalid(module: &Module, expected: &str) {
        match owned_module_failure(module) {
            Some(OwnedModuleFailure::Invalid(error)) => assert_eq!(error, expected),
            Some(OwnedModuleFailure::CfgConstruction(error)) => {
                panic!("invalid value contract was misclassified as CFG construction: {error}")
            }
            Some(OwnedModuleFailure::TypeConstruction(error)) => {
                panic!("invalid value contract was misclassified as type construction: {error}")
            }
            Some(OwnedModuleFailure::RawBufferConstruction(error)) => {
                panic!(
                    "invalid value contract was misclassified as raw-buffer construction: {error}"
                )
            }
            None => panic!("invalid value contract was accepted"),
        }
    }

    fn assert_owned_type_construction(module: &Module, expected: &str) {
        match owned_module_failure(module) {
            Some(OwnedModuleFailure::TypeConstruction(error)) => assert_eq!(error, expected),
            Some(OwnedModuleFailure::Invalid(error)) => {
                panic!("type construction was misclassified as invalid: {error}")
            }
            Some(OwnedModuleFailure::CfgConstruction(error)) => {
                panic!("type construction was misclassified as CFG construction: {error}")
            }
            Some(OwnedModuleFailure::RawBufferConstruction(error)) => {
                panic!("type construction was misclassified as raw-buffer construction: {error}")
            }
            None => panic!("invalid type construction was accepted"),
        }
    }

    fn module_with_annotation(annotation: Instruction) -> Module {
        let mut module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(18),
                vec![Operand::IdRef(12), Operand::IdRef(12)],
            ),
            Instruction::new(
                Op::Constant,
                Some(12),
                Some(19),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(20),
                vec![Operand::IdRef(12), Operand::IdRef(19)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(21),
                vec![
                    Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                    Operand::IdRef(12),
                ],
            ),
        ]);
        module.annotations.push(annotation);
        module
    }

    fn member_decoration(
        target: Word,
        member: u32,
        decoration: spirv::Decoration,
        operand: Option<Operand>,
    ) -> Instruction {
        inst(
            Op::MemberDecorate,
            [
                Some(Operand::IdRef(target)),
                Some(Operand::LiteralBit32(member)),
                Some(Operand::Decoration(decoration)),
                operand,
            ]
            .into_iter()
            .flatten()
            .collect(),
        )
    }

    fn module_with_storage_block(include_second_offset: bool) -> Module {
        let mut module = module_with_annotation(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(18),
                Operand::Decoration(spirv::Decoration::Block),
            ],
        ));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(22),
                vec![
                    Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                    Operand::IdRef(18),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(22),
                Some(23),
                vec![Operand::StorageClass(spirv::StorageClass::StorageBuffer)],
            ),
        ]);
        module.entry_points[0].operands.push(Operand::IdRef(23));
        module.annotations.extend([
            member_decoration(
                18,
                0,
                spirv::Decoration::Offset,
                Some(Operand::LiteralBit32(0)),
            ),
            inst(
                Op::Decorate,
                vec![
                    Operand::IdRef(23),
                    Operand::Decoration(spirv::Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                vec![
                    Operand::IdRef(23),
                    Operand::Decoration(spirv::Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
        ]);
        if include_second_offset {
            module.annotations.push(member_decoration(
                18,
                1,
                spirv::Decoration::Offset,
                Some(Operand::LiteralBit32(4)),
            ));
        }
        module
    }

    #[test]
    fn owned_module_enforces_annotation_target_contracts() {
        let decorate = |target, decoration, operand| {
            inst(
                Op::Decorate,
                std::iter::once(Operand::IdRef(target))
                    .chain(std::iter::once(Operand::Decoration(decoration)))
                    .chain(operand)
                    .collect(),
            )
        };
        let literal = |value| Some(Operand::LiteralBit32(value));

        let valid_array =
            module_with_annotation(decorate(20, spirv::Decoration::ArrayStride, literal(4)));
        assert!(owned_module_failure(&valid_array).is_none());
        let valid_buffer_pointer =
            module_with_annotation(decorate(21, spirv::Decoration::ArrayStride, literal(4)));
        if let Some(failure) = owned_module_failure(&valid_buffer_pointer) {
            let message = owned_failure_message(failure);
            panic!("valid buffer-pointer annotation was rejected: {message}");
        }

        assert_owned_invalid(
            &module_with_annotation(decorate(18, spirv::Decoration::ArrayStride, literal(4))),
            "native emitter: owned ArrayStride target is not an array or stridable buffer pointer type",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(14, spirv::Decoration::ArrayStride, literal(4))),
            "native emitter: owned ArrayStride target is not an array or stridable buffer pointer type",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(20, spirv::Decoration::Block, None)),
            "native emitter: owned block decoration target is not a structure type",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(13, spirv::Decoration::SpecId, literal(0))),
            "native emitter: owned SpecId target is not a specialization constant",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(13, spirv::Decoration::Location, literal(0))),
            "native emitter: owned interface decoration target is not a variable",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(
                13,
                spirv::Decoration::BuiltIn,
                Some(Operand::BuiltIn(spirv::BuiltIn::SampleId)),
            )),
            "native emitter: owned BuiltIn target does not match its built-in contract",
        );
        assert_owned_invalid(
            &module_with_annotation(decorate(18, spirv::Decoration::Offset, literal(0))),
            "native emitter: owned Offset is not a member decoration",
        );

        assert_owned_invalid(
            &module_with_annotation(inst(
                Op::MemberDecorate,
                vec![
                    Operand::IdRef(20),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(spirv::Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            )),
            "native emitter: owned OpMemberDecorate target is not a structure type",
        );
        assert_owned_invalid(
            &module_with_annotation(inst(
                Op::MemberDecorate,
                vec![
                    Operand::IdRef(18),
                    Operand::LiteralBit32(2),
                    Operand::Decoration(spirv::Decoration::Offset),
                    Operand::LiteralBit32(8),
                ],
            )),
            "native emitter: owned OpMemberDecorate member index is out of bounds",
        );
    }

    #[test]
    fn owned_annotation_check_matches_vulkan_validation() {
        let valid = module_with_annotation(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(20),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(4),
            ],
        ));
        let invalid = module_with_annotation(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(18),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(4),
            ],
        ));
        assert_owned_invalid(
            &invalid,
            "native emitter: owned ArrayStride target is not an array or stridable buffer pointer type",
        );

        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_annotation_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned annotation contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed annotation contract"
        );
    }

    #[test]
    fn owned_module_enforces_recursive_block_layout_completeness() {
        let mut complete = module_with_storage_block(true);
        assert!(owned_module_failure(&complete).is_none());

        assert_owned_invalid(
            &module_with_storage_block(false),
            "native emitter: owned Block layout structure member lacks Offset",
        );

        let struct_definition = complete
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(18))
            .expect("structure type");
        struct_definition.operands[0] = Operand::IdRef(20);
        assert_owned_invalid(
            &complete,
            "native emitter: owned Block layout array lacks ArrayStride",
        );
        complete.annotations.push(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(20),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(4),
            ],
        ));
        assert!(owned_module_failure(&complete).is_none());

        let mut nested = module_with_storage_block(true);
        nested.types_global_values.push(Instruction::new(
            Op::TypeStruct,
            None,
            Some(24),
            vec![Operand::IdRef(12)],
        ));
        nested
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(18))
            .expect("outer structure type")
            .operands[0] = Operand::IdRef(24);
        assert_owned_invalid(
            &nested,
            "native emitter: owned Block layout structure member lacks Offset",
        );
        nested.annotations.push(member_decoration(
            24,
            0,
            spirv::Decoration::Offset,
            Some(Operand::LiteralBit32(0)),
        ));
        assert!(owned_module_failure(&nested).is_none());

        let mut matrix = module_with_storage_block(true);
        matrix.types_global_values.extend([
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(24),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(25),
                vec![Operand::IdRef(24), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeMatrix,
                None,
                Some(26),
                vec![Operand::IdRef(25), Operand::LiteralBit32(2)],
            ),
        ]);
        matrix
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(18))
            .expect("matrix structure type")
            .operands[0] = Operand::IdRef(26);
        assert_owned_invalid(
            &matrix,
            "native emitter: owned Block layout matrix member lacks MatrixStride",
        );
        matrix.annotations.push(member_decoration(
            18,
            0,
            spirv::Decoration::MatrixStride,
            Some(Operand::LiteralBit32(8)),
        ));
        assert_owned_invalid(
            &matrix,
            "native emitter: owned Block layout matrix member does not have exactly one major order",
        );
        matrix
            .annotations
            .push(member_decoration(18, 0, spirv::Decoration::ColMajor, None));
        assert!(owned_module_failure(&matrix).is_none());

        let zero_array_stride = module_with_annotation(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(20),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(0),
            ],
        ));
        assert_owned_invalid(
            &zero_array_stride,
            "native emitter: owned ArrayStride decoration has zero stride",
        );
    }

    #[test]
    fn owned_block_layout_check_matches_vulkan_validation() {
        let valid = module_with_storage_block(true);
        let invalid = module_with_storage_block(false);
        assert_owned_invalid(
            &invalid,
            "native emitter: owned Block layout structure member lacks Offset",
        );

        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_block_layout_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the complete owned Block layout");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the incomplete Block layout"
        );
    }

    #[test]
    fn owned_module_enforces_conversion_and_nonpointer_select_types() {
        let cases = [
            Instruction::new(Op::UConvert, Some(15), Some(40), vec![Operand::IdRef(13)]),
            Instruction::new(
                Op::ConvertFToU,
                Some(12),
                Some(40),
                vec![Operand::IdRef(13)],
            ),
            Instruction::new(Op::FConvert, Some(17), Some(40), vec![Operand::IdRef(23)]),
        ];
        for instruction in cases {
            let opcode = instruction.class.opcode;
            let module = module_with_composite_instruction(instruction);
            assert_owned_invalid(
                &module,
                &format!(
                    "native emitter: owned {opcode:?} source and result shapes are inconsistent"
                ),
            );
        }

        let select = module_with_composite_instruction(Instruction::new(
            Op::Select,
            Some(12),
            Some(40),
            vec![Operand::IdRef(10), Operand::IdRef(13), Operand::IdRef(23)],
        ));
        assert_owned_invalid(
            &select,
            "native emitter: owned OpSelect objects do not match its result type",
        );
    }

    #[test]
    fn owned_module_enforces_bit_instruction_shapes() {
        let cases = [
            (
                Instruction::new(
                    Op::ShiftLeftLogical,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(13), Operand::IdRef(13)],
                ),
                "native emitter: owned ShiftLeftLogical operand and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::ShiftRightLogical,
                    Some(12),
                    Some(40),
                    vec![Operand::IdRef(13), Operand::IdRef(23)],
                ),
                "native emitter: owned ShiftRightLogical operand and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::ShiftRightArithmetic,
                    Some(16),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::IdRef(13)],
                ),
                "native emitter: owned ShiftRightArithmetic operand and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::Bitcast,
                    Some(17),
                    Some(40),
                    vec![Operand::IdRef(13)],
                ),
                "native emitter: owned OpBitcast source and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::Bitcast,
                    Some(18),
                    Some(40),
                    vec![Operand::IdRef(24)],
                ),
                "native emitter: owned OpBitcast source and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::BitReverse,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(23)],
                ),
                "native emitter: owned BitReverse has an invalid result type class",
            ),
            (
                Instruction::new(
                    Op::BitCount,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(13)],
                ),
                "native emitter: owned OpBitCount operand and result shapes are inconsistent",
            ),
            (
                Instruction::new(
                    Op::BitReverse,
                    Some(29),
                    Some(40),
                    vec![Operand::IdRef(33)],
                ),
                "native emitter: owned OpBitReverse base is not a Vulkan 1.2 32-bit integer shape",
            ),
            (
                Instruction::new(
                    Op::BitFieldInsert,
                    Some(12),
                    Some(40),
                    vec![
                        Operand::IdRef(13),
                        Operand::IdRef(23),
                        Operand::IdRef(13),
                        Operand::IdRef(13),
                    ],
                ),
                "native emitter: owned BitFieldInsert operands are not a Vulkan 1.2 bit-field shape",
            ),
            (
                Instruction::new(
                    Op::BitFieldUExtract,
                    Some(29),
                    Some(40),
                    vec![
                        Operand::IdRef(33),
                        Operand::IdRef(13),
                        Operand::IdRef(13),
                    ],
                ),
                "native emitter: owned BitFieldUExtract operands are not a Vulkan 1.2 bit-field shape",
            ),
        ];
        for (instruction, expected) in cases {
            let module = module_with_composite_instruction(instruction);
            assert_owned_invalid(&module, expected);
        }

        let valid_bitcast = module_with_composite_instruction(Instruction::new(
            Op::Bitcast,
            Some(15),
            Some(40),
            vec![Operand::IdRef(13)],
        ));
        assert!(owned_module_failure(&valid_bitcast).is_none());

        let identity_bitcast = module_with_composite_instruction(Instruction::new(
            Op::Bitcast,
            Some(16),
            Some(40),
            vec![Operand::IdRef(21)],
        ));
        assert!(owned_module_failure(&identity_bitcast).is_none());

        let valid_shift = module_with_composite_instruction(Instruction::new(
            Op::ShiftLeftLogical,
            Some(16),
            Some(40),
            vec![Operand::IdRef(21), Operand::IdRef(22)],
        ));
        assert!(owned_module_failure(&valid_shift).is_none());

        let valid_bitfield = module_with_composite_instruction(Instruction::new(
            Op::BitFieldSExtract,
            Some(12),
            Some(40),
            vec![Operand::IdRef(13), Operand::IdRef(13), Operand::IdRef(13)],
        ));
        assert!(owned_module_failure(&valid_bitfield).is_none());
    }

    #[test]
    fn owned_module_enforces_boolean_reduction_and_float_classification_shapes() {
        let cases = [
            (
                Instruction::new(Op::Any, Some(9), Some(40), vec![Operand::IdRef(10)]),
                "native emitter: owned Any requires a Boolean vector and scalar Boolean result",
            ),
            (
                Instruction::new(Op::All, Some(27), Some(40), vec![Operand::IdRef(28)]),
                "native emitter: owned All requires a Boolean vector and scalar Boolean result",
            ),
            (
                Instruction::new(Op::IsNan, Some(9), Some(40), vec![Operand::IdRef(13)]),
                "native emitter: owned IsNan operand and result shapes are inconsistent",
            ),
            (
                Instruction::new(Op::IsInf, Some(27), Some(40), vec![Operand::IdRef(23)]),
                "native emitter: owned IsInf operand and result shapes are inconsistent",
            ),
        ];
        for (instruction, expected) in cases {
            let module = module_with_composite_instruction(instruction);
            assert_owned_invalid(&module, expected);
        }

        let valid_any = module_with_composite_instruction(Instruction::new(
            Op::Any,
            Some(9),
            Some(40),
            vec![Operand::IdRef(28)],
        ));
        assert!(owned_module_failure(&valid_any).is_none());

        let valid_is_nan = module_with_composite_instruction(Instruction::new(
            Op::IsNan,
            Some(9),
            Some(40),
            vec![Operand::IdRef(23)],
        ));
        assert!(owned_module_failure(&valid_is_nan).is_none());
    }

    #[test]
    fn owned_module_enforces_vector_algebra_and_derivative_shapes() {
        let cases = [
            (
                Instruction::new(
                    Op::VectorTimesScalar,
                    Some(17),
                    Some(40),
                    vec![Operand::IdRef(26), Operand::IdRef(13)],
                ),
                "native emitter: owned OpVectorTimesScalar operands do not match its float-vector result",
            ),
            (
                Instruction::new(
                    Op::Dot,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::IdRef(22)],
                ),
                "native emitter: owned OpDot operands do not match its float-scalar result",
            ),
            (
                Instruction::new(Op::DPdx, Some(34), Some(40), vec![Operand::IdRef(35)]),
                "native emitter: owned DPdx does not have an identical 32-bit float operand and result",
            ),
        ];
        for (instruction, expected) in cases {
            let module = module_with_composite_instruction(instruction);
            assert_owned_invalid(&module, expected);
        }

        let valid_vector_scale = module_with_composite_instruction(Instruction::new(
            Op::VectorTimesScalar,
            Some(17),
            Some(40),
            vec![Operand::IdRef(26), Operand::IdRef(23)],
        ));
        assert!(owned_module_failure(&valid_vector_scale).is_none());

        let valid_dot = module_with_composite_instruction(Instruction::new(
            Op::Dot,
            Some(15),
            Some(40),
            vec![Operand::IdRef(26), Operand::IdRef(26)],
        ));
        assert!(owned_module_failure(&valid_dot).is_none());
    }

    #[test]
    fn owned_module_enforces_atomic_value_contracts() {
        let cases = [
            (
                Instruction::new(
                    Op::AtomicIAdd,
                    Some(15),
                    Some(40),
                    vec![
                        Operand::IdRef(37),
                        Operand::IdScope(41),
                        Operand::IdMemorySemantics(42),
                        Operand::IdRef(13),
                    ],
                ),
                "native emitter: owned AtomicIAdd result type does not match its pointer pointee",
            ),
            (
                Instruction::new(
                    Op::AtomicStore,
                    None,
                    None,
                    vec![
                        Operand::IdRef(37),
                        Operand::IdScope(41),
                        Operand::IdMemorySemantics(42),
                        Operand::IdRef(23),
                    ],
                ),
                "native emitter: owned AtomicStore value operands do not match its pointer pointee",
            ),
            (
                Instruction::new(
                    Op::AtomicCompareExchange,
                    Some(12),
                    Some(40),
                    vec![
                        Operand::IdRef(37),
                        Operand::IdScope(41),
                        Operand::IdMemorySemantics(42),
                        Operand::IdMemorySemantics(42),
                        Operand::IdRef(13),
                        Operand::IdRef(23),
                    ],
                ),
                "native emitter: owned AtomicCompareExchange value operands do not match its pointer pointee",
            ),
            (
                Instruction::new(
                    Op::AtomicIAdd,
                    Some(12),
                    Some(40),
                    vec![
                        Operand::IdRef(39),
                        Operand::IdScope(41),
                        Operand::IdMemorySemantics(42),
                        Operand::IdRef(23),
                    ],
                ),
                "native emitter: owned AtomicIAdd pointer does not target the required scalar type class",
            ),
            (
                Instruction::new(
                    Op::AtomicIAdd,
                    Some(12),
                    Some(40),
                    vec![
                        Operand::IdRef(37),
                        Operand::IdScope(33),
                        Operand::IdMemorySemantics(42),
                        Operand::IdRef(13),
                    ],
                ),
                "native emitter: owned AtomicIAdd scope and memory semantics are not 32-bit integer scalars",
            ),
        ];
        for (instruction, expected) in cases {
            let module = module_with_atomic_instruction(instruction);
            assert_owned_invalid(&module, expected);
        }

        let valid_integer = module_with_atomic_instruction(Instruction::new(
            Op::AtomicIAdd,
            Some(12),
            Some(40),
            vec![
                Operand::IdRef(37),
                Operand::IdScope(41),
                Operand::IdMemorySemantics(42),
                Operand::IdRef(13),
            ],
        ));
        assert!(owned_module_failure(&valid_integer).is_none());

        let mut valid_float = module_with_atomic_instruction(Instruction::new(
            Op::AtomicFAddEXT,
            Some(15),
            Some(40),
            vec![
                Operand::IdRef(39),
                Operand::IdScope(41),
                Operand::IdMemorySemantics(42),
                Operand::IdRef(23),
            ],
        ));
        valid_float.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::AtomicFloat32AddEXT)],
        ));
        valid_float.extensions.push(inst(
            Op::Extension,
            vec![Operand::LiteralString(
                "SPV_EXT_shader_atomic_float_add".to_string(),
            )],
        ));
        assert!(owned_module_failure(&valid_float).is_none());
    }

    #[test]
    fn owned_atomic_check_matches_vulkan_validation() {
        let module = module_with_atomic_instruction(Instruction::new(
            Op::AtomicIAdd,
            Some(12),
            Some(40),
            vec![
                Operand::IdRef(37),
                Operand::IdScope(41),
                Operand::IdMemorySemantics(42),
                Operand::IdRef(23),
            ],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned AtomicIAdd value operands do not match its pointer pointee",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_atomic_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed atomic value contract"
        );
    }

    #[test]
    fn owned_module_enforces_glsl_extended_instruction_contracts() {
        use spirv::GlslStd450Op as Glsl;

        let cases = [
            glsl_instruction(Glsl::FAbs, 12, &[23]),
            glsl_instruction(Glsl::Fma, 15, &[23, 23]),
            glsl_instruction(Glsl::SAbs, 15, &[23]),
            glsl_instruction(Glsl::FindILsb, 29, &[33]),
            glsl_instruction(Glsl::PackUnorm4x8, 12, &[26]),
            glsl_instruction(Glsl::UnpackUnorm2x16, 15, &[13]),
            glsl_instruction(Glsl::Ldexp, 17, &[26, 13]),
        ];
        for instruction in cases {
            let number = match instruction.operands[1] {
                Operand::LiteralExtInstInteger(number) => number,
                _ => unreachable!(),
            };
            let module = module_with_glsl_instruction(instruction);
            assert_owned_invalid(
                &module,
                &format!(
                    "native emitter: owned GLSL.std.450 instruction {number} violates its operand and result contract"
                ),
            );
        }

        let mut unknown = glsl_instruction(Glsl::FAbs, 15, &[23]);
        unknown.operands[1] = Operand::LiteralExtInstInteger(999);
        assert_owned_invalid(
            &module_with_glsl_instruction(unknown),
            "native emitter: owned GLSL.std.450 instruction is outside the emitted contract",
        );

        let mut unsupported_set =
            module_with_glsl_instruction(glsl_instruction(Glsl::FAbs, 15, &[23]));
        unsupported_set.ext_inst_imports[0].operands[0] =
            Operand::LiteralString("OpenCL.std".to_string());
        assert_owned_invalid(
            &unsupported_set,
            "native emitter: owned OpExtInst uses an unsupported extended instruction set",
        );

        let valid = [
            glsl_instruction(Glsl::FAbs, 15, &[23]),
            glsl_instruction(Glsl::SAbs, 12, &[13]),
            glsl_instruction(Glsl::Ldexp, 17, &[26, 21]),
            glsl_instruction(Glsl::PackUnorm4x8, 12, &[44]),
            glsl_instruction(Glsl::UnpackUnorm2x16, 17, &[13]),
            glsl_instruction(Glsl::FindILsb, 12, &[13]),
            glsl_instruction(Glsl::NClamp, 15, &[23, 23, 23]),
        ];
        for instruction in valid {
            assert!(owned_module_failure(&module_with_glsl_instruction(instruction)).is_none());
        }
    }

    #[test]
    fn owned_glsl_extended_instruction_check_matches_vulkan_validation() {
        let module =
            module_with_glsl_instruction(glsl_instruction(spirv::GlslStd450Op::FAbs, 12, &[23]));
        assert_owned_invalid(
            &module,
            "native emitter: owned GLSL.std.450 instruction 4 violates its operand and result contract",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_glsl_ext_inst_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed GLSL.std.450 value contract"
        );
    }

    #[test]
    fn owned_module_enforces_sampled_image_type_contracts() {
        let sampled_image = |result_type, image, sampler| {
            Instruction::new(
                Op::SampledImage,
                Some(result_type),
                Some(40),
                vec![Operand::IdRef(image), Operand::IdRef(sampler)],
            )
        };
        let image = |result_type, sampled| {
            Instruction::new(
                Op::Image,
                Some(result_type),
                Some(40),
                vec![Operand::IdRef(sampled)],
            )
        };

        for (instruction, opcode) in [
            (sampled_image(43, 50, 51), Op::SampledImage),
            (sampled_image(45, 51, 50), Op::SampledImage),
            (image(44, 52), Op::Image),
            (image(43, 50), Op::Image),
        ] {
            assert_owned_invalid(
                &module_with_sampled_image_instruction(instruction),
                &format!(
                    "native emitter: owned {opcode:?} image, sampler, and result types are inconsistent"
                ),
            );
        }

        for instruction in [sampled_image(45, 50, 51), image(43, 52)] {
            assert_eq!(
                owned_module_cfg_error(&module_with_sampled_image_instruction(instruction)),
                None
            );
        }
    }

    #[test]
    fn owned_sampled_image_check_matches_vulkan_validation() {
        let module = module_with_sampled_image_instruction(Instruction::new(
            Op::SampledImage,
            Some(43),
            Some(40),
            vec![Operand::IdRef(50), Operand::IdRef(51)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned SampledImage image, sampler, and result types are inconsistent",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_sampled_image_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed sampled-image type contract"
        );
    }

    #[test]
    fn owned_module_enforces_image_query_contracts() {
        let query = |opcode, result_type, operands: &[Word]| {
            Instruction::new(
                opcode,
                Some(result_type),
                Some(40),
                operands.iter().copied().map(Operand::IdRef).collect(),
            )
        };
        let invalid = [
            query(Op::ImageQuerySizeLod, 12, &[50, 13]),
            query(Op::ImageQuerySizeLod, 16, &[50, 23]),
            query(Op::ImageQuerySize, 16, &[50]),
            query(Op::ImageQueryLevels, 16, &[50]),
            query(Op::ImageQuerySamples, 12, &[50]),
        ];
        for instruction in invalid {
            let opcode = instruction.class.opcode;
            assert_owned_invalid(
                &module_with_image_query_instruction(instruction),
                &format!(
                    "native emitter: owned {opcode:?} violates its image-query type and dimensionality contract"
                ),
            );
        }

        for instruction in [
            query(Op::ImageQuerySizeLod, 16, &[50, 13]),
            query(Op::ImageQueryLevels, 12, &[50]),
        ] {
            assert_eq!(
                owned_module_cfg_error(&module_with_image_query_instruction(instruction)),
                None
            );
        }

        let mut storage = module_with_image_query_instruction(query(Op::ImageQuerySize, 16, &[50]));
        storage
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(43))
            .expect("image type")
            .operands[5] = Operand::LiteralBit32(2);
        assert_eq!(owned_module_cfg_error(&storage), None);

        let mut multisampled =
            module_with_image_query_instruction(query(Op::ImageQuerySamples, 12, &[50]));
        multisampled
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(43))
            .expect("image type")
            .operands[4] = Operand::LiteralBit32(1);
        assert_eq!(owned_module_cfg_error(&multisampled), None);

        let query_lod =
            |result_type, coordinate| query(Op::ImageQueryLod, result_type, &[52, coordinate]);
        for instruction in [query_lod(15, 26), query_lod(17, 23)] {
            assert_owned_invalid(
                &module_with_image_query_lod(instruction),
                "native emitter: owned ImageQueryLod violates its image-query type and dimensionality contract",
            );
        }
        assert_eq!(
            owned_module_cfg_error(&module_with_image_query_lod(query_lod(17, 26))),
            None
        );

        let mut non_fragment = module_with_sampled_image_instruction(query_lod(17, 26));
        non_fragment.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::ImageQuery)],
        ));
        assert_owned_invalid(
            &non_fragment,
            "native emitter: owned derivative instruction is reachable from a non-Fragment entry point",
        );
    }

    #[test]
    fn owned_image_query_check_matches_vulkan_validation() {
        let module = module_with_image_query_instruction(Instruction::new(
            Op::ImageQuerySizeLod,
            Some(12),
            Some(40),
            vec![Operand::IdRef(50), Operand::IdRef(13)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned ImageQuerySizeLod violates its image-query type and dimensionality contract",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_image_query_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed image-query contract"
        );
    }

    #[test]
    fn owned_module_enforces_texel_access_contracts() {
        let fetch = |result_type, coordinate, image_operands: Vec<Operand>| {
            Instruction::new(
                Op::ImageFetch,
                Some(result_type),
                Some(40),
                [
                    vec![Operand::IdRef(50), Operand::IdRef(coordinate)],
                    image_operands,
                ]
                .concat(),
            )
        };
        let read = |result_type| {
            Instruction::new(
                Op::ImageRead,
                Some(result_type),
                Some(40),
                vec![Operand::IdRef(50), Operand::IdRef(21)],
            )
        };
        let write = |texel| {
            Instruction::new(
                Op::ImageWrite,
                None,
                None,
                vec![
                    Operand::IdRef(50),
                    Operand::IdRef(21),
                    Operand::IdRef(texel),
                ],
            )
        };
        let lod = vec![
            Operand::ImageOperands(spirv::ImageOperands::LOD),
            Operand::IdRef(13),
        ];
        let sample = vec![
            Operand::ImageOperands(spirv::ImageOperands::SAMPLE),
            Operand::IdRef(13),
        ];

        let invalid = [
            module_with_texel_access_instruction(fetch(17, 21, lod.clone()), 1, false),
            module_with_texel_access_instruction(fetch(53, 21, lod.clone()), 2, false),
            module_with_texel_access_instruction(fetch(53, 23, lod.clone()), 1, false),
            module_with_texel_access_instruction(fetch(53, 21, sample.clone()), 1, false),
            module_with_texel_access_instruction(fetch(53, 21, vec![]), 1, true),
            module_with_texel_access_instruction(read(53), 1, false),
            module_with_texel_access_instruction(read(16), 2, false),
            module_with_texel_access_instruction(write(21), 2, false),
        ];
        for module in invalid {
            let opcode = module.functions[0].blocks[0].instructions[1].class.opcode;
            assert_owned_invalid(
                &module,
                &format!(
                    "native emitter: owned {opcode:?} violates its image, coordinate, texel, or image-operands contract"
                ),
            );
        }

        let valid = [
            module_with_texel_access_instruction(fetch(53, 21, lod), 1, false),
            module_with_texel_access_instruction(fetch(53, 21, sample), 1, true),
            module_with_texel_access_instruction(read(53), 2, false),
            module_with_texel_access_instruction(write(54), 2, false),
        ];
        for module in valid {
            assert_eq!(owned_module_cfg_error(&module), None);
        }
    }

    #[test]
    fn owned_texel_access_check_matches_vulkan_validation() {
        let module = module_with_texel_access_instruction(
            Instruction::new(
                Op::ImageFetch,
                Some(17),
                Some(40),
                vec![
                    Operand::IdRef(50),
                    Operand::IdRef(21),
                    Operand::ImageOperands(spirv::ImageOperands::LOD),
                    Operand::IdRef(13),
                ],
            ),
            1,
            false,
        );
        assert_owned_invalid(
            &module,
            "native emitter: owned ImageFetch violates its image, coordinate, texel, or image-operands contract",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_texel_access_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed texel-access contract"
        );
    }

    #[test]
    fn owned_module_enforces_sample_operation_contracts() {
        let sample = |opcode, result_type, coordinate, tail: Vec<Operand>| {
            Instruction::new(
                opcode,
                Some(result_type),
                Some(40),
                [vec![Operand::IdRef(52), Operand::IdRef(coordinate)], tail].concat(),
            )
        };
        let gather = |result_type, component, tail: Vec<Operand>| {
            Instruction::new(
                Op::ImageGather,
                Some(result_type),
                Some(40),
                [
                    vec![
                        Operand::IdRef(52),
                        Operand::IdRef(26),
                        Operand::IdRef(component),
                    ],
                    tail,
                ]
                .concat(),
            )
        };
        let lod = vec![
            Operand::ImageOperands(spirv::ImageOperands::LOD),
            Operand::IdRef(23),
        ];
        let offset = vec![
            Operand::ImageOperands(spirv::ImageOperands::CONST_OFFSET),
            Operand::IdRef(55),
        ];
        let lod_offset = vec![
            Operand::ImageOperands(spirv::ImageOperands::LOD | spirv::ImageOperands::CONST_OFFSET),
            Operand::IdRef(23),
            Operand::IdRef(55),
        ];

        let invalid = [
            sample(Op::ImageSampleExplicitLod, 17, 26, lod.clone()),
            sample(Op::ImageSampleExplicitLod, 53, 23, lod.clone()),
            sample(Op::ImageSampleExplicitLod, 53, 26, offset.clone()),
            sample(
                Op::ImageSampleExplicitLod,
                53,
                26,
                vec![
                    Operand::ImageOperands(spirv::ImageOperands::LOD),
                    Operand::IdRef(13),
                ],
            ),
            sample(Op::ImageSampleImplicitLod, 53, 26, lod.clone()),
            gather(17, 13, vec![]),
            gather(53, 23, vec![]),
            gather(
                53,
                13,
                vec![
                    Operand::ImageOperands(spirv::ImageOperands::CONST_OFFSET),
                    Operand::IdRef(21),
                ],
            ),
        ];
        for instruction in invalid {
            let opcode = instruction.class.opcode;
            assert_owned_invalid(
                &module_with_sample_operation_instruction(instruction),
                &format!(
                    "native emitter: owned {opcode:?} violates its sampled image, result, coordinate, component, or image-operands contract"
                ),
            );
        }

        for instruction in [
            sample(Op::ImageSampleExplicitLod, 53, 26, lod),
            sample(Op::ImageSampleExplicitLod, 53, 26, lod_offset),
            sample(Op::ImageSampleImplicitLod, 53, 26, vec![]),
            sample(Op::ImageSampleImplicitLod, 53, 26, offset.clone()),
            gather(53, 13, vec![]),
            gather(53, 13, offset),
        ] {
            assert_eq!(
                owned_module_cfg_error(&module_with_sample_operation_instruction(instruction)),
                None
            );
        }

        for operand in [4, 5] {
            let mut module = module_with_sample_operation_instruction(sample(
                Op::ImageSampleExplicitLod,
                53,
                26,
                vec![
                    Operand::ImageOperands(spirv::ImageOperands::LOD),
                    Operand::IdRef(23),
                ],
            ));
            module
                .types_global_values
                .iter_mut()
                .find(|instruction| instruction.result_id == Some(43))
                .expect("image type")
                .operands[operand] = if operand == 4 {
                Operand::LiteralBit32(1)
            } else {
                Operand::LiteralBit32(2)
            };
            assert_owned_invalid(
                &module,
                "native emitter: owned ImageSampleExplicitLod violates its sampled image, result, coordinate, component, or image-operands contract",
            );
        }

        let mut non_fragment = module_with_sample_operation_instruction(sample(
            Op::ImageSampleImplicitLod,
            53,
            26,
            vec![],
        ));
        non_fragment.entry_points[0].operands[0] =
            Operand::ExecutionModel(spirv::ExecutionModel::GLCompute);
        non_fragment.execution_modes = vec![inst(
            Op::ExecutionMode,
            vec![
                Operand::IdRef(32),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
            ],
        )];
        assert_owned_invalid(
            &non_fragment,
            "native emitter: owned derivative instruction is reachable from a non-Fragment entry point",
        );
    }

    #[test]
    fn owned_module_enforces_image_texel_pointer_contracts() {
        let pointer = |result_type, image, coordinate, sample| {
            Instruction::new(
                Op::ImageTexelPointer,
                Some(result_type),
                Some(40),
                vec![
                    Operand::IdRef(image),
                    Operand::IdRef(coordinate),
                    Operand::IdRef(sample),
                ],
            )
        };
        let expected = "native emitter: owned OpImageTexelPointer violates its result, image, coordinate, sample, or atomic-format contract";

        for instruction in [
            pointer(14, 48, 21, 13),
            pointer(57, 48, 21, 13),
            pointer(56, 21, 21, 13),
            pointer(56, 48, 13, 13),
            pointer(56, 48, 26, 13),
            pointer(56, 48, 21, 21),
            pointer(56, 48, 21, 59),
        ] {
            assert_owned_invalid(
                &module_with_image_texel_pointer_instruction(instruction),
                expected,
            );
        }

        for (operand, replacement) in [
            (0, Operand::IdRef(15)),
            (1, Operand::Dim(spirv::Dim::DimSubpassData)),
            (5, Operand::LiteralBit32(1)),
            (6, Operand::ImageFormat(spirv::ImageFormat::R32i)),
            (6, Operand::ImageFormat(spirv::ImageFormat::Rgba32ui)),
        ] {
            let mut module = module_with_image_texel_pointer_instruction(pointer(56, 48, 21, 13));
            module
                .types_global_values
                .iter_mut()
                .find(|instruction| instruction.result_id == Some(43))
                .expect("image type")
                .operands[operand] = replacement;
            assert_owned_invalid(&module, expected);
        }

        let valid = module_with_image_texel_pointer_instruction(pointer(56, 48, 21, 13));
        assert_eq!(owned_module_cfg_error(&valid), None);

        let mut multisampled = module_with_image_texel_pointer_instruction(pointer(56, 48, 21, 59));
        multisampled
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(43))
            .expect("image type")
            .operands[4] = Operand::LiteralBit32(1);
        assert_eq!(owned_module_cfg_error(&multisampled), None);
    }

    #[test]
    fn owned_image_texel_pointer_check_matches_vulkan_validation() {
        let valid = module_with_image_texel_pointer_instruction(Instruction::new(
            Op::ImageTexelPointer,
            Some(56),
            Some(40),
            vec![Operand::IdRef(48), Operand::IdRef(21), Operand::IdRef(13)],
        ));
        let module = module_with_image_texel_pointer_instruction(Instruction::new(
            Op::ImageTexelPointer,
            Some(14),
            Some(40),
            vec![Operand::IdRef(48), Operand::IdRef(21), Operand::IdRef(13)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned OpImageTexelPointer violates its result, image, coordinate, sample, or atomic-format contract",
        );
        let valid_bytes = valid
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_image_texel_pointer_type_{}",
            std::process::id()
        ));
        crate::tools::spirv_val_bytes(&valid_bytes, &tmp)
            .expect("spirv-val must accept the owned image-texel-pointer contract");
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed image-texel-pointer contract"
        );
    }

    #[test]
    fn owned_module_enforces_barrier_contracts() {
        let control = |execution, memory, semantics| {
            inst(
                Op::ControlBarrier,
                vec![
                    Operand::IdScope(execution),
                    Operand::IdScope(memory),
                    Operand::IdMemorySemantics(semantics),
                ],
            )
        };
        let memory = |scope, semantics| {
            inst(
                Op::MemoryBarrier,
                vec![
                    Operand::IdScope(scope),
                    Operand::IdMemorySemantics(semantics),
                ],
            )
        };

        for instruction in [
            control(43, 41, 44),
            control(49, 41, 44),
            control(41, 48, 44),
            control(41, 41, 46),
            control(41, 41, 47),
            memory(49, 45),
            memory(43, 49),
            memory(43, 46),
            memory(43, 47),
        ] {
            let opcode = instruction.class.opcode;
            assert_owned_invalid(
                &module_with_barrier_instruction(instruction),
                &format!(
                    "native emitter: owned {opcode:?} violates its constant scope and memory-semantics contract"
                ),
            );
        }

        for instruction in [
            control(41, 41, 44),
            control(42, 43, 45),
            memory(43, 45),
            memory(41, 44),
        ] {
            assert_eq!(
                owned_module_cfg_error(&module_with_barrier_instruction(instruction)),
                None
            );
        }

        let mut vertex = module_with_barrier_instruction(control(41, 41, 44));
        vertex.entry_points[0].operands[0] = Operand::ExecutionModel(spirv::ExecutionModel::Vertex);
        vertex.execution_modes.clear();
        assert_owned_invalid(
            &vertex,
            "native emitter: owned OpControlBarrier is reachable from an unsupported execution model",
        );
    }

    #[test]
    fn owned_barrier_check_matches_vulkan_validation() {
        let valid = module_with_barrier_instruction(inst(
            Op::ControlBarrier,
            vec![
                Operand::IdScope(41),
                Operand::IdScope(41),
                Operand::IdMemorySemantics(44),
            ],
        ));
        let invalid = module_with_barrier_instruction(inst(
            Op::ControlBarrier,
            vec![
                Operand::IdScope(49),
                Operand::IdScope(41),
                Operand::IdMemorySemantics(44),
            ],
        ));
        assert_owned_invalid(
            &invalid,
            "native emitter: owned ControlBarrier violates its constant scope and memory-semantics contract",
        );
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_barrier_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned barrier contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed barrier contract"
        );
    }

    #[test]
    fn an_id_renders_as_its_definition_and_a_wide_one_is_elided() {
        let int = Instruction::new(
            Op::TypeInt,
            None,
            Some(7),
            vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
        );
        let pointer = Instruction::new(
            Op::TypePointer,
            None,
            Some(8),
            vec![
                Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                Operand::IdRef(7),
            ],
        );
        let wide = Instruction::new(
            Op::TypeStruct,
            None,
            Some(9),
            (0..12).map(|_| Operand::IdRef(7)).collect(),
        );
        let definitions = [&int, &pointer, &wide]
            .into_iter()
            .map(|instruction| (instruction.result_id.expect("result id"), instruction))
            .collect::<HashMap<_, _>>();

        assert_eq!(describe_owned_id(7, &definitions), "%7 (TypeInt 64 0)");
        assert_eq!(
            describe_owned_id(8, &definitions),
            "%8 (TypePointer StorageBuffer %7)"
        );
        assert_eq!(
            describe_owned_id(9, &definitions),
            "%9 (TypeStruct %7 %7 %7 %7 %7 %7 %7 %7 +4 more)",
            "a wide aggregate must not bury the sentence the message is trying to say"
        );
        assert_eq!(describe_owned_id(99, &definitions), "%99 (undefined)");
    }

    #[test]
    fn a_dynamic_index_into_a_single_member_block_reports_the_view_it_failed_to_repair() {
        let word = Instruction::new(
            Op::TypeInt,
            None,
            Some(1),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        );
        let byte = Instruction::new(
            Op::TypeInt,
            None,
            Some(2),
            vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
        );
        let words = Instruction::new(Op::TypeRuntimeArray, None, Some(3), vec![Operand::IdRef(1)]);
        let block = Instruction::new(Op::TypeStruct, None, Some(4), vec![Operand::IdRef(3)]);
        let byte_pointer = Instruction::new(
            Op::TypePointer,
            None,
            Some(5),
            vec![
                Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                Operand::IdRef(2),
            ],
        );
        let pair = Instruction::new(
            Op::TypeStruct,
            None,
            Some(6),
            vec![Operand::IdRef(1), Operand::IdRef(1)],
        );
        let wrapper = Instruction::new(Op::TypeStruct, None, Some(7), vec![Operand::IdRef(1)]);
        let definitions = [&word, &byte, &words, &block, &byte_pointer, &pair, &wrapper]
            .into_iter()
            .map(|instruction| (instruction.result_id.expect("result id"), instruction))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            describe_element_view(4, Some(5), &definitions),
            ". The struct is a single-member block over %1 (TypeInt 32 0), so this is not a member \
             select but a %2 (TypeInt 8 0) view of that array which no dynamic-struct-index rewrite \
             repaired",
            "a byte view of a word array is the fact that picks out the missing rewrite"
        );
        assert_eq!(
            describe_element_view(6, Some(5), &definitions),
            "",
            "a multi-member struct is not a block, so the index really is a bad member select"
        );
        assert_eq!(
            describe_element_view(7, Some(5), &definitions),
            "",
            "a single member that is not an array has no elements to view"
        );
        assert_eq!(
            describe_element_view(1, Some(5), &definitions),
            "",
            "only a struct can have a member index"
        );
    }

    #[test]
    fn owned_module_enforces_access_chain_contracts() {
        let chain = |opcode, result_type, base, indices: &[Word]| {
            let mut operands = vec![Operand::IdRef(base)];
            operands.extend(indices.iter().copied().map(Operand::IdRef));
            Instruction::new(opcode, Some(result_type), Some(40), operands)
        };
        let ptr_chain = |opcode, result_type, base, element, indices: &[Word]| {
            let mut operands = vec![Operand::IdRef(base), Operand::IdRef(element)];
            operands.extend(indices.iter().copied().map(Operand::IdRef));
            Instruction::new(opcode, Some(result_type), Some(40), operands)
        };

        for instruction in [
            chain(Op::AccessChain, 36, 37, &[]),
            chain(Op::AccessChain, 14, 37, &[13]),
            chain(Op::InBoundsAccessChain, 14, 39, &[13]),
            chain(Op::AccessChain, 41, 39, &[55]),
            chain(Op::AccessChain, 14, 39, &[45]),
            chain(Op::AccessChain, 14, 43, &[46]),
            chain(Op::AccessChain, 54, 53, &[46]),
            chain(Op::AccessChain, 41, 53, &[46, 46]),
            ptr_chain(Op::PtrAccessChain, 36, 37, 46, &[]),
            ptr_chain(Op::PtrAccessChain, 14, 37, 46, &[46]),
        ] {
            assert_eq!(
                owned_module_cfg_error(&module_with_access_chain_instruction(instruction)),
                None
            );
        }

        for (instruction, reason) in [
            (
                chain(Op::AccessChain, 14, 13, &[13]),
                "base %13 has type %12 (TypeInt 32 0), which is not a pointer type",
            ),
            (
                chain(Op::AccessChain, 12, 37, &[13]),
                "result type %12 (TypeInt 32 0) is not a pointer type",
            ),
            (
                chain(Op::AccessChain, 50, 37, &[13]),
                "base %37 is in storage class Function but the result pointer is in Private",
            ),
            (
                chain(Op::AccessChain, 41, 37, &[13]),
                "the index path selects %12 (TypeInt 32 0), but the result pointer points to \
                 %15 (TypeFloat 32)",
            ),
            (
                chain(Op::AccessChain, 14, 37, &[23]),
                "index 0 has type %15 (TypeFloat 32), which is not a scalar integer",
            ),
            (
                chain(Op::AccessChain, 14, 37, &[21]),
                "index 0 has type %16 (TypeVector %12 2), which is not a scalar integer",
            ),
            (
                chain(Op::AccessChain, 14, 39, &[46]),
                "index 0 into %18 (TypeStruct %12 %15) is %46 (Undef), and a struct member index \
                 must be an OpConstant of 32-bit integer type",
            ),
            (
                chain(Op::AccessChain, 14, 39, &[19]),
                "index 0 selects member 2 of %18 (TypeStruct %12 %15), which has 2 members",
            ),
            (
                chain(Op::AccessChain, 14, 37, &[13, 13]),
                "index 1 descends into %12 (TypeInt 32 0), which is not a composite type",
            ),
            (
                ptr_chain(Op::PtrAccessChain, 36, 37, 23, &[]),
                "the element operand has type %15 (TypeFloat 32), which is not a scalar integer",
            ),
            (
                ptr_chain(Op::PtrAccessChain, 41, 37, 46, &[46]),
                "the index path selects %12 (TypeInt 32 0), but the result pointer points to \
                 %15 (TypeFloat 32)",
            ),
            (
                ptr_chain(Op::InBoundsPtrAccessChain, 41, 37, 46, &[46]),
                "the index path selects %12 (TypeInt 32 0), but the result pointer points to \
                 %15 (TypeFloat 32)",
            ),
        ] {
            let opcode = instruction.class.opcode;
            assert_owned_type_construction(
                &module_with_access_chain_instruction(instruction),
                &format!(
                    "native emitter: owned {opcode:?} violates its access-chain contract: {reason}"
                ),
            );
        }
    }

    #[test]
    fn owned_access_chain_check_matches_vulkan_validation() {
        let chain = |result_type| {
            Instruction::new(
                Op::AccessChain,
                Some(result_type),
                Some(40),
                vec![Operand::IdRef(37), Operand::IdRef(13)],
            )
        };
        let valid = module_with_access_chain_instruction(chain(14));
        let invalid = module_with_access_chain_instruction(chain(41));
        assert_owned_type_construction(
            &invalid,
            "native emitter: owned AccessChain violates its access-chain contract: the index path \
             selects %12 (TypeInt 32 0), but the result pointer points to %15 (TypeFloat 32)",
        );
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_access_chain_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned access-chain contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let dynamic_struct = module_with_access_chain_instruction(Instruction::new(
            Op::AccessChain,
            Some(14),
            Some(40),
            vec![Operand::IdRef(39), Operand::IdRef(46)],
        ));
        let dynamic_struct_validation =
            crate::tools::spirv_val_bytes(&bytes(&dynamic_struct), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed access-chain contract"
        );
        assert!(
            dynamic_struct_validation.is_err(),
            "spirv-val must reject a dynamic structure-member index"
        );
    }

    #[test]
    fn owned_module_enforces_memory_type_contracts() {
        for (instruction, expected) in [
            (
                Instruction::new(Op::Load, Some(15), Some(40), vec![Operand::IdRef(33)]),
                "native emitter: owned Load violates its pointer-pointee and value-type contract: \
                 pointer %33, of type %14 (TypePointer Function %12), points at %12 \
                 (TypeInt 32 0), but the result type is %15 (TypeFloat 32)",
            ),
            (
                Instruction::new(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(33), Operand::IdRef(36)],
                ),
                "native emitter: owned Store violates its pointer-pointee and value-type contract: \
                 pointer %33, of type %14 (TypePointer Function %12), points at %12 \
                 (TypeInt 32 0), but the stored value is %36, of type %15 (TypeFloat 32)",
            ),
            (
                Instruction::new(
                    Op::CopyMemory,
                    None,
                    None,
                    vec![Operand::IdRef(33), Operand::IdRef(35)],
                ),
                "native emitter: owned CopyMemory violates its pointer-pointee and value-type \
                 contract: the target %33, of type %14 (TypePointer Function %12), points at \
                 %12 (TypeInt 32 0), and the source %35, of type %16 (TypePointer Function %15), \
                 points at %15 (TypeFloat 32)",
            ),
        ] {
            assert_owned_type_construction(&module_with_memory_instruction(instruction), expected);
        }
        assert_owned_type_construction(
            &module_with_memory_instruction(Instruction::new(
                Op::Load,
                Some(12),
                Some(40),
                vec![
                    Operand::IdRef(33),
                    Operand::MemoryAccess(spirv::MemoryAccess::ALIGNED),
                    Operand::LiteralBit32(3),
                ],
            )),
            "native emitter: owned Load violates its aligned memory-access contract",
        );
        let mut physical = module_with_memory_instruction(Instruction::new(
            Op::Load,
            Some(12),
            Some(40),
            vec![Operand::IdRef(37)],
        ));
        physical.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(17),
                vec![
                    Operand::StorageClass(spirv::StorageClass::PhysicalStorageBuffer),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(Op::Undef, Some(17), Some(37), vec![]),
        ]);
        assert_owned_type_construction(
            &physical,
            "native emitter: owned Load violates its aligned memory-access contract",
        );
    }

    #[test]
    fn owned_memory_check_matches_vulkan_validation() {
        let copy = |source| {
            Instruction::new(
                Op::CopyMemory,
                None,
                None,
                vec![Operand::IdRef(33), Operand::IdRef(source)],
            )
        };
        let valid = module_with_memory_instruction(copy(34));
        let invalid = module_with_memory_instruction(copy(35));
        let invalid_alignment = module_with_memory_instruction(Instruction::new(
            Op::Load,
            Some(12),
            Some(40),
            vec![
                Operand::IdRef(33),
                Operand::MemoryAccess(spirv::MemoryAccess::ALIGNED),
                Operand::LiteralBit32(3),
            ],
        ));
        assert_owned_type_construction(
            &invalid,
            "native emitter: owned CopyMemory violates its pointer-pointee and value-type \
             contract: the target %33, of type %14 (TypePointer Function %12), points at \
             %12 (TypeInt 32 0), and the source %35, of type %16 (TypePointer Function %15), \
             points at %15 (TypeFloat 32)",
        );
        assert_owned_type_construction(
            &invalid_alignment,
            "native emitter: owned Load violates its aligned memory-access contract",
        );
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_memory_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned memory contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let alignment_validation = crate::tools::spirv_val_bytes(&bytes(&invalid_alignment), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed memory contract"
        );
        assert!(
            alignment_validation.is_err(),
            "spirv-val must reject a non-power-of-two memory alignment"
        );
    }

    #[test]
    fn owned_module_enforces_variable_construction_contracts() {
        let local_variable =
            |result_type: Word, storage: spirv::StorageClass, initializer: Option<Word>| {
                let mut operands = vec![Operand::StorageClass(storage)];
                operands.extend(initializer.map(Operand::IdRef));
                module_with_blocks(vec![block(
                    50,
                    vec![
                        Instruction::new(Op::Variable, Some(result_type), Some(40), operands),
                        inst(Op::Return, vec![]),
                    ],
                )])
            };
        let expected = "native emitter: owned OpVariable violates its scope, pointer storage, or initializer-type contract";
        assert_owned_type_construction(
            &local_variable(12, spirv::StorageClass::Function, None),
            expected,
        );
        assert_owned_type_construction(
            &local_variable(14, spirv::StorageClass::Private, None),
            expected,
        );

        let invalid_initializer = module_with_memory_instruction(Instruction::new(
            Op::Variable,
            Some(14),
            Some(40),
            vec![
                Operand::StorageClass(spirv::StorageClass::Function),
                Operand::IdRef(36),
            ],
        ));
        assert_owned_type_construction(&invalid_initializer, expected);

        let mut module_scope_function =
            module_with_blocks(vec![block(50, vec![inst(Op::Return, vec![])])]);
        module_scope_function
            .types_global_values
            .push(Instruction::new(
                Op::Variable,
                Some(14),
                Some(40),
                vec![Operand::StorageClass(spirv::StorageClass::Function)],
            ));
        assert_owned_type_construction(&module_scope_function, expected);
    }

    #[test]
    fn owned_variable_check_matches_vulkan_validation() {
        let local_variable = |storage| {
            module_with_blocks(vec![block(
                50,
                vec![
                    Instruction::new(
                        Op::Variable,
                        Some(14),
                        Some(40),
                        vec![Operand::StorageClass(storage)],
                    ),
                    inst(Op::Return, vec![]),
                ],
            )])
        };
        let valid = local_variable(spirv::StorageClass::Function);
        let invalid = local_variable(spirv::StorageClass::Private);
        assert_owned_type_construction(
            &invalid,
            "native emitter: owned OpVariable violates its scope, pointer storage, or initializer-type contract",
        );
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_variable_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned variable contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject a local variable with non-Function storage"
        );
    }

    #[test]
    fn owned_module_enforces_pointer_construction_contracts() {
        let uint_to_ptr = module_with_composite_instruction(Instruction::new(
            Op::ConvertUToPtr,
            Some(14),
            Some(40),
            vec![Operand::IdRef(23)],
        ));
        assert_owned_type_construction(
            &uint_to_ptr,
            "native emitter: owned OpConvertUToPtr requires a pointer result and integer-scalar input",
        );

        let ptr_to_uint = module_with_memory_instruction(Instruction::new(
            Op::ConvertPtrToU,
            Some(15),
            Some(40),
            vec![Operand::IdRef(33)],
        ));
        assert_owned_type_construction(
            &ptr_to_uint,
            "native emitter: owned OpConvertPtrToU requires an integer-scalar result and pointer input",
        );

        let atomic = module_with_memory_instruction(Instruction::new(
            Op::AtomicLoad,
            Some(12),
            Some(40),
            vec![
                Operand::IdRef(33),
                Operand::IdScope(13),
                Operand::IdMemorySemantics(13),
            ],
        ));
        assert_owned_type_construction(
            &atomic,
            "native emitter: owned AtomicLoad pointer %33 (Variable Function) has the \
             non-atomic storage class Function",
        );

        let pointer_select = |left, right| {
            module_with_memory_instruction(Instruction::new(
                Op::Select,
                Some(14),
                Some(40),
                vec![
                    Operand::IdRef(10),
                    Operand::IdRef(left),
                    Operand::IdRef(right),
                ],
            ))
        };
        assert_owned_type_construction(
            &pointer_select(33, 35),
            "native emitter: cannot reinterpret pointer for owned OpSelect type mismatch",
        );
        assert_owned_type_construction(
            &pointer_select(33, 34),
            "native emitter: cannot retain cross-root pointer OpSelect under Logical addressing",
        );

        let mut cross_root_phi = module_with_blocks(vec![block(
            50,
            vec![
                Instruction::new(
                    Op::Phi,
                    Some(60),
                    Some(63),
                    vec![
                        Operand::IdRef(61),
                        Operand::IdRef(50),
                        Operand::IdRef(62),
                        Operand::IdRef(50),
                    ],
                ),
                inst(Op::Return, vec![]),
            ],
        )]);
        cross_root_phi.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(60),
                vec![
                    Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(60),
                Some(61),
                vec![Operand::StorageClass(spirv::StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Variable,
                Some(60),
                Some(62),
                vec![Operand::StorageClass(spirv::StorageClass::StorageBuffer)],
            ),
        ]);
        match owned_module_failure(&cross_root_phi) {
            Some(OwnedModuleFailure::RawBufferConstruction(error)) => assert_eq!(
                error,
                "native emitter: cross-root StorageBuffer pointer phi requires address-domain construction"
            ),
            _ => panic!("cross-root pointer phi was not classified for raw construction"),
        }

        let mut workgroup_select = pointer_select(33, 34);
        workgroup_select.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(spirv::Capability::VariablePointers)],
        ));
        workgroup_select.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(60),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Workgroup),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(60),
                Some(61),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::Variable,
                Some(60),
                Some(62),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
        ]);
        workgroup_select.entry_points[0]
            .operands
            .extend([Operand::IdRef(61), Operand::IdRef(62)]);
        let select = workgroup_select.functions[0].blocks[0]
            .instructions
            .iter_mut()
            .find(|instruction| instruction.class.opcode == Op::Select)
            .expect("pointer select");
        select.result_type = Some(60);
        select.operands[1] = Operand::IdRef(61);
        select.operands[2] = Operand::IdRef(62);
        assert_eq!(owned_module_cfg_error(&workgroup_select), None);

        let null = module_with_composite_instruction(Instruction::new(
            Op::ConstantNull,
            Some(14),
            Some(40),
            vec![],
        ));
        assert_owned_type_construction(
            &null,
            "native emitter: cannot retain OpConstantNull pointer under Logical addressing",
        );
        let mut variable_pointer_null = null.clone();
        variable_pointer_null
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(14))
            .expect("pointer type")
            .operands[0] = Operand::StorageClass(spirv::StorageClass::StorageBuffer);
        variable_pointer_null.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(spirv::Capability::VariablePointers)],
        ));
        assert_eq!(owned_module_cfg_error(&variable_pointer_null), None);

        let mut pointer_variable = module_with_blocks(vec![block(
            50,
            vec![
                Instruction::new(
                    Op::Variable,
                    Some(16),
                    Some(40),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                inst(Op::Return, vec![]),
            ],
        )]);
        pointer_variable.types_global_values.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(16),
            vec![
                Operand::StorageClass(spirv::StorageClass::Function),
                Operand::IdRef(14),
            ],
        ));
        assert_owned_type_construction(
            &pointer_variable,
            "native emitter: missing pointer storage for pointer-valued Logical variable",
        );
    }

    #[test]
    fn owned_pointer_check_matches_vulkan_validation() {
        let valid = module_with_blocks(vec![block(50, vec![inst(Op::Return, vec![])])]);
        let invalid = module_with_composite_instruction(Instruction::new(
            Op::ConstantNull,
            Some(14),
            Some(40),
            vec![],
        ));
        assert_owned_type_construction(
            &invalid,
            "native emitter: cannot retain OpConstantNull pointer under Logical addressing",
        );
        let mut variable_pointer_null = invalid.clone();
        variable_pointer_null
            .types_global_values
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(14))
            .expect("pointer type")
            .operands[0] = Operand::StorageClass(spirv::StorageClass::StorageBuffer);
        variable_pointer_null.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(spirv::Capability::VariablePointers)],
        ));
        assert_eq!(owned_module_cfg_error(&variable_pointer_null), None);
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_pointer_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned pointer-contract baseline");
        crate::tools::spirv_val_bytes(&bytes(&variable_pointer_null), &tmp)
            .expect("spirv-val must accept a null admitted by VariablePointers");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject a Logical-addressing pointer null"
        );
    }

    #[test]
    fn owned_module_enforces_group_non_uniform_contracts() {
        let vote = |opcode, result_type, value| {
            Instruction::new(
                opcode,
                Some(result_type),
                Some(40),
                vec![Operand::IdScope(45), Operand::IdRef(value)],
            )
        };
        let shuffle = |opcode, result_type, value, index| {
            Instruction::new(
                opcode,
                Some(result_type),
                Some(40),
                vec![
                    Operand::IdScope(45),
                    Operand::IdRef(value),
                    Operand::IdRef(index),
                ],
            )
        };
        let arithmetic = |opcode, result_type, operation, value, cluster| {
            let mut operands = vec![
                Operand::IdScope(45),
                Operand::GroupOperation(operation),
                Operand::IdRef(value),
            ];
            if let Some(cluster) = cluster {
                operands.push(Operand::IdRef(cluster));
            }
            Instruction::new(opcode, Some(result_type), Some(40), operands)
        };
        let error = |opcode| {
            format!(
                "native emitter: owned {opcode:?} violates its subgroup scope, operand, result, or group-operation contract"
            )
        };

        let valid = [
            Instruction::new(
                Op::GroupNonUniformElect,
                Some(9),
                Some(40),
                vec![Operand::IdScope(45)],
            ),
            vote(Op::GroupNonUniformAll, 9, 10),
            vote(Op::GroupNonUniformAny, 9, 11),
            vote(Op::GroupNonUniformAllEqual, 9, 21),
            vote(Op::GroupNonUniformBallot, 43, 10),
            vote(Op::GroupNonUniformBroadcastFirst, 17, 26),
            shuffle(Op::GroupNonUniformShuffle, 16, 21, 49),
            shuffle(Op::GroupNonUniformShuffleXor, 41, 42, 13),
            shuffle(Op::GroupNonUniformShuffleDown, 15, 23, 13),
            arithmetic(
                Op::GroupNonUniformIAdd,
                12,
                spirv::GroupOperation::Reduce,
                13,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformFAdd,
                15,
                spirv::GroupOperation::InclusiveScan,
                23,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformFMin,
                17,
                spirv::GroupOperation::ExclusiveScan,
                26,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformSMax,
                41,
                spirv::GroupOperation::Reduce,
                42,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformUMin,
                16,
                spirv::GroupOperation::Reduce,
                21,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformBitwiseOr,
                16,
                spirv::GroupOperation::ClusteredReduce,
                21,
                Some(47),
            ),
        ];
        for instruction in valid {
            assert_eq!(
                owned_module_cfg_error(&module_with_group_non_uniform_instruction(instruction)),
                None
            );
        }

        let mut wrong_scope = vote(Op::GroupNonUniformAll, 9, 10);
        wrong_scope.operands[0] = Operand::IdScope(46);
        let malformed = [
            wrong_scope,
            Instruction::new(
                Op::GroupNonUniformElect,
                Some(12),
                Some(40),
                vec![Operand::IdScope(45)],
            ),
            vote(Op::GroupNonUniformAny, 9, 13),
            vote(Op::GroupNonUniformAllEqual, 12, 21),
            vote(Op::GroupNonUniformBallot, 16, 10),
            vote(Op::GroupNonUniformBroadcastFirst, 15, 26),
            shuffle(Op::GroupNonUniformShuffle, 16, 21, 23),
            arithmetic(
                Op::GroupNonUniformFMax,
                12,
                spirv::GroupOperation::Reduce,
                13,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformSMin,
                12,
                spirv::GroupOperation::Reduce,
                13,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformUMax,
                41,
                spirv::GroupOperation::Reduce,
                42,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformBitwiseXor,
                27,
                spirv::GroupOperation::Reduce,
                28,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformIAdd,
                12,
                spirv::GroupOperation::Reduce,
                13,
                Some(47),
            ),
            arithmetic(
                Op::GroupNonUniformIAdd,
                12,
                spirv::GroupOperation::ClusteredReduce,
                13,
                None,
            ),
            arithmetic(
                Op::GroupNonUniformIAdd,
                12,
                spirv::GroupOperation::ClusteredReduce,
                13,
                Some(48),
            ),
        ];
        for instruction in malformed {
            let opcode = instruction.class.opcode;
            assert_owned_invalid(
                &module_with_group_non_uniform_instruction(instruction),
                &error(opcode),
            );
        }
    }

    #[test]
    fn owned_group_non_uniform_check_matches_vulkan_validation() {
        let shuffle = |index| {
            Instruction::new(
                Op::GroupNonUniformShuffle,
                Some(16),
                Some(40),
                vec![
                    Operand::IdScope(45),
                    Operand::IdRef(21),
                    Operand::IdRef(index),
                ],
            )
        };
        let valid = module_with_group_non_uniform_instruction(shuffle(49));
        let invalid = module_with_group_non_uniform_instruction(shuffle(23));
        assert_owned_invalid(
            &invalid,
            "native emitter: owned GroupNonUniformShuffle violates its subgroup scope, operand, result, or group-operation contract",
        );
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_group_non_uniform_contract_{}",
            std::process::id()
        ));
        let bytes = |module: &Module| {
            module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        };
        crate::tools::spirv_val_bytes(&bytes(&valid), &tmp)
            .expect("spirv-val must accept the owned subgroup contract");
        let validation = crate::tools::spirv_val_bytes(&bytes(&invalid), &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed subgroup contract"
        );
    }

    #[test]
    fn owned_sample_operation_check_matches_vulkan_validation() {
        let module = module_with_sample_operation_instruction(Instruction::new(
            Op::ImageSampleExplicitLod,
            Some(17),
            Some(40),
            vec![
                Operand::IdRef(52),
                Operand::IdRef(26),
                Operand::ImageOperands(spirv::ImageOperands::LOD),
                Operand::IdRef(23),
            ],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned ImageSampleExplicitLod violates its sampled image, result, coordinate, component, or image-operands contract",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_sample_operation_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed sample-operation contract"
        );
    }

    #[test]
    fn owned_module_rejects_derivatives_from_non_fragment_call_trees() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::DPdy,
            Some(15),
            Some(40),
            vec![Operand::IdRef(23)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned derivative instruction is reachable from a non-Fragment entry point",
        );
    }

    #[test]
    fn owned_bitcast_check_matches_vulkan_validation() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::Bitcast,
            Some(17),
            Some(40),
            vec![Operand::IdRef(13)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned OpBitcast source and result shapes are inconsistent",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_bitcast_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed numeric bitcast contract"
        );
    }

    #[test]
    fn owned_bitfield_width_check_matches_vulkan_validation() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::BitFieldUExtract,
            Some(29),
            Some(40),
            vec![Operand::IdRef(33), Operand::IdRef(13), Operand::IdRef(13)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned BitFieldUExtract operands are not a Vulkan 1.2 bit-field shape",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_bitfield_width_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the non-32-bit Vulkan 1.2 bit-field base"
        );
    }

    #[test]
    fn owned_derivative_execution_model_check_is_not_backed_by_vulkan_validation() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::Fwidth,
            Some(15),
            Some(40),
            vec![Operand::IdRef(23)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned derivative instruction is reachable from a non-Fragment entry point",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_derivative_model_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_ok(),
            "spirv-val now rejects a derivative reachable from GLCompute, so the owned check has a \
             second line of defence behind it; say so here rather than leaving this note stale: \
             {validation:?}"
        );
    }

    #[test]
    fn owned_conversion_check_matches_vulkan_validation() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::UConvert,
            Some(15),
            Some(40),
            vec![Operand::IdRef(13)],
        ));
        assert_owned_invalid(
            &module,
            "native emitter: owned UConvert source and result shapes are inconsistent",
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_conversion_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed conversion type contract"
        );
    }

    #[test]
    fn owned_width_conversions_require_a_width_change() {
        let signed_short = Instruction::new(
            Op::TypeInt,
            None,
            Some(39),
            vec![Operand::LiteralBit32(16), Operand::LiteralBit32(1)],
        );
        let rejected: [(Instruction, Option<Instruction>, &str); 6] = [
            (
                Instruction::new(Op::UConvert, Some(12), Some(40), vec![Operand::IdRef(13)]),
                None,
                "native emitter: owned UConvert source and result shapes are inconsistent",
            ),
            (
                Instruction::new(Op::SConvert, Some(12), Some(40), vec![Operand::IdRef(13)]),
                None,
                "native emitter: owned SConvert source and result shapes are inconsistent",
            ),
            (
                Instruction::new(Op::FConvert, Some(15), Some(40), vec![Operand::IdRef(23)]),
                None,
                "native emitter: owned FConvert source and result shapes are inconsistent",
            ),
            (
                Instruction::new(Op::UConvert, Some(16), Some(40), vec![Operand::IdRef(21)]),
                None,
                "native emitter: owned UConvert source and result shapes are inconsistent",
            ),
            (
                Instruction::new(Op::UConvert, Some(39), Some(40), vec![Operand::IdRef(13)]),
                Some(signed_short.clone()),
                "native emitter: owned OpUConvert result type is not an unsigned integer",
            ),
            (
                Instruction::new(
                    Op::SatConvertSToU,
                    Some(29),
                    Some(40),
                    vec![Operand::IdRef(13)],
                ),
                None,
                "native emitter: owned SatConvertSToU is not available in Vulkan 1.2",
            ),
        ];
        for (instruction, extra_type, expected) in rejected {
            let opcode = instruction.class.opcode;
            let mut module = module_with_composite_instruction(instruction);
            if let Some(extra_type) = extra_type {
                module.types_global_values.push(extra_type);
            }
            assert_owned_invalid(&module, expected);
            let bytes = module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>();
            let tmp = std::env::temp_dir().join(format!(
                "metal2vulkan_owned_width_conversion_{opcode:?}_{}",
                std::process::id()
            ));
            let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
            let _ = std::fs::remove_dir(&tmp);
            assert!(
                validation.is_err(),
                "spirv-val must reject {opcode:?} without a width change"
            );
        }
    }

    #[test]
    fn owned_width_conversions_accept_a_real_width_change() {
        let accepted = [
            Instruction::new(Op::UConvert, Some(29), Some(40), vec![Operand::IdRef(13)]),
            Instruction::new(Op::UConvert, Some(12), Some(40), vec![Operand::IdRef(33)]),
            Instruction::new(Op::FConvert, Some(34), Some(40), vec![Operand::IdRef(23)]),
            Instruction::new(Op::FConvert, Some(15), Some(40), vec![Operand::IdRef(35)]),
            Instruction::new(
                Op::ConvertFToU,
                Some(12),
                Some(40),
                vec![Operand::IdRef(23)],
            ),
            Instruction::new(
                Op::ConvertUToF,
                Some(15),
                Some(40),
                vec![Operand::IdRef(13)],
            ),
        ];
        for instruction in accepted {
            let opcode = instruction.class.opcode;
            let result_type = instruction.result_type;
            let module = module_with_composite_instruction(instruction);
            assert!(
                owned_module_failure(&module).is_none(),
                "owned construction rejected a valid {opcode:?} to {result_type:?}"
            );
            let bytes = module
                .assemble()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>();
            let tmp = std::env::temp_dir().join(format!(
                "metal2vulkan_owned_valid_conversion_{opcode:?}_{}",
                std::process::id()
            ));
            let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
            let _ = std::fs::remove_dir(&tmp);
            assert!(
                validation.is_ok(),
                "spirv-val rejected a conversion the owned contract accepts: {validation:?}"
            );
        }
    }

    #[test]
    fn owned_module_enforces_composite_instruction_types_and_indices() {
        let cases = [
            (
                Instruction::new(
                    Op::CompositeConstruct,
                    Some(16),
                    Some(40),
                    vec![Operand::IdRef(13), Operand::IdRef(23)],
                ),
                "native emitter: owned CompositeConstruct constituents do not match its result type",
            ),
            (
                Instruction::new(
                    Op::ConstantComposite,
                    Some(16),
                    Some(40),
                    vec![Operand::IdRef(13)],
                ),
                "native emitter: owned ConstantComposite constituents do not match its result type",
            ),
            (
                Instruction::new(
                    Op::CompositeExtract,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::LiteralBit32(0)],
                ),
                "native emitter: owned OpCompositeExtract result does not match its index path",
            ),
            (
                Instruction::new(
                    Op::CompositeExtract,
                    Some(12),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::LiteralBit32(2)],
                ),
                "native emitter: owned composite operation index is out of bounds",
            ),
            (
                Instruction::new(
                    Op::CompositeInsert,
                    Some(18),
                    Some(40),
                    vec![
                        Operand::IdRef(13),
                        Operand::IdRef(24),
                        Operand::LiteralBit32(1),
                    ],
                ),
                "native emitter: owned OpCompositeInsert object does not match its index path",
            ),
            (
                Instruction::new(
                    Op::VectorExtractDynamic,
                    Some(15),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::IdRef(13)],
                ),
                "native emitter: owned OpVectorExtractDynamic types are inconsistent",
            ),
            (
                Instruction::new(
                    Op::VectorInsertDynamic,
                    Some(16),
                    Some(40),
                    vec![Operand::IdRef(21), Operand::IdRef(23), Operand::IdRef(13)],
                ),
                "native emitter: owned OpVectorInsertDynamic types are inconsistent",
            ),
            (
                Instruction::new(
                    Op::VectorShuffle,
                    Some(16),
                    Some(40),
                    vec![
                        Operand::IdRef(21),
                        Operand::IdRef(22),
                        Operand::LiteralBit32(0),
                        Operand::LiteralBit32(4),
                    ],
                ),
                "native emitter: owned OpVectorShuffle types are inconsistent",
            ),
        ];

        for (instruction, expected) in cases {
            let module = module_with_composite_instruction(instruction);
            assert_eq!(owned_module_cfg_error(&module).as_deref(), Some(expected));
        }
    }

    #[test]
    fn owned_composite_check_matches_vulkan_validation() {
        let module = module_with_composite_instruction(Instruction::new(
            Op::CompositeExtract,
            Some(12),
            Some(40),
            vec![Operand::IdRef(21), Operand::LiteralBit32(2)],
        ));
        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned composite operation index is out of bounds")
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_composite_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed composite index contract"
        );
    }

    #[test]
    fn owned_module_layout_requires_the_vulkan_header_and_exact_logical_sections() {
        let module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);
        assert_eq!(owned_module_cfg_error(&module), None);

        let mut missing_header = module.clone();
        missing_header.header = None;
        assert_eq!(
            owned_module_cfg_error(&missing_header).as_deref(),
            Some("native emitter: owned module has no SPIR-V header")
        );

        let mut malformed_header = module.clone();
        malformed_header
            .header
            .as_mut()
            .expect("fixture header")
            .reserved_word = 1;
        assert_eq!(
            owned_module_cfg_error(&malformed_header).as_deref(),
            Some("native emitter: owned module has an invalid Vulkan 1.2 SPIR-V header")
        );

        let mut no_capability = module.clone();
        no_capability.capabilities.clear();
        assert_eq!(
            owned_module_cfg_error(&no_capability).as_deref(),
            Some("native emitter: owned module declares no capability")
        );

        let mut no_memory_model = module.clone();
        no_memory_model.memory_model = None;
        assert_eq!(
            owned_module_cfg_error(&no_memory_model).as_deref(),
            Some("native emitter: owned module has no OpMemoryModel")
        );

        let mut misplaced_global = module.clone();
        misplaced_global
            .types_global_values
            .push(inst(Op::Name, vec![Operand::IdRef(32), "misplaced".into()]));
        assert_eq!(
            owned_module_cfg_error(&misplaced_global).as_deref(),
            Some(
                "native emitter: owned module type/constant/global section contains an instruction from another logical section"
            )
        );

        let mut misplaced_block_instruction = module.clone();
        misplaced_block_instruction.functions[0].blocks[0]
            .instructions
            .insert(
                0,
                inst(Op::Name, vec![Operand::IdRef(32), "misplaced".into()]),
            );
        assert_eq!(
            owned_module_cfg_error(&misplaced_block_instruction).as_deref(),
            Some("native emitter: owned function block contains Name from another logical section")
        );

        let mut malformed_label = module;
        malformed_label.functions[0].blocks[0].label =
            Some(Instruction::new(Op::TypeVoid, None, Some(1), vec![]));
        assert_eq!(
            owned_module_cfg_error(&malformed_label).as_deref(),
            Some("native emitter: owned function block label is not OpLabel")
        );
    }

    #[test]
    fn owned_module_linkage_connects_entries_modes_interfaces_and_imports() {
        let module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut wrong_entry_target = module.clone();
        wrong_entry_target.entry_points[0].operands[1] = Operand::IdRef(13);
        assert_eq!(
            owned_module_cfg_error(&wrong_entry_target).as_deref(),
            Some("native emitter: owned OpEntryPoint target is not an owned function")
        );

        let mut local_interface = module.clone();
        local_interface.entry_points[0]
            .operands
            .push(Operand::IdRef(1));
        assert_eq!(
            owned_module_cfg_error(&local_interface).as_deref(),
            Some("native emitter: owned OpEntryPoint interface id is not a global variable")
        );

        let mut wrong_mode_target = module.clone();
        wrong_mode_target.execution_modes[0].operands[0] = Operand::IdRef(13);
        assert_eq!(
            owned_module_cfg_error(&wrong_mode_target).as_deref(),
            Some("native emitter: owned execution mode does not target an entry point")
        );

        let mut wrong_mode_form = module.clone();
        wrong_mode_form.execution_modes[0].class.opcode = Op::ExecutionModeId;
        assert_eq!(
            owned_module_cfg_error(&wrong_mode_form).as_deref(),
            Some("native emitter: owned execution mode uses the wrong literal/id instruction form")
        );

        let mut wrong_import = module.clone();
        wrong_import.functions[0].blocks[0].instructions.insert(
            0,
            Instruction::new(
                Op::ExtInst,
                Some(12),
                Some(42),
                vec![
                    Operand::IdRef(13),
                    Operand::LiteralExtInstInteger(spirv::GlslStd450Op::SAbs as u32),
                    Operand::IdRef(13),
                ],
            ),
        );
        assert_eq!(
            owned_module_cfg_error(&wrong_import).as_deref(),
            Some("native emitter: owned OpExtInst set is not an OpExtInstImport")
        );

        let mut imported = module.clone();
        imported.ext_inst_imports.push(Instruction::new(
            Op::ExtInstImport,
            None,
            Some(40),
            vec![Operand::LiteralString("GLSL.std.450".to_string())],
        ));
        imported.functions[0].blocks[0].instructions.insert(
            0,
            Instruction::new(
                Op::ExtInst,
                Some(12),
                Some(42),
                vec![
                    Operand::IdRef(40),
                    Operand::LiteralExtInstInteger(spirv::GlslStd450Op::SAbs as u32),
                    Operand::IdRef(13),
                ],
            ),
        );
        assert_eq!(owned_module_cfg_error(&imported), None);
    }

    #[test]
    fn owned_entry_interface_covers_globals_used_through_the_static_call_tree() {
        let mut module = module_with_blocks(vec![block(
            1,
            vec![
                Instruction::new(
                    Op::FunctionCall,
                    Some(30),
                    Some(52),
                    vec![Operand::IdRef(50)],
                ),
                inst(Op::Return, vec![]),
            ],
        )]);
        module.types_global_values.extend([
            Instruction::new(
                Op::TypePointer,
                None,
                Some(40),
                vec![
                    Operand::StorageClass(spirv::StorageClass::StorageBuffer),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(40),
                Some(41),
                vec![Operand::StorageClass(spirv::StorageClass::StorageBuffer)],
            ),
        ]);
        let mut helper = Function::new();
        helper.def = Some(Instruction::new(
            Op::Function,
            Some(30),
            Some(50),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(31),
            ],
        ));
        helper.blocks.push(block(
            51,
            vec![
                Instruction::new(Op::Load, Some(12), Some(42), vec![Operand::IdRef(41)]),
                inst(Op::Return, vec![]),
            ],
        ));
        helper.end = Some(inst(Op::FunctionEnd, vec![]));
        module.functions.push(helper);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpEntryPoint omits a global used by its static call tree")
        );
        module.entry_points[0].operands.push(Operand::IdRef(41));
        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_environment_closes_opcode_declarations_and_vulkan_memory_models() {
        let module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut missing_capability = module.clone();
        missing_capability.functions[0].blocks[0]
            .instructions
            .insert(0, inst(Op::DemoteToHelperInvocation, vec![]));
        assert_eq!(
            owned_module_cfg_error(&missing_capability).as_deref(),
            Some("native emitter: owned DemoteToHelperInvocation lacks an enabling capability")
        );

        let mut missing_extension = module.clone();
        missing_extension.functions[0].blocks[0].instructions[0] =
            inst(Op::TerminateInvocation, vec![]);
        assert_eq!(
            owned_module_cfg_error(&missing_extension).as_deref(),
            Some("native emitter: owned TerminateInvocation lacks an enabling extension")
        );

        let mut unavailable_memory_model = module.clone();
        unavailable_memory_model
            .memory_model
            .as_mut()
            .expect("fixture memory model")
            .operands[1] = Operand::MemoryModel(spirv::MemoryModel::OpenCL);
        assert_eq!(
            owned_module_cfg_error(&unavailable_memory_model).as_deref(),
            Some("native emitter: owned memory model is not available in Vulkan 1.2")
        );

        let mut physical_without_declarations = module.clone();
        physical_without_declarations
            .memory_model
            .as_mut()
            .expect("fixture memory model")
            .operands[0] =
            Operand::AddressingModel(spirv::AddressingModel::PhysicalStorageBuffer64);
        assert_eq!(
            owned_module_cfg_error(&physical_without_declarations).as_deref(),
            Some("native emitter: PhysicalStorageBuffer64 lacks its capability")
        );

        let mut promoted_physical = module.clone();
        promoted_physical
            .memory_model
            .as_mut()
            .expect("fixture memory model")
            .operands[0] =
            Operand::AddressingModel(spirv::AddressingModel::PhysicalStorageBuffer64);
        promoted_physical.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(
                spirv::Capability::PhysicalStorageBufferAddresses,
            )],
        ));
        assert_eq!(owned_module_environment_error(&promoted_physical), Ok(()));
        promoted_physical
            .header
            .as_mut()
            .expect("fixture header")
            .set_version(1, 4);
        assert_eq!(
            owned_module_environment_error(&promoted_physical),
            Err("native emitter: owned Capability operand lacks an enabling extension".to_string())
        );

        let mut vulkan_without_declarations = module;
        vulkan_without_declarations
            .memory_model
            .as_mut()
            .expect("fixture memory model")
            .operands[1] = Operand::MemoryModel(spirv::MemoryModel::Vulkan);
        assert_eq!(
            owned_module_cfg_error(&vulkan_without_declarations).as_deref(),
            Some("native emitter: Vulkan memory model lacks its capability")
        );
    }

    #[test]
    fn owned_environment_closes_capability_implications_and_scalar_type_forms() {
        let module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut implied_group_capability = module.clone();
        implied_group_capability.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(
                spirv::Capability::GroupNonUniformArithmetic,
            )],
        ));
        implied_group_capability.functions[0].blocks[0]
            .instructions
            .insert(
                0,
                Instruction::new(
                    Op::GroupNonUniformElect,
                    Some(9),
                    Some(60),
                    vec![Operand::IdScope(13)],
                ),
            );
        assert_eq!(
            owned_module_environment_error(&implied_group_capability),
            Ok(())
        );

        let mut capability_without_extension = module.clone();
        capability_without_extension.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(
                spirv::Capability::FragmentShaderPixelInterlockEXT,
            )],
        ));
        assert_eq!(
            owned_module_environment_error(&capability_without_extension),
            Err("native emitter: owned Capability operand lacks an enabling extension".to_string())
        );

        let mut promoted_capability = module.clone();
        promoted_capability.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(
                spirv::Capability::StorageBuffer8BitAccess,
            )],
        ));
        assert_eq!(owned_module_environment_error(&promoted_capability), Ok(()));

        let mut narrow_integer = module.clone();
        narrow_integer.types_global_values[5].operands[0] = Operand::LiteralBit32(8);
        assert_eq!(
            owned_module_environment_error(&narrow_integer),
            Err("native emitter: owned TypeInt scalar width lacks its capability".to_string())
        );
        narrow_integer.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(spirv::Capability::Int8)],
        ));
        assert_eq!(owned_module_environment_error(&narrow_integer), Ok(()));

        let mut invalid_signedness = module.clone();
        invalid_signedness.types_global_values[5].operands[1] = Operand::LiteralBit32(2);
        assert_eq!(
            owned_module_environment_error(&invalid_signedness),
            Err("native emitter: owned OpTypeInt has invalid signedness".to_string())
        );

        let mut wide_vector = module;
        wide_vector.types_global_values.push(Instruction::new(
            Op::TypeVector,
            None,
            Some(60),
            vec![Operand::IdRef(12), Operand::LiteralBit32(8)],
        ));
        assert_eq!(
            owned_module_environment_error(&wide_vector),
            Err("native emitter: owned wide OpTypeVector lacks Vector16 capability".to_string())
        );
    }

    #[test]
    fn owned_environment_closes_operand_enumerant_requirements() {
        let module = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut builtin = module.clone();
        builtin.annotations.push(inst(
            Op::Decorate,
            vec![
                Operand::IdRef(13),
                Operand::Decoration(spirv::Decoration::BuiltIn),
                Operand::BuiltIn(spirv::BuiltIn::SampleId),
            ],
        ));
        assert_eq!(
            owned_module_environment_error(&builtin),
            Err("native emitter: owned Decorate operand lacks an enabling capability".to_string())
        );

        let mut group_operation = module.clone();
        group_operation.capabilities.push(inst(
            Op::Capability,
            vec![Operand::Capability(
                spirv::Capability::GroupNonUniformArithmetic,
            )],
        ));
        group_operation.functions[0].blocks[0].instructions.insert(
            0,
            Instruction::new(
                Op::GroupNonUniformIAdd,
                Some(12),
                Some(60),
                vec![
                    Operand::IdScope(13),
                    Operand::GroupOperation(spirv::GroupOperation::ClusteredReduce),
                    Operand::IdRef(13),
                    Operand::IdRef(13),
                ],
            ),
        );
        assert_eq!(
            owned_module_environment_error(&group_operation),
            Err(
                "native emitter: owned GroupNonUniformIAdd operand lacks an enabling capability"
                    .to_string()
            )
        );

        let mut storage_class = module.clone();
        storage_class.types_global_values.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(60),
            vec![
                Operand::StorageClass(spirv::StorageClass::PhysicalStorageBuffer),
                Operand::IdRef(12),
            ],
        ));
        assert_eq!(
            owned_module_environment_error(&storage_class),
            Err(
                "native emitter: owned TypePointer operand lacks an enabling capability"
                    .to_string()
            )
        );

        let image_type = |dim, format| {
            Instruction::new(
                Op::TypeImage,
                None,
                Some(60),
                vec![
                    Operand::IdRef(12),
                    Operand::Dim(dim),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(format),
                ],
            )
        };
        let mut image_dimension = module.clone();
        image_dimension.types_global_values.push(image_type(
            spirv::Dim::DimBuffer,
            spirv::ImageFormat::Unknown,
        ));
        assert_eq!(
            owned_module_environment_error(&image_dimension),
            Err("native emitter: owned TypeImage operand lacks an enabling capability".to_string())
        );

        let mut image_format = module;
        image_format
            .types_global_values
            .push(image_type(spirv::Dim::Dim2D, spirv::ImageFormat::Rg32f));
        assert_eq!(
            owned_module_environment_error(&image_format),
            Err("native emitter: owned TypeImage operand lacks an enabling capability".to_string())
        );
    }

    fn with_integer_constant(mut module: Module) -> Module {
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeInt,
                None,
                Some(20),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(20),
                Some(21),
                vec![Operand::LiteralBit32(0)],
            ),
        ]);
        module
    }

    fn with_bool_identity_function(mut module: Module) -> Module {
        module.types_global_values.push(Instruction::new(
            Op::TypeFunction,
            None,
            Some(34),
            vec![Operand::IdRef(9), Operand::IdRef(9)],
        ));
        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(9),
            Some(35),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(34),
            ],
        ));
        function.parameters.push(Instruction::new(
            Op::FunctionParameter,
            Some(9),
            Some(36),
            vec![],
        ));
        function.blocks = vec![block(
            38,
            vec![inst(Op::ReturnValue, vec![Operand::IdRef(36)])],
        )];
        function.end = Some(inst(Op::FunctionEnd, vec![]));
        module.functions.push(function);
        module
    }

    fn copy(result: Word, operand: Word) -> Instruction {
        Instruction::new(
            Op::CopyObject,
            Some(20),
            Some(result),
            vec![Operand::IdRef(operand)],
        )
    }

    fn phi_module(phi_operands: Vec<Operand>, prefix: Vec<Instruction>) -> Module {
        with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(
                4,
                prefix
                    .into_iter()
                    .chain([
                        Instruction::new(Op::Phi, Some(20), Some(22), phi_operands),
                        inst(Op::Return, vec![]),
                    ])
                    .collect(),
            ),
        ]))
    }

    #[test]
    fn owned_cfg_accepts_a_directly_constructed_selection() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_rejects_a_nondominating_merge_before_serialization() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(4)],
                )],
            ),
            block(
                2,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(11), Operand::IdRef(3), Operand::IdRef(4)],
                    ),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned construct header does not structurally dominate its merge")
        );
        assert!(matches!(
            owned_module_failure(&module),
            Some(OwnedModuleFailure::CfgConstruction(error))
                if error
                    == "native emitter: owned construct header does not structurally dominate its merge"
        ));
    }

    #[test]
    fn owned_cfg_rejects_a_backedge_to_a_selection_header() {
        let module = module_with_blocks(vec![
            block(5, vec![inst(Op::Branch, vec![Operand::IdRef(1)])]),
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(1)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned back-edge does not target a loop header")
        );
    }

    #[test]
    fn owned_cfg_accepts_phi_pairs_for_every_predecessor() {
        let module = phi_module(
            vec![
                Operand::IdRef(21),
                Operand::IdRef(2),
                Operand::IdRef(21),
                Operand::IdRef(3),
            ],
            vec![],
        );

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_rejects_phi_with_incomplete_predecessors() {
        let module = phi_module(vec![Operand::IdRef(21), Operand::IdRef(2)], vec![]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpPhi parents do not match block predecessors")
        );
    }

    #[test]
    fn owned_cfg_rejects_phi_after_a_non_phi_instruction() {
        let module = phi_module(
            vec![
                Operand::IdRef(21),
                Operand::IdRef(2),
                Operand::IdRef(21),
                Operand::IdRef(3),
            ],
            vec![inst(Op::Nop, vec![])],
        );

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpPhi follows a non-phi instruction")
        );
    }

    #[test]
    fn owned_cfg_rejects_misplaced_merge_instruction() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(2),
                    inst(Op::Nop, vec![]),
                    inst(Op::Branch, vec![Operand::IdRef(2)]),
                ],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned selection merge does not immediately precede its branch")
        );
    }

    #[test]
    fn owned_cfg_rejects_malformed_control_instruction_operands() {
        let malformed_branch = module_with_blocks(vec![block(
            1,
            vec![inst(
                Op::Branch,
                vec![Operand::IdRef(1), Operand::LiteralBit32(0)],
            )],
        )]);
        assert_eq!(
            owned_module_cfg_error(&malformed_branch).as_deref(),
            Some("native emitter: owned Branch has operands outside the core grammar")
        );

        let malformed_merge = module_with_blocks(vec![
            block(
                1,
                vec![
                    inst(Op::SelectionMerge, vec![Operand::IdRef(2)]),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(2)],
                    ),
                ],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
        ]);
        assert_eq!(
            owned_module_cfg_error(&malformed_merge).as_deref(),
            Some("native emitter: owned SelectionMerge has operands outside the core grammar")
        );
    }

    #[test]
    fn owned_cfg_rejects_invalid_conditional_and_switch_selector_types() {
        let conditional = module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(13), Operand::IdRef(2), Operand::IdRef(2)],
                )],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
        ]);
        assert_eq!(
            owned_module_cfg_error(&conditional).as_deref(),
            Some("native emitter: owned branch condition is not a boolean scalar")
        );

        let switch = module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(2),
                    inst(
                        Op::Switch,
                        vec![
                            Operand::IdRef(10),
                            Operand::IdRef(2),
                            Operand::LiteralBit32(0),
                            Operand::IdRef(2),
                        ],
                    ),
                ],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
        ]);
        assert_eq!(
            owned_module_cfg_error(&switch).as_deref(),
            Some("native emitter: owned switch selector or literals have incompatible types")
        );
    }

    #[test]
    fn owned_cfg_rejects_a_function_signature_mismatch() {
        let mut module = with_integer_constant(module_with_blocks(vec![block(
            1,
            vec![inst(Op::Return, vec![])],
        )]));
        module.functions[0].def.as_mut().unwrap().result_type = Some(20);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpFunction return type disagrees with its function type")
        );
    }

    #[test]
    fn owned_cfg_rejects_a_return_opcode_for_the_wrong_function_type() {
        let module = module_with_blocks(vec![block(
            1,
            vec![inst(Op::ReturnValue, vec![Operand::IdRef(10)])],
        )]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpReturnValue terminates a void function")
        );
    }

    #[test]
    fn owned_cfg_enforces_function_call_signatures() {
        let module = with_bool_identity_function(module_with_blocks(vec![block(
            1,
            vec![
                Instruction::new(
                    Op::FunctionCall,
                    Some(9),
                    Some(40),
                    vec![Operand::IdRef(35), Operand::IdRef(10)],
                ),
                inst(Op::Return, vec![]),
            ],
        )]));
        assert_eq!(owned_module_cfg_error(&module), None);

        let mut wrong_result = module.clone();
        wrong_result.functions[0].blocks[0].instructions[0].result_type = Some(30);
        assert_eq!(
            owned_module_cfg_error(&wrong_result).as_deref(),
            Some("native emitter: owned OpFunctionCall disagrees with its function type")
        );

        let mut wrong_argument = module.clone();
        wrong_argument.functions[0].blocks[0].instructions[0].operands[1] = Operand::IdRef(13);
        assert_eq!(
            owned_module_cfg_error(&wrong_argument).as_deref(),
            Some(
                "native emitter: owned OpFunctionCall argument type disagrees with its function \
                 type: argument 0 is %13, of type %12 (TypeInt 32 0), but the parameter type is \
                 %9 (TypeBool)"
            )
        );

        let mut wrong_arity = module;
        wrong_arity.functions[0].blocks[0].instructions[0]
            .operands
            .pop();
        assert_eq!(
            owned_module_cfg_error(&wrong_arity).as_deref(),
            Some("native emitter: owned OpFunctionCall disagrees with its function type")
        );
    }

    #[test]
    fn owned_cfg_enforces_grammar_result_fields_for_every_opcode() {
        let mut unexpected_type =
            module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);
        unexpected_type.types_global_values[2].result_type = Some(30);
        assert_eq!(
            owned_module_cfg_error(&unexpected_type).as_deref(),
            Some("native emitter: owned TypeBool has malformed result fields")
        );

        let missing_results = module_with_blocks(vec![block(
            1,
            vec![
                inst(Op::IAdd, vec![Operand::IdRef(13), Operand::IdRef(13)]),
                inst(Op::Return, vec![]),
            ],
        )]);
        assert_eq!(
            owned_module_cfg_error(&missing_results).as_deref(),
            Some("native emitter: owned IAdd has malformed result fields")
        );
    }

    #[test]
    fn owned_cfg_requires_every_result_type_to_name_a_type_declaration() {
        let module = module_with_blocks(vec![block(
            1,
            vec![
                Instruction::new(
                    Op::IAdd,
                    Some(13),
                    Some(20),
                    vec![Operand::IdRef(13), Operand::IdRef(13)],
                ),
                inst(Op::Return, vec![]),
            ],
        )]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned instruction result type is not a type declaration")
        );

        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_result_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed result-type category"
        );
    }

    #[test]
    fn owned_cfg_requires_core_type_operands_to_name_type_declarations() {
        let base = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut vector = base.clone();
        vector.types_global_values.push(Instruction::new(
            Op::TypeVector,
            None,
            Some(15),
            vec![Operand::IdRef(13), Operand::LiteralBit32(4)],
        ));

        let mut pointer = base.clone();
        pointer.types_global_values[7].operands[1] = Operand::IdRef(13);

        let mut structure = base.clone();
        structure.types_global_values.push(Instruction::new(
            Op::TypeStruct,
            None,
            Some(15),
            vec![Operand::IdRef(13)],
        ));

        let mut function = base;
        function.types_global_values[1].operands[0] = Operand::IdRef(13);

        for (module, expected) in [
            (
                vector,
                "native emitter: owned TypeVector type operand is not a type declaration",
            ),
            (
                pointer,
                "native emitter: owned TypePointer type operand is not a type declaration",
            ),
            (
                structure,
                "native emitter: owned TypeStruct type operand is not a type declaration",
            ),
            (
                function,
                "native emitter: owned TypeFunction type operand is not a type declaration",
            ),
        ] {
            assert_eq!(owned_module_cfg_error(&module).as_deref(), Some(expected));
        }
    }

    #[test]
    fn owned_cfg_enforces_core_composite_type_categories() {
        let base = module_with_blocks(vec![block(1, vec![inst(Op::Return, vec![])])]);

        let mut vector = base.clone();
        vector.types_global_values.push(Instruction::new(
            Op::TypeVector,
            None,
            Some(15),
            vec![Operand::IdRef(30), Operand::LiteralBit32(4)],
        ));

        let mut matrix = base.clone();
        matrix.types_global_values.push(Instruction::new(
            Op::TypeMatrix,
            None,
            Some(15),
            vec![Operand::IdRef(12), Operand::LiteralBit32(4)],
        ));

        let mut sampled_image = base.clone();
        sampled_image.types_global_values.push(Instruction::new(
            Op::TypeSampledImage,
            None,
            Some(15),
            vec![Operand::IdRef(12)],
        ));

        let mut array = base;
        array.types_global_values.push(Instruction::new(
            Op::TypeArray,
            None,
            Some(15),
            vec![Operand::IdRef(12), Operand::IdRef(10)],
        ));

        for (module, expected) in [
            (
                vector,
                "native emitter: owned OpTypeVector component is not a scalar type",
            ),
            (
                matrix,
                "native emitter: owned OpTypeMatrix column is not a float vector",
            ),
            (
                sampled_image,
                "native emitter: owned OpTypeSampledImage operand is not OpTypeImage",
            ),
            (
                array,
                "native emitter: owned OpTypeArray length is not an integer constant",
            ),
        ] {
            assert_eq!(owned_module_cfg_error(&module).as_deref(), Some(expected));
        }
    }

    #[test]
    fn owned_cfg_rejects_a_late_function_variable() {
        let module = module_with_blocks(vec![block(
            1,
            vec![
                inst(Op::Nop, vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(14),
                    Some(15),
                    vec![Operand::StorageClass(spirv::StorageClass::Function)],
                ),
                inst(Op::Return, vec![]),
            ],
        )]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned function variable is outside the entry prefix")
        );
    }

    #[test]
    fn owned_cfg_rejects_unreachable_branch_to_entry() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Return, vec![])]),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(1)])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned function entry block is a branch target")
        );
    }

    #[test]
    fn owned_cfg_accepts_an_ordinary_dominating_value_use() {
        let module = with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![copy(22, 21), inst(Op::Branch, vec![Operand::IdRef(2)])],
            ),
            block(2, vec![copy(23, 22), inst(Op::Return, vec![])]),
        ]));

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_rejects_a_nondominating_ordinary_value_use() {
        let module = with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                )],
            ),
            block(
                2,
                vec![copy(22, 21), inst(Op::Branch, vec![Operand::IdRef(4)])],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![copy(23, 22), inst(Op::Return, vec![])]),
        ]));

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned SSA definition does not dominate its use: CopyObject %23 \
                 in block %4 reads %22, which is defined in block %2"
            )
        );
    }

    #[test]
    fn remat_a_pure_value_read_from_a_sibling_block_is_re_emitted_there_and_the_contract_holds() {
        let mut module = with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(
                2,
                vec![copy(22, 21), inst(Op::Branch, vec![Operand::IdRef(4)])],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![copy(23, 22), inst(Op::Return, vec![])]),
        ]));
        let mut bound = 100;
        let n = rematerialize_nondominating_pure_uses(&mut module.functions[0], &mut bound);
        module.header.as_mut().unwrap().bound = bound;
        assert_eq!(n, 1, "one definition re-emitted");
        assert_eq!(owned_module_cfg_error(&module), None);
        let join = &module.functions[0].blocks[3].instructions;
        assert_eq!(join[0].result_id, Some(100), "the copy lands above its use");
        assert_eq!(
            join[1].operands,
            vec![Operand::IdRef(100)],
            "the use reads the copy"
        );
    }

    #[test]
    fn remat_a_chain_of_pure_values_is_re_emitted_operands_first() {
        let mut module = with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(
                2,
                vec![
                    copy(22, 21),
                    copy(24, 22),
                    inst(Op::Branch, vec![Operand::IdRef(4)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![copy(23, 24), inst(Op::Return, vec![])]),
        ]));
        let mut bound = 100;
        assert_eq!(
            rematerialize_nondominating_pure_uses(&mut module.functions[0], &mut bound),
            2
        );
        module.header.as_mut().unwrap().bound = bound;
        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn remat_does_not_touch_an_impure_value_and_the_contract_still_fires() {
        let mut module = with_integer_constant(module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(4),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(
                2,
                vec![
                    Instruction::new(
                        Op::IAdd,
                        Some(20),
                        Some(22),
                        vec![Operand::IdRef(21), Operand::IdRef(21)],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(4)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![copy(23, 22), inst(Op::Return, vec![])]),
        ]));
        let mut bound = 100;
        assert_eq!(
            rematerialize_nondominating_pure_uses(&mut module.functions[0], &mut bound),
            0
        );
        assert!(owned_module_cfg_error(&module)
            .unwrap_or_default()
            .contains("does not dominate its use"));
    }

    #[test]
    fn owned_cfg_rejects_a_same_block_forward_value_use() {
        let module = with_integer_constant(module_with_blocks(vec![block(
            1,
            vec![copy(23, 22), copy(22, 21), inst(Op::Return, vec![])],
        )]));

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned SSA value is used before its definition: CopyObject %23 \
                 reads %22, which block %1 defines below it"
            )
        );
    }

    #[test]
    fn owned_cfg_rejects_duplicate_and_undefined_ids() {
        let mut duplicate = with_integer_constant(module_with_blocks(vec![block(
            1,
            vec![copy(10, 21), inst(Op::Return, vec![])],
        )]));
        let err = owned_module_cfg_error(&duplicate).unwrap_or_default();
        assert!(
            err.starts_with("native emitter: owned module defines a result id more than once"),
            "{err}"
        );

        duplicate.functions[0].blocks[0].instructions[0] = copy(22, 99);
        assert_eq!(
            owned_module_cfg_error(&duplicate).as_deref(),
            Some("native emitter: owned CopyObject %22 references undefined id %99")
        );
    }

    #[test]
    fn owned_cfg_rejects_a_value_use_from_another_function() {
        let mut module = with_integer_constant(module_with_blocks(vec![block(
            1,
            vec![copy(22, 21), inst(Op::Return, vec![])],
        )]));
        let mut second = Function::new();
        second.def = Some(Instruction::new(
            Op::Function,
            Some(30),
            Some(33),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(31),
            ],
        ));
        second.end = Some(inst(Op::FunctionEnd, vec![]));
        second.blocks = vec![block(2, vec![copy(23, 22), inst(Op::Return, vec![])])];
        module.functions.push(second);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned function references an id from another function")
        );
    }

    #[test]
    fn owned_cfg_rejects_a_block_serialized_before_its_dominator() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(3)])]),
            block(2, vec![inst(Op::Return, vec![])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned block is serialized before one of its dominators")
        );
    }

    #[test]
    fn owned_cfg_rejects_a_continue_target_entered_outside_its_loop_header() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                )],
            ),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(4),
                            Operand::IdRef(3),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(4, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned loop header does not structurally dominate its continue target"
            )
        );
    }

    #[test]
    fn owned_cfg_rejects_a_continue_target_equal_to_its_merge() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(3),
                            Operand::IdRef(3),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned loop continue target is its merge block")
        );
    }

    #[test]
    fn owned_cfg_accepts_a_backedge_that_postdominates_its_continue_target() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(6),
                            Operand::IdRef(4),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Branch, vec![Operand::IdRef(5)])]),
            block(
                5,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(6)],
                )],
            ),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_rejects_a_backedge_that_does_not_postdominate_its_continue_target() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(6),
                            Operand::IdRef(4),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(
                4,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(5), Operand::IdRef(6)],
                )],
            ),
            block(5, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned loop back-edge does not structurally post-dominate its continue target"
            )
        );
    }

    #[test]
    fn post_dominance_is_built_only_for_a_distinct_continue_and_backedge() {
        let construct = Construct {
            header: 1,
            merge: 4,
            kind: MergeKind::Loop { continue_target: 3 },
            is_switch: false,
        };
        let same_backedge = crate::fxhash::map_from([(1, vec![3])]);
        let distinct_backedge = crate::fxhash::map_from([(1, vec![2])]);

        assert!(!needs_post_dominance(&[], &distinct_backedge));
        assert!(!needs_post_dominance(&[construct], &same_backedge));
        assert!(needs_post_dominance(&[construct], &distinct_backedge));
    }

    #[test]
    fn owned_cfg_rejects_an_external_nonbackedge_continue_entry() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(6),
                            Operand::IdRef(4),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Branch, vec![Operand::IdRef(5)])]),
            block(
                5,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(6)],
                )],
            ),
            block(6, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned non-backedge continue entry originates outside its loop construct"
            )
        );
    }

    #[test]
    fn owned_cfg_rejects_an_illegal_continue_construct_exit() {
        let module = module_with_blocks(vec![
            block(1, vec![inst(Op::Branch, vec![Operand::IdRef(2)])]),
            block(
                2,
                vec![
                    inst(
                        Op::LoopMerge,
                        vec![
                            Operand::IdRef(6),
                            Operand::IdRef(4),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(Op::Branch, vec![Operand::IdRef(3)]),
                ],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(
                4,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(5)],
                )],
            ),
            block(5, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some(
                "native emitter: owned edge exits a continue construct without its loop header or merge"
            )
        );
    }

    fn switch(default: Word, targets: impl IntoIterator<Item = (u32, Word)>) -> Instruction {
        let mut operands = vec![Operand::IdRef(13), Operand::IdRef(default)];
        for (literal, target) in targets {
            operands.push(Operand::LiteralBit32(literal));
            operands.push(Operand::IdRef(target));
        }
        inst(Op::Switch, operands)
    }

    #[test]
    fn owned_cfg_accepts_switch_fallthrough_in_target_order() {
        let module = module_with_blocks(vec![
            block(1, vec![selection_merge(6), switch(6, [(0, 2), (1, 3)])]),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(3)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_rejects_switch_without_selection_merge() {
        let module = module_with_blocks(vec![
            block(1, vec![switch(3, [(0, 2)])]),
            block(2, vec![inst(Op::Return, vec![])]),
            block(3, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpSwitch has no selection merge")
        );
    }

    #[test]
    fn owned_cfg_rejects_nonconsecutive_switch_target_runs() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![selection_merge(6), switch(6, [(0, 2), (1, 3), (2, 2)])],
            ),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned OpSwitch repeats a target nonconsecutively")
        );
    }

    #[test]
    fn owned_cfg_rejects_switch_case_fanout() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![selection_merge(6), switch(6, [(0, 2), (1, 3), (2, 4)])],
            ),
            block(
                2,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(3), Operand::IdRef(4)],
                )],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(4, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned switch case branches to multiple other cases")
        );
    }

    #[test]
    fn owned_cfg_rejects_switch_case_fanin() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![selection_merge(6), switch(6, [(0, 2), (1, 3), (2, 4)])],
            ),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(4, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned switch case is entered by multiple other cases")
        );
    }

    #[test]
    fn owned_cfg_rejects_switch_fallthrough_out_of_target_order() {
        let module = module_with_blocks(vec![
            block(1, vec![selection_merge(6), switch(6, [(0, 3), (1, 2)])]),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(3)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned switch case fallthrough disagrees with target order")
        );
    }

    #[test]
    fn owned_cfg_rejects_switch_case_exit_into_shared_non_case_block() {
        let module = module_with_blocks(vec![
            block(1, vec![selection_merge(6), switch(6, [(0, 2), (1, 3)])]),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(5)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(5)])]),
            block(5, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned switch case exits to a non-structured target")
        );
    }

    #[test]
    fn owned_cfg_rejects_default_bridge_out_of_target_order() {
        let module = module_with_blocks(vec![
            block(1, vec![selection_merge(6), switch(4, [(0, 3), (1, 2)])]),
            block(2, vec![inst(Op::Branch, vec![Operand::IdRef(4)])]),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(6)])]),
            block(4, vec![inst(Op::Branch, vec![Operand::IdRef(3)])]),
            block(6, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned default-case bridge disagrees with target order")
        );
    }

    #[test]
    fn owned_cfg_rejects_divergent_conditional_without_merge_declaration() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                )],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
            block(3, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(
            owned_module_cfg_error(&module).as_deref(),
            Some("native emitter: owned divergent conditional has no merge declaration")
        );
    }

    #[test]
    fn owned_cfg_accepts_identical_conditional_targets_without_merge() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(2)],
                )],
            ),
            block(2, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(owned_module_cfg_error(&module), None);
    }

    #[test]
    fn owned_cfg_accepts_bare_conditional_with_declared_merge_target() {
        let module = module_with_blocks(vec![
            block(
                1,
                vec![
                    selection_merge(5),
                    inst(
                        Op::BranchConditional,
                        vec![Operand::IdRef(10), Operand::IdRef(2), Operand::IdRef(3)],
                    ),
                ],
            ),
            block(
                2,
                vec![inst(
                    Op::BranchConditional,
                    vec![Operand::IdRef(11), Operand::IdRef(3), Operand::IdRef(5)],
                )],
            ),
            block(3, vec![inst(Op::Branch, vec![Operand::IdRef(5)])]),
            block(5, vec![inst(Op::Return, vec![])]),
        ]);

        assert_eq!(owned_module_cfg_error(&module), None);
    }
}
