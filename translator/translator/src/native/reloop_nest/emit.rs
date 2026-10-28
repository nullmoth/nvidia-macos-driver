use super::super::relooper::{block_label, decode_term, Term, TypeCtx};
use crate::fxhash::{HashMap, HashSet};
use super::shape::{Graph, Shape};
use crate::dominators::{build_predecessors, dominance, dominates_interval};
use crate::spirv_module::{Block, Function, Instruction, Operand};
use spirv::{LoopControl, Op, SelectionControl, StorageClass, Word};
use std::collections::BTreeSet;

struct Action {
    label: Word,
    flow: Option<u64>,
}

struct Frame {
    id: usize,
    loop_entries: BTreeSet<Word>,
    continue_label: Option<Word>,
    merge_label: Word,
    next_entries: BTreeSet<Word>,
    dispatches: bool,
}

#[derive(Default)]
struct ConstructFacts {
    destinations: BTreeSet<Word>,
    forwards: bool,
    needs_switch: bool,
}

impl ConstructFacts {
    fn needs_dispatch(&self) -> bool {
        self.forwards || self.destinations.len() > 1
    }
}

struct AnalysisFrame {
    id: usize,
    loop_entries: BTreeSet<Word>,
    next_entries: BTreeSet<Word>,
    is_loop: bool,
}

pub(super) struct Emitter<'a, 'b> {
    tc: &'a mut TypeCtx<'b>,
    blocks: HashMap<Word, Block>,
    terms: HashMap<Word, Term>,
    block_order: Vec<Word>,
    phi_loads: HashMap<Word, Vec<(Word, Word, Word)>>,
    edge_values: HashMap<Word, (Word, Word)>,
    phi_slots: Vec<(Word, Word, Word)>,
    phi_stores: HashMap<(Word, Word), Vec<(Word, Word)>>,
    flow_var: Option<Word>,
    flow_ty: Word,
    flow_id: HashMap<Word, u64>,
    facts: HashMap<usize, ConstructFacts>,
    head: HashMap<usize, Word>,
    merge: HashMap<usize, Word>,
    merge_flow: HashSet<usize>,
    fused: HashSet<usize>,
    forced_dispatch: HashSet<usize>,
    variables: Vec<Instruction>,
    out: Vec<Block>,
}

pub(super) struct Structured {
    pub(super) blocks: Vec<Block>,
    pub(super) flow_variable: Option<Word>,
}

pub(super) fn structure_function(
    function: &Function,
    graph: &Graph,
    shape: &Shape,
    tc: &mut TypeCtx<'_>,
) -> Result<Structured, String> {
    let flow_ty = tc.i32_ty();
    let mut emitter = Emitter {
        tc,
        blocks: HashMap::default(),
        block_order: Vec::new(),
        terms: HashMap::default(),
        phi_loads: HashMap::default(),
        edge_values: HashMap::default(),
        phi_slots: Vec::new(),
        phi_stores: HashMap::default(),
        flow_var: None,
        flow_ty,
        flow_id: HashMap::default(),
        facts: HashMap::default(),
        head: HashMap::default(),
        merge: HashMap::default(),
        merge_flow: HashSet::default(),
        fused: HashSet::default(),
        forced_dispatch: HashSet::default(),
        variables: Vec::new(),
        out: Vec::new(),
    };
    emitter.index(function)?;
    emitter.plan(shape, true)?;
    emitter.analyze(shape, graph, &mut Vec::new())?;
    emitter.assign_merges(shape)?;
    emitter.demote_phis(graph)?;

    let entry_label = emitter.head_of(shape)?;
    emitter.emit(shape, &mut Vec::new())?;
    emitter.recover_dominating_edge_values(entry_label)?;
    emitter.promote_phi_slots();
    emitter.repair_dominance(entry_label)?;

    let prologue_label = emitter.tc.fresh();
    let mut prologue = Block::new();
    prologue.label = Some(Instruction::new(
        Op::Label,
        None,
        Some(prologue_label),
        vec![],
    ));
    let Emitter {
        mut variables,
        flow_var,
        out,
        ..
    } = emitter;
    variables.push(Instruction::new(
        Op::Branch,
        None,
        None,
        vec![Operand::IdRef(entry_label)],
    ));
    prologue.instructions = variables;
    let mut blocks = vec![prologue];
    blocks.extend(out);
    Ok(Structured {
        blocks,
        flow_variable: flow_var,
    })
}

impl Emitter<'_, '_> {

    fn index(&mut self, function: &Function) -> Result<(), String> {
        for block in &function.blocks {
            let label = block_label(block).ok_or_else(|| "block without a label".to_string())?;
            let term = block
                .instructions
                .last()
                .and_then(decode_term)
                .ok_or_else(|| "unhandled terminator".to_string())?;
            self.terms.insert(label, term);
            self.block_order.push(label);
            let mut block = block.clone();
            let mut kept = Vec::with_capacity(block.instructions.len());
            for instruction in std::mem::take(&mut block.instructions) {
                match instruction.class.opcode {
                    Op::Variable => self.variables.push(instruction),
                    Op::SelectionMerge | Op::LoopMerge => {}
                    _ => kept.push(instruction),
                }
            }
            block.instructions = kept;
            self.blocks.insert(label, block);
        }
        Ok(())
    }

    fn plan(&mut self, shape: &Shape, single_predecessor: bool) -> Result<(), String> {
        match shape {
            Shape::Simple { id, label, next } => {
                self.head.insert(*id, *label);
                if let Some(next) = next {
                    self.plan(next, true)?;
                }
            }
            Shape::Loop {
                id,
                entries,
                inner,
                next,
            } => {
                if entries.len() != 1 {
                    return Err("loop with several entries".to_string());
                }
                let header = self.tc.fresh();
                self.head.insert(*id, header);
                self.plan(inner, true)?;
                if let Some(next) = next {
                    self.plan(next, false)?;
                }
            }
            Shape::Multiple {
                id, handled, next, ..
            } => {
                if handled.is_empty() {
                    return Err("dispatch with no arms".to_string());
                }
                if single_predecessor {
                    self.fused.insert(*id);
                } else {
                    let header = self.tc.fresh();
                    self.head.insert(*id, header);
                    self.forced_dispatch.insert(*id);
                }
                for (_, arm) in handled {
                    self.plan(arm, true)?;
                }
                if let Some(next) = next {
                    self.plan(next, false)?;
                }
            }
        }
        Ok(())
    }

    fn head_of(&self, shape: &Shape) -> Result<Word, String> {
        self.head
            .get(&shape.id())
            .copied()
            .ok_or_else(|| "shape has no single entry".to_string())
    }

    fn analyze(
        &mut self,
        shape: &Shape,
        graph: &Graph,
        stack: &mut Vec<AnalysisFrame>,
    ) -> Result<(), String> {
        match shape {
            Shape::Simple { label, next, .. } => {
                let local = next.as_ref().map(|next| next.entry_labels());
                let targets = graph.succ(*label).to_vec();
                let divergent = targets.len() > 1;
                for target in targets {
                    if local.as_ref().is_some_and(|local| local.contains(&target)) {
                        if let Some(next) = next {
                            self.route(next, target);
                        }
                        continue;
                    }
                    self.classify(target, divergent, stack)?;
                }
                if let Some(next) = next {
                    self.analyze(next, graph, stack)?;
                }
            }
            Shape::Loop {
                id,
                entries,
                inner,
                next,
            } => {
                stack.push(AnalysisFrame {
                    id: *id,
                    loop_entries: entries.clone(),
                    next_entries: next
                        .as_ref()
                        .map(|next| next.entry_labels())
                        .unwrap_or_default(),
                    is_loop: true,
                });
                let result = self.analyze(inner, graph, stack);
                stack.pop();
                result?;
                if let Some(next) = next {
                    self.route_destinations(*id, next);
                    self.analyze(next, graph, stack)?;
                }
            }
            Shape::Multiple {
                id, handled, next, ..
            } => {
                stack.push(AnalysisFrame {
                    id: *id,
                    loop_entries: BTreeSet::new(),
                    next_entries: next
                        .as_ref()
                        .map(|next| next.entry_labels())
                        .unwrap_or_default(),
                    is_loop: false,
                });
                let mut result = Ok(());
                for (_, arm) in handled {
                    result = self.analyze(arm, graph, stack);
                    if result.is_err() {
                        break;
                    }
                }
                stack.pop();
                result?;
                if let Some(next) = next {
                    self.route_destinations(*id, next);
                    self.analyze(next, graph, stack)?;
                }
            }
        }
        Ok(())
    }

    fn route_destinations(&mut self, id: usize, next: &Shape) {
        let destinations = self
            .facts
            .get(&id)
            .map(|facts| facts.destinations.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for destination in destinations {
            self.route(next, destination);
        }
    }

    fn route(&mut self, shape: &Shape, target: Word) {
        let Shape::Multiple {
            id, handled, next, ..
        } = shape
        else {
            return;
        };
        if handled.iter().any(|(label, _)| *label == target) {
            return;
        }
        self.facts
            .entry(*id)
            .or_default()
            .destinations
            .insert(target);
        if let Some(next) = next {
            self.route(next, target);
        }
    }

    fn classify(
        &mut self,
        target: Word,
        divergent: bool,
        stack: &[AnalysisFrame],
    ) -> Result<(), String> {
        if let Some(innermost_loop) = stack.iter().rev().find(|frame| frame.is_loop) {
            if innermost_loop.loop_entries.contains(&target) {
                return Ok(());
            }
        }
        if stack
            .iter()
            .any(|frame| frame.loop_entries.contains(&target))
        {
            return Err("branch continues an outer loop".to_string());
        }
        let Some(depth) = stack
            .iter()
            .rposition(|frame| frame.next_entries.contains(&target))
        else {
            return Err("branch target is outside every enclosing construct".to_string());
        };
        self.facts
            .entry(stack[depth].id)
            .or_default()
            .destinations
            .insert(target);
        for frame in &stack[depth + 1..] {
            self.facts.entry(frame.id).or_default().forwards = true;
        }
        if divergent {
            if let Some(innermost) = stack.last() {
                self.facts.entry(innermost.id).or_default().needs_switch = true;
            }
        }
        Ok(())
    }

    fn assign_merges(&mut self, shape: &Shape) -> Result<(), String> {
        match shape {
            Shape::Simple { next, .. } => {
                if let Some(next) = next {
                    self.assign_merges(next)?;
                }
            }
            Shape::Loop {
                id, inner, next, ..
            } => {
                if let Some(next) = next {
                    self.assign_merges(next)?;
                }
                self.assign_construct_merge(*id, next.as_deref())?;
                self.assign_merges(inner)?;
            }
            Shape::Multiple {
                id, handled, next, ..
            } => {
                if let Some(next) = next {
                    self.assign_merges(next)?;
                }
                self.assign_construct_merge(*id, next.as_deref())?;
                for (_, arm) in handled {
                    self.assign_merges(arm)?;
                }
            }
        }
        Ok(())
    }

    fn assign_construct_merge(&mut self, id: usize, next: Option<&Shape>) -> Result<(), String> {
        let dispatches = self.dispatches(id);
        let destination = self
            .facts
            .entry(id)
            .or_default()
            .destinations
            .iter()
            .copied()
            .next();
        let label = if dispatches {
            self.tc.fresh()
        } else {
            match (destination, next) {
                (Some(destination), Some(next)) => {
                    let (label, needs_flow) = self.entry_label(next, destination)?;
                    if needs_flow {
                        self.merge_flow.insert(id);
                    }
                    label
                }
                _ => self.fresh_unreachable(),
            }
        };
        self.merge.insert(id, label);
        Ok(())
    }

    fn entry_label(&self, shape: &Shape, target: Word) -> Result<(Word, bool), String> {
        match shape {
            Shape::Simple { label, .. } if *label == target => Ok((*label, false)),
            Shape::Loop { entries, id, .. } if entries.contains(&target) => self
                .head
                .get(id)
                .copied()
                .map(|header| (header, false))
                .ok_or_else(|| "loop without a header".to_string()),
            Shape::Multiple {
                id, handled, next, ..
            } => {
                if !self.fused.contains(id) {
                    return self
                        .head
                        .get(id)
                        .copied()
                        .map(|header| (header, true))
                        .ok_or_else(|| "dispatch without a header".to_string());
                }
                if let Some((_, arm)) = handled.iter().find(|(label, _)| *label == target) {
                    return Ok((self.head_of(arm)?, false));
                }
                match next {
                    Some(next) => self.entry_label(next, target),
                    None => Err("destination is not an entry of the continuation".to_string()),
                }
            }
            _ => Err("destination is not an entry of the continuation".to_string()),
        }
    }

    fn merge_of(&self, id: usize) -> Result<Word, String> {
        self.merge
            .get(&id)
            .copied()
            .ok_or_else(|| "construct without a merge".to_string())
    }

    fn dispatches(&self, id: usize) -> bool {
        self.forced_dispatch.contains(&id)
            || self
                .facts
                .get(&id)
                .is_some_and(ConstructFacts::needs_dispatch)
    }

    fn fresh_unreachable(&mut self) -> Word {
        let label = self.tc.fresh();
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(label), vec![]));
        block
            .instructions
            .push(Instruction::new(Op::Unreachable, None, None, vec![]));
        self.out.push(block);
        label
    }

    fn demote_phis(&mut self, graph: &Graph) -> Result<(), String> {
        let labels = self.block_order.clone();
        let mut edge_sources = HashSet::default();
        for label in labels {
            let phis = self
                .blocks
                .get(&label)
                .map(|block| {
                    block
                        .instructions
                        .iter()
                        .filter(|inst| inst.class.opcode == Op::Phi)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if phis.is_empty() {
                continue;
            }
            for phi in &phis {
                let result = phi
                    .result_id
                    .ok_or_else(|| "phi without a result".to_string())?;
                let ty = phi
                    .result_type
                    .ok_or_else(|| "phi without a result type".to_string())?;
                let pointer = self.tc.ptr_function(ty);
                let var = self.tc.fresh();
                self.variables.push(Instruction::new(
                    Op::Variable,
                    Some(pointer),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Function)],
                ));
                self.phi_loads
                    .entry(label)
                    .or_default()
                    .push((result, var, ty));
                self.phi_slots.push((label, result, var));
                self.edge_values.insert(result, (var, ty));
                let mut index = 0;
                while index + 1 < phi.operands.len() {
                    let (Operand::IdRef(value), Operand::IdRef(predecessor)) =
                        (&phi.operands[index], &phi.operands[index + 1])
                    else {
                        return Err("phi operand shape".to_string());
                    };
                    if graph.successors.contains_key(predecessor) {
                        edge_sources.insert(*value);
                        self.phi_stores
                            .entry((*predecessor, label))
                            .or_default()
                            .push((var, *value));
                    }
                    index += 2;
                }
            }
            if let Some(block) = self.blocks.get_mut(&label) {
                block
                    .instructions
                    .retain(|inst| inst.class.opcode != Op::Phi);
            }
        }
        for label in &self.block_order {
            let Some(block) = self.blocks.get_mut(label) else {
                continue;
            };
            let mut index = 0;
            while index < block.instructions.len() {
                let instruction = &block.instructions[index];
                let value = instruction.result_id;
                let ty = instruction.result_type;
                if let (Some(value), Some(ty)) = (value, ty) {
                    if edge_sources.contains(&value)
                        && !self.edge_values.contains_key(&value)
                        && matches!(
                            self.tc.type_opcode(ty),
                            Some(Op::TypeBool | Op::TypeInt | Op::TypeFloat | Op::TypeVector)
                        )
                    {
                        let pointer = self.tc.ptr_function(ty);
                        let var = self.tc.fresh();
                        self.variables.push(Instruction::new(
                            Op::Variable,
                            Some(pointer),
                            Some(var),
                            vec![Operand::StorageClass(StorageClass::Function)],
                        ));
                        self.edge_values.insert(value, (var, ty));
                        block.instructions.insert(
                            index + 1,
                            Instruction::new(
                                Op::Store,
                                None,
                                None,
                                vec![Operand::IdRef(var), Operand::IdRef(value)],
                            ),
                        );
                        index += 1;
                    }
                }
                index += 1;
            }
        }
        Ok(())
    }

    fn flow_variable(&mut self) -> Word {
        if let Some(var) = self.flow_var {
            return var;
        }
        let pointer = self.tc.ptr_function(self.flow_ty);
        let var = self.tc.fresh();
        self.variables.push(Instruction::new(
            Op::Variable,
            Some(pointer),
            Some(var),
            vec![Operand::StorageClass(StorageClass::Function)],
        ));
        self.flow_var = Some(var);
        var
    }

    fn flow_id_of(&mut self, label: Word) -> u64 {
        let next = self.flow_id.len() as u64 + 1;
        *self.flow_id.entry(label).or_insert(next)
    }

    fn resolve(
        &mut self,
        target: Word,
        local: Option<&Shape>,
        stack: &[Frame],
    ) -> Result<Action, String> {
        if let Some(local) = local {
            if local.entry_labels().contains(&target) {
                return self.enter(local, target);
            }
        }
        if let Some(frame) = stack
            .iter()
            .rev()
            .find(|frame| frame.continue_label.is_some())
        {
            if frame.loop_entries.contains(&target) {
                let label = frame
                    .continue_label
                    .ok_or_else(|| "loop frame without a continue target".to_string())?;
                return Ok(Action { label, flow: None });
            }
        }
        if !stack
            .iter()
            .any(|frame| frame.next_entries.contains(&target))
        {
            return Err("branch target is outside every enclosing construct".to_string());
        }
        let frame = stack
            .last()
            .ok_or_else(|| "branch leaves a construct at the top level".to_string())?;
        let records_flow = frame.dispatches || self.merge_flow.contains(&frame.id);
        let label = frame.merge_label;
        let flow = records_flow.then(|| self.flow_id_of(target));
        Ok(Action { label, flow })
    }

    fn enter(&mut self, shape: &Shape, target: Word) -> Result<Action, String> {
        match shape {
            Shape::Simple { .. } | Shape::Loop { .. } => {
                let (label, _) = self.entry_label(shape, target)?;
                Ok(Action { label, flow: None })
            }
            Shape::Multiple { id, handled, .. } => {
                if handled.iter().any(|(label, _)| *label == target) {
                    let (label, needs_flow) = self.entry_label(shape, target)?;
                    let flow = needs_flow.then(|| self.flow_id_of(target));
                    return Ok(Action { label, flow });
                }
                let label = self.merge_of(*id)?;
                let flow = self.dispatches(*id).then(|| self.flow_id_of(target));
                Ok(Action { label, flow })
            }
        }
    }

    fn emit(&mut self, shape: &Shape, stack: &mut Vec<Frame>) -> Result<(), String> {
        match shape {
            Shape::Simple { label, next, .. } => self.emit_simple(*label, next.as_deref(), stack),
            Shape::Loop {
                id,
                entries,
                inner,
                next,
            } => self.emit_loop(*id, entries, inner, next.as_deref(), stack),
            Shape::Multiple {
                id, handled, next, ..
            } => self.emit_multiple(*id, handled, next.as_deref(), stack),
        }
    }

    fn emit_simple(
        &mut self,
        label: Word,
        next: Option<&Shape>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), String> {
        let source = self
            .blocks
            .get(&label)
            .cloned()
            .ok_or_else(|| "missing block".to_string())?;
        let term = self
            .terms
            .get(&label)
            .cloned()
            .ok_or_else(|| "missing terminator".to_string())?;
        let mut block = Block::new();
        block.label = source.label.clone();
        for (result, var, ty) in self.phi_loads.get(&label).cloned().unwrap_or_default() {
            block.instructions.push(Instruction::new(
                Op::Load,
                Some(ty),
                Some(result),
                vec![Operand::IdRef(var)],
            ));
        }
        let body = source.instructions.len().saturating_sub(1);
        block
            .instructions
            .extend(source.instructions.iter().take(body).cloned());
        self.emit_terminator(label, term, &mut block, next, stack)?;
        self.out.push(block);
        if let Some(next) = next {
            self.emit(next, stack)?;
        }
        Ok(())
    }

    fn emit_terminator(
        &mut self,
        label: Word,
        term: Term,
        block: &mut Block,
        next: Option<&Shape>,
        stack: &[Frame],
    ) -> Result<(), String> {
        match term {
            Term::Return => {
                block
                    .instructions
                    .push(Instruction::new(Op::Return, None, None, vec![]));
                Ok(())
            }
            Term::ReturnValue(value) => {
                block.instructions.push(Instruction::new(
                    Op::ReturnValue,
                    None,
                    None,
                    vec![Operand::IdRef(value)],
                ));
                Ok(())
            }
            Term::Unreachable => {
                block
                    .instructions
                    .push(Instruction::new(Op::Unreachable, None, None, vec![]));
                Ok(())
            }
            Term::Kill(instruction) => {
                block.instructions.push(instruction);
                Ok(())
            }
            Term::Branch(target) => {
                let action = self.resolve(target, next, stack)?;
                self.finish_single_edge(label, target, action, block);
                Ok(())
            }
            Term::BranchCond(condition, on_true, on_false) => {
                if on_true == on_false {
                    let action = self.resolve(on_true, next, stack)?;
                    self.finish_single_edge(label, on_true, action, block);
                    return Ok(());
                }
                let true_label = self.edge_label(label, on_true, next, stack)?;
                let false_label = self.edge_label(label, on_false, next, stack)?;
                let merge = self.selection_merge(next)?;
                block.instructions.push(Instruction::new(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(merge),
                        Operand::SelectionControl(SelectionControl::NONE),
                    ],
                ));
                if self.needs_switch(next) {
                    let selector = self.widen_condition(condition, block);
                    block.instructions.push(Instruction::new(
                        Op::Switch,
                        None,
                        None,
                        vec![
                            Operand::IdRef(selector),
                            Operand::IdRef(false_label),
                            Operand::LiteralBit32(1),
                            Operand::IdRef(true_label),
                        ],
                    ));
                    return Ok(());
                }
                block.instructions.push(Instruction::new(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![
                        Operand::IdRef(condition),
                        Operand::IdRef(true_label),
                        Operand::IdRef(false_label),
                    ],
                ));
                Ok(())
            }
            Term::Switch(selector, default, cases) => {
                let default_label = self.edge_label(label, default, next, stack)?;
                let mut operands = vec![Operand::IdRef(selector), Operand::IdRef(default_label)];
                for (literal, target) in &cases {
                    let case_label = self.edge_label(label, *target, next, stack)?;
                    operands.push(if *literal > u64::from(u32::MAX) {
                        Operand::LiteralBit64(*literal)
                    } else {
                        Operand::LiteralBit32(*literal as u32)
                    });
                    operands.push(Operand::IdRef(case_label));
                }
                let merge = self.selection_merge(next)?;
                block.instructions.push(Instruction::new(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(merge),
                        Operand::SelectionControl(SelectionControl::NONE),
                    ],
                ));
                block
                    .instructions
                    .push(Instruction::new(Op::Switch, None, None, operands));
                Ok(())
            }
        }
    }

    fn needs_switch(&self, next: Option<&Shape>) -> bool {
        matches!(next, Some(Shape::Multiple { id, .. })
            if self.facts.get(id).is_some_and(|facts| facts.needs_switch))
    }

    fn widen_condition(&mut self, condition: Word, block: &mut Block) -> Word {
        let one = self.tc.int_const(self.flow_ty, 1);
        let zero = self.tc.int_const(self.flow_ty, 0);
        let selector = self.tc.fresh();
        let selection = Instruction::new(
            Op::Select,
            Some(self.flow_ty),
            Some(selector),
            vec![
                Operand::IdRef(condition),
                Operand::IdRef(one),
                Operand::IdRef(zero),
            ],
        );
        let position = block.instructions.len().saturating_sub(1);
        block.instructions.insert(position, selection);
        selector
    }

    fn selection_merge(&mut self, next: Option<&Shape>) -> Result<Word, String> {
        match next {
            Some(Shape::Multiple { id, .. }) => self.merge_of(*id),
            Some(shape) => self.head_of(shape),
            None => Ok(self.fresh_unreachable()),
        }
    }

    fn edge_label(
        &mut self,
        from: Word,
        target: Word,
        next: Option<&Shape>,
        stack: &[Frame],
    ) -> Result<Word, String> {
        let action = self.resolve(target, next, stack)?;
        let stores = self
            .phi_stores
            .get(&(from, target))
            .cloned()
            .unwrap_or_default();
        if stores.is_empty() && action.flow.is_none() {
            return Ok(action.label);
        }
        let helper = self.tc.fresh();
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(helper), vec![]));
        self.write_edge_stores(&stores, action.flow, &mut block);
        block.instructions.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(action.label)],
        ));
        self.out.push(block);
        Ok(helper)
    }

    fn finish_single_edge(&mut self, from: Word, target: Word, action: Action, block: &mut Block) {
        let stores = self
            .phi_stores
            .get(&(from, target))
            .cloned()
            .unwrap_or_default();
        self.write_edge_stores(&stores, action.flow, block);
        block.instructions.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(action.label)],
        ));
    }

    fn write_edge_stores(&mut self, stores: &[(Word, Word)], flow: Option<u64>, block: &mut Block) {
        let mut reads = HashMap::default();
        for (_, value) in stores {
            if reads.contains_key(value) {
                continue;
            }
            if let Some(&(slot, ty)) = self.edge_values.get(value) {
                let loaded = self.tc.fresh();
                block.instructions.push(Instruction::new(
                    Op::Load,
                    Some(ty),
                    Some(loaded),
                    vec![Operand::IdRef(slot)],
                ));
                reads.insert(*value, loaded);
            }
        }
        for (var, original) in stores {
            let value = reads.get(original).copied().unwrap_or(*original);
            block.instructions.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(*var), Operand::IdRef(value)],
            ));
        }
        if let Some(flow) = flow {
            let variable = self.flow_variable();
            let constant = self.tc.int_const(self.flow_ty, flow);
            block.instructions.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(variable), Operand::IdRef(constant)],
            ));
        }
    }

    fn emit_loop(
        &mut self,
        id: usize,
        entries: &BTreeSet<Word>,
        inner: &Shape,
        next: Option<&Shape>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), String> {
        let header = self
            .head
            .get(&id)
            .copied()
            .ok_or_else(|| "loop without a header".to_string())?;
        let continue_label = self.tc.fresh();
        let merge_label = self.merge_of(id)?;
        let body_start = self.head_of(inner)?;
        let mut head = Block::new();
        head.label = Some(Instruction::new(Op::Label, None, Some(header), vec![]));
        head.instructions.push(Instruction::new(
            Op::LoopMerge,
            None,
            None,
            vec![
                Operand::IdRef(merge_label),
                Operand::IdRef(continue_label),
                Operand::LoopControl(LoopControl::NONE),
            ],
        ));
        head.instructions.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(body_start)],
        ));
        self.out.push(head);

        stack.push(Frame {
            id,
            loop_entries: entries.clone(),
            continue_label: Some(continue_label),
            merge_label,
            next_entries: next.map(Shape::entry_labels).unwrap_or_default(),
            dispatches: self.dispatches(id),
        });
        let result = self.emit(inner, stack);
        stack.pop();
        result?;

        let mut latch = Block::new();
        latch.label = Some(Instruction::new(
            Op::Label,
            None,
            Some(continue_label),
            vec![],
        ));
        latch.instructions.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(header)],
        ));
        self.out.push(latch);

        self.emit_construct_tail(id, next, stack)
    }

    fn emit_multiple(
        &mut self,
        id: usize,
        handled: &[(Word, Shape)],
        next: Option<&Shape>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), String> {
        let merge_label = self.merge_of(id)?;
        if !self.fused.contains(&id) {
            self.emit_dispatch_header(id, handled, merge_label)?;
        }
        stack.push(Frame {
            id,
            loop_entries: BTreeSet::new(),
            continue_label: None,
            merge_label,
            next_entries: next.map(Shape::entry_labels).unwrap_or_default(),
            dispatches: self.dispatches(id),
        });
        let mut result = Ok(());
        for (_, arm) in handled {
            result = self.emit(arm, stack);
            if result.is_err() {
                break;
            }
        }
        stack.pop();
        result?;

        self.emit_construct_tail(id, next, stack)
    }

    fn emit_dispatch_header(
        &mut self,
        id: usize,
        handled: &[(Word, Shape)],
        merge_label: Word,
    ) -> Result<(), String> {
        let header = self
            .head
            .get(&id)
            .copied()
            .ok_or_else(|| "dispatch without a header".to_string())?;
        let variable = self.flow_variable();
        let loaded = self.tc.fresh();
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(header), vec![]));
        block.instructions.push(Instruction::new(
            Op::Load,
            Some(self.flow_ty),
            Some(loaded),
            vec![Operand::IdRef(variable)],
        ));
        let mut operands = vec![Operand::IdRef(loaded), Operand::IdRef(merge_label)];
        for (arm, shape) in handled {
            let flow = self.flow_id_of(*arm);
            let head = self.head_of(shape)?;
            operands.push(Operand::LiteralBit32(flow as u32));
            operands.push(Operand::IdRef(head));
        }
        block.instructions.push(Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(merge_label),
                Operand::SelectionControl(SelectionControl::NONE),
            ],
        ));
        block
            .instructions
            .push(Instruction::new(Op::Switch, None, None, operands));
        self.out.push(block);
        Ok(())
    }

    fn join_of(
        &self,
        block: Word,
        var: Word,
        predecessors: &HashMap<Word, Vec<Word>>,
        index: &HashMap<Word, usize>,
    ) -> Word {
        let mut join = block;
        while let Some([only]) = predecessors.get(&join).map(Vec::as_slice) {
            let writes_slot = index.get(only).is_some_and(|position| {
                self.out[*position].instructions.iter().any(|instruction| {
                    instruction.class.opcode == Op::Store
                        && instruction.operands.first() == Some(&Operand::IdRef(var))
                })
            });
            if writes_slot {
                break;
            }
            join = *only;
        }
        join
    }

    fn recover_dominating_edge_values(&mut self, entry: Word) -> Result<(), String> {
        let index = self
            .out
            .iter()
            .enumerate()
            .filter_map(|(position, block)| Some((block_label(block)?, position + 1)))
            .collect::<HashMap<_, _>>();
        let mut successors = vec![Vec::new(); self.out.len() + 1];
        successors[0].push(*index.get(&entry).ok_or("missing nested entry")?);
        let mut definitions = HashMap::default();
        for (position, block) in self.out.iter().enumerate() {
            for (instruction_position, instruction) in block.instructions.iter().enumerate() {
                if let Some(id) = instruction.result_id {
                    definitions.insert(id, (position + 1, instruction_position));
                }
            }
            let term = block
                .instructions
                .last()
                .and_then(decode_term)
                .ok_or("missing nested terminator")?;
            let targets = match term {
                Term::Branch(target) => vec![target],
                Term::BranchCond(_, yes, no) => vec![yes, no],
                Term::Switch(_, default, cases) => {
                    let mut targets = vec![default];
                    targets.extend(cases.into_iter().map(|(_, label)| label));
                    targets
                }
                Term::Return | Term::ReturnValue(_) | Term::Unreachable | Term::Kill(_) => vec![],
            };
            for target in targets {
                successors[position + 1].push(*index.get(&target).ok_or("missing nested target")?);
            }
        }
        let (_, intervals, _) = dominance(&successors, &build_predecessors(&successors));
        let owners = self
            .edge_values
            .iter()
            .map(|(&value, &(slot, _))| (slot, value))
            .collect::<HashMap<_, _>>();
        let phi_slots = self
            .phi_slots
            .iter()
            .map(|(_, _, slot)| *slot)
            .collect::<HashSet<_>>();
        let mut replacements = HashMap::default();
        let mut retained_slots = HashSet::default();
        for (position, block) in self.out.iter().enumerate() {
            for (instruction_position, instruction) in block.instructions.iter().enumerate() {
                if instruction.class.opcode != Op::Load {
                    continue;
                }
                let (Some(Operand::IdRef(slot)), Some(loaded)) =
                    (instruction.operands.first(), instruction.result_id)
                else {
                    continue;
                };
                let Some(&value) = owners.get(slot) else {
                    continue;
                };
                if loaded == value {
                    continue;
                }
                let safe = definitions.get(&value).is_some_and(|&(producer, at)| {
                    dominates_interval(&intervals, producer, position + 1)
                        && (producer != position + 1 || at < instruction_position)
                });
                if safe {
                    replacements.insert(loaded, value);
                } else {
                    retained_slots.insert(*slot);
                }
            }
        }
        let removed_slots = owners
            .keys()
            .copied()
            .filter(|slot| !phi_slots.contains(slot) && !retained_slots.contains(slot))
            .collect::<HashSet<_>>();
        for block in &mut self.out {
            block.instructions.retain(|instruction| {
                !(instruction.class.opcode == Op::Load
                    && instruction
                        .result_id
                        .is_some_and(|id| replacements.contains_key(&id)))
                    && !(instruction.class.opcode == Op::Store
                        && matches!(instruction.operands.first(), Some(Operand::IdRef(slot))
                            if removed_slots.contains(slot)))
            });
            for instruction in &mut block.instructions {
                for operand in &mut instruction.operands {
                    if let Operand::IdRef(id) = operand {
                        if let Some(&replacement) = replacements.get(id) {
                            *id = replacement;
                        }
                    }
                }
            }
        }
        self.variables.retain(|variable| {
            !variable
                .result_id
                .is_some_and(|id| removed_slots.contains(&id))
        });
        Ok(())
    }

    /// Nesting routes some edges through dispatch headers that other arms also reach, so a value
    /// defined on the source block of such an edge no longer dominates the block that used it
    /// (the original branch was direct). Carry every such value through a function variable:
    /// one store after its definition, one load before the first use in each offending block
    /// (or at the end of the offending phi predecessor).
    fn repair_dominance(&mut self, entry: Word) -> Result<(), String> {
        for _round in 0..16 {
            let index = self
                .out
                .iter()
                .enumerate()
                .filter_map(|(position, block)| Some((block_label(block)?, position + 1)))
                .collect::<HashMap<_, _>>();
            let mut successors = vec![Vec::new(); self.out.len() + 1];
            successors[0].push(*index.get(&entry).ok_or("missing nested entry")?);
            for (position, block) in self.out.iter().enumerate() {
                let term = block
                    .instructions
                    .last()
                    .and_then(decode_term)
                    .ok_or("missing nested terminator")?;
                let targets = match term {
                    Term::Branch(target) => vec![target],
                    Term::BranchCond(_, yes, no) => vec![yes, no],
                    Term::Switch(_, default, cases) => {
                        let mut targets = vec![default];
                        targets.extend(cases.into_iter().map(|(_, label)| label));
                        targets
                    }
                    Term::Return | Term::ReturnValue(_) | Term::Unreachable | Term::Kill(_) => {
                        vec![]
                    }
                };
                for target in targets {
                    successors[position + 1]
                        .push(*index.get(&target).ok_or("missing nested target")?);
                }
            }
            let (_, intervals, _) = dominance(&successors, &build_predecessors(&successors));
            let mut definitions = HashMap::default();
            for (position, block) in self.out.iter().enumerate() {
                for (at, instruction) in block.instructions.iter().enumerate() {
                    if let Some(id) = instruction.result_id {
                        definitions.insert(id, (position + 1, at, instruction.result_type));
                    }
                }
            }
            // (use block position, instruction index, value, phi predecessor)
            let mut violations: Vec<(usize, usize, Word, Option<Word>)> = Vec::new();
            for (position, block) in self.out.iter().enumerate() {
                for (at, instruction) in block.instructions.iter().enumerate() {
                    if instruction.class.opcode == Op::Phi {
                        let mut k = 0;
                        while k + 1 < instruction.operands.len() {
                            if let (Operand::IdRef(value), Operand::IdRef(pred)) =
                                (&instruction.operands[k], &instruction.operands[k + 1])
                            {
                                if let (Some(&(dp, _, _)), Some(&pp)) =
                                    (definitions.get(value), index.get(pred))
                                {
                                    if !dominates_interval(&intervals, dp, pp) {
                                        violations.push((position, at, *value, Some(*pred)));
                                    }
                                }
                            }
                            k += 2;
                        }
                        continue;
                    }
                    for operand in &instruction.operands {
                        let Operand::IdRef(id) = operand else { continue };
                        let Some(&(dp, _, _)) = definitions.get(id) else { continue };
                        if dp != position + 1 && !dominates_interval(&intervals, dp, position + 1) {
                            violations.push((position, at, *id, None));
                        }
                    }
                }
            }
            if violations.is_empty() {
                return Ok(());
            }
            let mut slots: HashMap<Word, Word> = HashMap::default();
            // (block position, instruction index, instruction) in collection order
            let mut insertions: Vec<(usize, usize, Instruction)> = Vec::new();
            let is_merge = |instruction: &Instruction| {
                matches!(instruction.class.opcode, Op::LoopMerge | Op::SelectionMerge)
            };
            let slot_for = |value: Word,
                                slots: &mut HashMap<Word, Word>,
                                insertions: &mut Vec<(usize, usize, Instruction)>,
                                tc: &mut TypeCtx<'_>,
                                variables: &mut Vec<Instruction>,
                                out: &[Block]|
             -> Result<(Word, Word), String> {
                let &(dp, di, ty) = definitions
                    .get(&value)
                    .ok_or("dominance repair lost a definition")?;
                let ty = ty.ok_or("dominance repair on a value without a type")?;
                if let Some(&var) = slots.get(&value) {
                    return Ok((var, ty));
                }
                if !matches!(
                    tc.type_opcode(ty),
                    Some(
                        Op::TypeBool
                            | Op::TypeInt
                            | Op::TypeFloat
                            | Op::TypeVector
                            | Op::TypeMatrix
                            | Op::TypeArray
                            | Op::TypeStruct
                    )
                ) {
                    return Err(format!(
                        "nesting broke the dominance of %{value}, which cannot live in a variable"
                    ));
                }
                let pointer = tc.ptr_function(ty);
                let var = tc.fresh();
                variables.push(Instruction::new(
                    Op::Variable,
                    Some(pointer),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Function)],
                ));
                let block = &out[dp - 1];
                let mut store_at = di + 1;
                if block.instructions[di].class.opcode == Op::Phi {
                    while store_at < block.instructions.len()
                        && block.instructions[store_at].class.opcode == Op::Phi
                    {
                        store_at += 1;
                    }
                }
                insertions.push((
                    dp - 1,
                    store_at,
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(var), Operand::IdRef(value)],
                    ),
                ));
                slots.insert(value, var);
                Ok((var, ty))
            };
            // Ordinary uses: one load per (block, value) before the first use.
            let mut loaded: HashMap<(usize, Word), Word> = HashMap::default();
            let mut phi_loaded: HashMap<(Word, Word), Word> = HashMap::default();
            let mut rewrites: Vec<(usize, usize, Word, Option<Word>, Word)> = Vec::new();
            for &(position, at, value, pred) in &violations {
                let (var, ty) = slot_for(
                    value,
                    &mut slots,
                    &mut insertions,
                    self.tc,
                    &mut self.variables,
                    &self.out,
                )?;
                match pred {
                    None => {
                        let id = if let Some(&id) = loaded.get(&(position, value)) {
                            id
                        } else {
                            let id = self.tc.fresh();
                            let mut load_at = at;
                            if load_at > 0 && is_merge(&self.out[position].instructions[load_at - 1]) {
                                load_at -= 1;
                            }
                            insertions.push((
                                position,
                                load_at,
                                Instruction::new(
                                    Op::Load,
                                    Some(ty),
                                    Some(id),
                                    vec![Operand::IdRef(var)],
                                ),
                            ));
                            loaded.insert((position, value), id);
                            id
                        };
                        rewrites.push((position, at, value, None, id));
                    }
                    Some(pred) => {
                        let id = if let Some(&id) = phi_loaded.get(&(pred, value)) {
                            id
                        } else {
                            let id = self.tc.fresh();
                            let pp = *index.get(&pred).ok_or("missing phi predecessor")? - 1;
                            let block = &self.out[pp];
                            let mut load_at = block.instructions.len() - 1;
                            if load_at > 0 && is_merge(&block.instructions[load_at - 1]) {
                                load_at -= 1;
                            }
                            insertions.push((
                                pp,
                                load_at,
                                Instruction::new(
                                    Op::Load,
                                    Some(ty),
                                    Some(id),
                                    vec![Operand::IdRef(var)],
                                ),
                            ));
                            phi_loaded.insert((pred, value), id);
                            id
                        };
                        rewrites.push((position, at, value, Some(pred), id));
                    }
                }
            }
            for &(position, at, value, pred, id) in &rewrites {
                let instruction = &mut self.out[position].instructions[at];
                match pred {
                    None => {
                        for operand in &mut instruction.operands {
                            if *operand == Operand::IdRef(value) {
                                *operand = Operand::IdRef(id);
                            }
                        }
                    }
                    Some(pred) => {
                        let mut k = 0;
                        while k + 1 < instruction.operands.len() {
                            if instruction.operands[k] == Operand::IdRef(value)
                                && instruction.operands[k + 1] == Operand::IdRef(pred)
                            {
                                instruction.operands[k] = Operand::IdRef(id);
                            }
                            k += 2;
                        }
                    }
                }
            }
            let mut by_block: HashMap<usize, Vec<(usize, Instruction)>> = HashMap::default();
            for (position, at, instruction) in insertions {
                by_block.entry(position).or_default().push((at, instruction));
            }
            for (position, mut list) in by_block {
                // Insert from the back so earlier indices stay valid; equal indices keep collection order.
                list.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
                let mut k = 0;
                while k < list.len() {
                    let at = list[k].0;
                    let mut end = k;
                    while end < list.len() && list[end].0 == at {
                        end += 1;
                    }
                    for j in (k..end).rev() {
                        let instruction = list[j].1.clone();
                        self.out[position].instructions.insert(at, instruction);
                    }
                    k = end;
                }
            }
        }
        Err("nesting could not repair SSA dominance".to_string())
    }

    fn promote_phi_slots(&mut self) {
        let mut predecessors: HashMap<Word, Vec<Word>> = HashMap::default();
        for block in &self.out {
            let Some(label) = block_label(block) else {
                continue;
            };
            let Some(term) = block.instructions.last().and_then(decode_term) else {
                continue;
            };
            let targets = match term {
                Term::Branch(target) => vec![target],
                Term::BranchCond(_, on_true, on_false) => vec![on_true, on_false],
                Term::Switch(_, default, cases) => {
                    let mut targets = vec![default];
                    targets.extend(cases.into_iter().map(|(_, label)| label));
                    targets
                }
                Term::Return | Term::ReturnValue(_) | Term::Unreachable | Term::Kill(_) => vec![],
            };
            for target in targets {
                let list = predecessors.entry(target).or_default();
                if !list.contains(&label) {
                    list.push(label);
                }
            }
        }
        let index = self
            .out
            .iter()
            .enumerate()
            .filter_map(|(position, block)| Some((block_label(block)?, position)))
            .collect::<HashMap<_, _>>();

        let mut load_counts = HashMap::<Word, usize>::default();
        for instruction in self.out.iter().flat_map(|block| &block.instructions) {
            if instruction.class.opcode == Op::Load {
                if let Some(Operand::IdRef(slot)) = instruction.operands.first() {
                    *load_counts.entry(*slot).or_default() += 1;
                }
            }
        }
        let mut promoted = HashSet::default();
        for &(loaded_in, result, var) in &self.phi_slots {
            if load_counts.get(&var) != Some(&1) {
                continue;
            }
            let block = self.join_of(loaded_in, var, &predecessors, &index);
            let Some(incoming) = predecessors.get(&block) else {
                continue;
            };
            let mut operands = Vec::with_capacity(incoming.len() * 2);
            let mut carriers = Vec::with_capacity(incoming.len());
            let mut usable = !incoming.is_empty();
            for predecessor in incoming {
                let Some(source) = index.get(predecessor).map(|position| &self.out[*position])
                else {
                    usable = false;
                    break;
                };
                if !matches!(
                    source.instructions.last().and_then(decode_term),
                    Some(Term::Branch(_))
                ) {
                    usable = false;
                    break;
                }
                let mut carrier = *predecessor;
                let value = loop {
                    let Some(&position) = index.get(&carrier) else {
                        break None;
                    };
                    let mut stored =
                        self.out[position]
                            .instructions
                            .iter()
                            .filter_map(|instruction| {
                                (instruction.class.opcode == Op::Store
                                    && instruction.operands.first() == Some(&Operand::IdRef(var)))
                                .then(|| instruction.operands.get(1).cloned())
                                .flatten()
                            });
                    match (stored.next(), stored.next()) {
                        (Some(value), None) => break Some(value),
                        (Some(_), Some(_)) => break None,
                        (None, _) => {}
                    }
                    let ancestors = predecessors.get(&carrier).map(Vec::as_slice).unwrap_or(&[]);
                    let [only] = ancestors else {
                        break None;
                    };
                    if !matches!(
                        index
                            .get(only)
                            .and_then(|position| self.out[*position].instructions.last())
                            .and_then(decode_term),
                        Some(Term::Branch(_))
                    ) {
                        break None;
                    }
                    carrier = *only;
                };
                let Some(value) = value else {
                    usable = false;
                    break;
                };
                operands.push(value);
                operands.push(Operand::IdRef(*predecessor));
                carriers.push(carrier);
            }
            if !usable {
                continue;
            }
            let (Some(&join), Some(&position)) = (index.get(&block), index.get(&loaded_in)) else {
                continue;
            };
            let Some(load) = self.out[position]
                .instructions
                .iter()
                .position(|instruction| {
                    instruction.result_id == Some(result) && instruction.class.opcode == Op::Load
                })
            else {
                continue;
            };
            let ty = self.out[position].instructions[load].result_type;
            self.out[position].instructions.remove(load);
            self.out[join]
                .instructions
                .insert(0, Instruction::new(Op::Phi, ty, Some(result), operands));
            for carrier in carriers {
                if let Some(&source) = index.get(&carrier) {
                    self.out[source].instructions.retain(|instruction| {
                        instruction.class.opcode != Op::Store
                            || instruction.operands.first() != Some(&Operand::IdRef(var))
                    });
                }
            }
            promoted.insert(var);
        }
        self.variables
            .retain(|variable| !variable.result_id.is_some_and(|id| promoted.contains(&id)));
    }

    fn emit_construct_tail(
        &mut self,
        id: usize,
        next: Option<&Shape>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), String> {
        if !self.dispatches(id) {
            return match next {
                Some(next) => self.emit(next, stack),
                None => Ok(()),
            };
        }
        let dispatch = self.merge_of(id)?;
        let destinations = self
            .facts
            .get(&id)
            .map(|facts| facts.destinations.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let Some(next) = next else {
            let mut block = Block::new();
            block.label = Some(Instruction::new(Op::Label, None, Some(dispatch), vec![]));
            match stack.last().map(|frame| frame.merge_label) {
                Some(outer) => block.instructions.push(Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(outer)],
                )),
                None => {
                    block
                        .instructions
                        .push(Instruction::new(Op::Unreachable, None, None, vec![]))
                }
            }
            self.out.push(block);
            return Ok(());
        };
        let after = self.tc.fresh();
        let mut cases: Vec<(u64, Word)> = Vec::with_capacity(destinations.len());
        for destination in destinations {
            let (label, _) = self.entry_label(next, destination)?;
            cases.push((self.flow_id_of(destination), label));
        }
        let variable = self.flow_variable();
        let loaded = self.tc.fresh();
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(dispatch), vec![]));
        block.instructions.push(Instruction::new(
            Op::Load,
            Some(self.flow_ty),
            Some(loaded),
            vec![Operand::IdRef(variable)],
        ));
        let mut operands = vec![Operand::IdRef(loaded), Operand::IdRef(after)];
        for (flow, label) in &cases {
            operands.push(Operand::LiteralBit32(*flow as u32));
            operands.push(Operand::IdRef(*label));
        }
        block.instructions.push(Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(after),
                Operand::SelectionControl(SelectionControl::NONE),
            ],
        ));
        block
            .instructions
            .push(Instruction::new(Op::Switch, None, None, operands));
        self.out.push(block);

        let restore = stack.last().map(|frame| frame.merge_label);
        if let Some(frame) = stack.last_mut() {
            frame.merge_label = after;
        }
        let result = self.emit(next, stack);
        if let (Some(frame), Some(restore)) = (stack.last_mut(), restore) {
            frame.merge_label = restore;
        }
        result?;

        let mut tail = Block::new();
        tail.label = Some(Instruction::new(Op::Label, None, Some(after), vec![]));
        match restore {
            Some(outer) => tail.instructions.push(Instruction::new(
                Op::Branch,
                None,
                None,
                vec![Operand::IdRef(outer)],
            )),
            None => tail
                .instructions
                .push(Instruction::new(Op::Unreachable, None, None, vec![])),
        }
        self.out.push(tail);
        Ok(())
    }
}
