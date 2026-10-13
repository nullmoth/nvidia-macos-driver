use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::native::ir::{LlGep, LlType, LlValue};
use crate::native::parse::{
    parse_call, parse_phi, parse_switch, parse_typed_value, strip_call_prefix, strip_comment,
    LlSwitch,
};

pub(in crate::native) fn ret_emit(terminator_text: &str) -> RetEmit {
    let cleaned = strip_comment(terminator_text).trim();
    let Some(rest) = cleaned.strip_prefix("ret ") else {
        return RetEmit::FromText;
    };
    if rest.trim() == "void" {
        return RetEmit::Void;
    }
    match parse_typed_value(rest) {
        Ok(tv) => RetEmit::Value(tv),
        Err(_) => RetEmit::FromText,
    }
}

pub(in crate::native) fn switch_emit(terminator_text: &str) -> Option<LlSwitch> {
    let cleaned = strip_comment(terminator_text).trim();
    if !cleaned.starts_with("switch ") {
        return None;
    }
    parse_switch(cleaned).ok()
}

pub(in crate::native) fn phi_incoming_parse(
    line: &str,
) -> (Option<(LlType, Vec<(LlValue, String)>)>, Option<String>) {
    let Some(rhs) = strip_comment(line)
        .trim()
        .split_once(" = ")
        .map(|s| s.1.trim())
    else {
        return (None, None);
    };
    let opcode = rhs.split_whitespace().next().unwrap_or("");
    let rest = rhs[opcode.len()..].trim_start();
    match parse_phi(rest) {
        Ok(parsed) => (Some(parsed), None),
        Err(error) => (None, Some(error)),
    }
}

pub(in crate::native) fn collect_forward_geps<B: AsRef<TirBlock>>(
    blocks: &[B],
) -> HashMap<String, LlGep> {
    let mut geps = HashMap::default();
    for block in blocks {
        let block = block.as_ref();
        for inst in &block.insts {
            if let (Some(name), Some(gep)) = (&inst.result, &inst.gep()) {
                geps.insert(name.clone(), (**gep).clone());
            }
        }
    }
    geps
}

pub(in crate::native) fn collect_pointer_phi_sets<B: AsRef<TirBlock>>(
    blocks: &[B],
) -> (HashSet<String>, HashSet<String>) {
    let mut results = HashSet::default();
    let mut incoming = HashSet::default();
    for block in blocks {
        let block = block.as_ref();
        for inst in &block.insts {
            if inst.opcode != "phi" || !matches!(inst.result_ty, Some(LlType::Ptr(_))) {
                continue;
            }
            let Some(name) = &inst.result else { continue };
            results.insert(name.clone());
            if let Some(values) = inst.phi_values() {
                for value in values {
                    if let LlValue::Local(name) = value {
                        incoming.insert(name.clone());
                    }
                }
            } else {
                for operand in &inst.operands {
                    if let TirOperand::Value { name, .. } = operand {
                        incoming.insert(name.clone());
                    }
                }
            }
        }
    }
    (results, incoming)
}

#[cfg(test)]
pub(in crate::native) fn build(
    body: &[String],
    entry_label: &str,
    named_types: &HashMap<String, LlType>,
) -> Result<TirFunction, String> {
    let mut blocks: Vec<TirBlock> = Vec::new();
    let mut value_types: HashMap<String, LlType> = HashMap::default();
    let mut pointer_pointees: HashMap<String, LlType> = HashMap::default();

    let mut cur_label = entry_label.to_string();
    let mut cur_insts: Vec<TirInst> = Vec::new();
    let mut cur_term: Option<TirTerminator> = None;
    let mut cur_term_text = String::new();

    for raw in body {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(label) = parse_block_label(line) {
            if cur_term.is_some() || !cur_insts.is_empty() {
                blocks.push(finish_flat_block(
                    cur_label,
                    cur_insts,
                    cur_term,
                    cur_term_text,
                )?);
                cur_insts = Vec::new();
                cur_term = None;
                cur_term_text = String::new();
            }
            cur_label = label;
            continue;
        }
        if let Some(term) = parse_terminator(line) {
            cur_term = Some(term);
            cur_term_text = line.to_string();
            continue;
        }
        push_inst_line(
            line,
            named_types,
            &mut value_types,
            &mut pointer_pointees,
            &mut cur_insts,
        );
    }
    if !cur_insts.is_empty() || cur_term.is_some() || !blocks.is_empty() {
        blocks.push(finish_flat_block(
            cur_label,
            cur_insts,
            cur_term,
            cur_term_text,
        )?);
    }

    let (use_pointees, _, byte_view_pointers) = infer_use_pointees(&blocks);
    let (pointer_phi_results, pointer_phi_incoming) = collect_pointer_phi_sets(&blocks);
    let forward_geps = collect_forward_geps(&blocks);
    Ok(TirFunction {
        blocks: blocks.into_iter().map(std::sync::Arc::new).collect(),
        value_types,
        pointer_pointees,
        use_pointees,
        byte_view_pointers,
        pointer_phi_results,
        pointer_phi_incoming,
        forward_geps,
    })
}

#[cfg(test)]
fn finish_flat_block(
    label: String,
    insts: Vec<TirInst>,
    term: Option<TirTerminator>,
    terminator_text: String,
) -> Result<TirBlock, String> {
    let terminator = term.ok_or_else(|| format!("native tir: block {label} has no terminator"))?;
    Ok(TirBlock {
        label,
        insts,
        terminator,
        ret: ret_emit(&terminator_text),
        switch: switch_emit(&terminator_text),
    })
}

pub(in crate::native) fn build_from_blocks(
    blocks: &[crate::native::cfg::BodyBlock],
) -> Result<TirFunction, String> {
    let mut tir_blocks: Vec<std::sync::Arc<TirBlock>> = Vec::new();
    let mut value_types: HashMap<String, LlType> = HashMap::default();
    let mut pointer_pointees: HashMap<String, LlType> = HashMap::default();

    for bb in blocks {
        let Some(carrier) = &bb.typed else {
            return Err(format!(
                "native tir: block {} role={:?} has no typed carrier (unpopulated synthesis site)",
                bb.name, bb.role
            ));
        };
        for inst in &carrier.insts {
            if let (Some(name), Some(ty)) = (&inst.result, &inst.result_ty) {
                value_types.insert(name.clone(), ty.clone());
            }
            if let (Some(name), Some(pointee)) = (&inst.result, &inst.pointer_pointee()) {
                pointer_pointees.insert(name.clone(), pointee.clone());
            }
        }
        tir_blocks.push(std::sync::Arc::clone(carrier));
    }

    let (use_pointees, _, byte_view_pointers) = infer_use_pointees(&tir_blocks);
    let (pointer_phi_results, pointer_phi_incoming) = collect_pointer_phi_sets(&tir_blocks);
    let forward_geps = collect_forward_geps(&tir_blocks);
    Ok(TirFunction {
        blocks: tir_blocks,
        value_types,
        pointer_pointees,
        use_pointees,
        byte_view_pointers,
        pointer_phi_results,
        pointer_phi_incoming,
        forward_geps,
    })
}

pub(in crate::native) fn lower_block<S: AsRef<str>>(
    name: &str,
    lines: &[S],
    named_types: &HashMap<String, LlType>,
    value_types: &mut HashMap<String, LlType>,
    pointer_pointees: &mut HashMap<String, LlType>,
) -> Result<TirBlock, String> {
    let mut cur_insts: Vec<TirInst> = Vec::new();
    let mut cur_term: Option<TirTerminator> = None;
    let mut cur_term_text = String::new();
    for raw in lines {
        let line = raw.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(term) = parse_terminator(line) {
            cur_term = Some(term);
            cur_term_text = line.to_string();
            continue;
        }
        push_inst_line(
            line,
            named_types,
            value_types,
            pointer_pointees,
            &mut cur_insts,
        );
    }
    let terminator =
        cur_term.ok_or_else(|| format!("native tir: block {name} has no terminator"))?;
    Ok(TirBlock {
        label: name.to_string(),
        insts: cur_insts,
        terminator,
        ret: ret_emit(&cur_term_text),
        switch: switch_emit(&cur_term_text),
    })
}

pub(in crate::native) fn lower_block_carrier<S: AsRef<str>>(
    name: &str,
    lines: &[S],
    named_types: &HashMap<String, LlType>,
) -> Option<TirBlock> {
    let mut value_types = HashMap::default();
    let mut pointer_pointees = HashMap::default();
    lower_block(
        name,
        lines,
        named_types,
        &mut value_types,
        &mut pointer_pointees,
    )
    .ok()
}

pub(in crate::native) fn lower_block_carrier_with_prefix(
    name: &str,
    prefix: &TirBlock,
    tail_lines: &[String],
) -> Option<TirBlock> {
    let tail = lower_block_carrier(name, tail_lines, &HashMap::default())?;
    let mut insts = prefix.insts.clone();
    insts.extend(tail.insts);
    Some(TirBlock {
        label: name.to_string(),
        insts,
        terminator: tail.terminator,
        ret: tail.ret,
        switch: tail.switch,
    })
}

pub(in crate::native) fn lower_block_carrier_from_suffix(
    name: &str,
    source: &TirBlock,
    skip: usize,
    terminator_line: &str,
) -> Option<TirBlock> {
    let insts = source.insts.get(skip..)?.to_vec();
    let term = lower_block_carrier(
        name,
        std::slice::from_ref(&terminator_line.to_string()),
        &HashMap::default(),
    )?;
    Some(TirBlock {
        label: name.to_string(),
        insts,
        terminator: term.terminator,
        ret: term.ret,
        switch: term.switch,
    })
}

pub(in crate::native) fn lower_block_carrier_prefix(
    name: &str,
    source: &TirBlock,
    keep: usize,
    terminator_line: &str,
) -> Option<TirBlock> {
    let insts = source.insts.get(..keep)?.to_vec();
    let term = lower_block_carrier(
        name,
        std::slice::from_ref(&terminator_line.to_string()),
        &HashMap::default(),
    )?;
    Some(TirBlock {
        label: name.to_string(),
        insts,
        terminator: term.terminator,
        ret: term.ret,
        switch: term.switch,
    })
}

pub(in crate::native) fn push_inst_line(
    line: &str,
    named_types: &HashMap<String, LlType>,
    value_types: &mut HashMap<String, LlType>,
    pointer_pointees: &mut HashMap<String, LlType>,
    cur_insts: &mut Vec<TirInst>,
) {
    let mut pointer_pointee: Option<LlType> = None;
    let (result, result_ty) = match resolve_result(line, named_types) {
        Some((name, ty)) => {
            if let Some(ty) = &ty {
                value_types.insert(name.clone(), ty.clone());
            }
            if matches!(ty, Some(LlType::Ptr(_))) {
                if let Some(pointee) = resolve_gep_pointee(rhs_of(line), named_types) {
                    pointer_pointees.insert(name.clone(), pointee.clone());
                    pointer_pointee = Some(pointee);
                }
            }
            (Some(name), ty)
        }
        None => (result_name(line), None),
    };
    let mut operands = resolve_operands(line);
    let cmp_predicate = resolve_cmp_predicate(line);
    let mem_align = resolve_mem_align(line);
    let gep = resolve_gep(line).map(Box::new);
    let call = resolve_call(line).map(Box::new);
    let opcode = rhs_of(line)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    let fast_math = rhs_of(line).split_whitespace().nth(1) == Some("fast");
    let float_math_mode = granted_float_relaxations(rhs_of(line));
    let alloca_ty = if opcode == "alloca" {
        resolve_alloca_ty(line)
    } else {
        None
    };
    let (phi_incoming, phi_parse_error) = if opcode == "phi" {
        phi_incoming_parse(line)
    } else {
        (None, None)
    };
    if phi_incoming.is_some() {
        operands.clear();
    }
    let uses = (phi_incoming.is_none()
        && operands
            .iter()
            .any(|operand| matches!(operand, TirOperand::Unresolved)))
    .then(|| instruction_uses(line, result.as_deref()));
    let aggregate_indices = resolve_aggregate_indices(line, &opcode);
    let diag_line = if matches!(
        opcode.as_str(),
        "extractelement" | "insertelement" | "shufflevector"
    ) {
        Some(crate::native::lex::strip_comment(line).trim().to_string())
    } else {
        None
    };
    let shuffle_mask = if opcode == "shufflevector" {
        resolve_shuffle_mask(line)
    } else {
        None
    };
    let bitcast = resolve_bitcast(line, &opcode).map(Box::new);
    let icmp_rest = resolve_icmp_rest(line, &opcode);
    let identity_ptr_bitcast = resolve_identity_ptr_bitcast(line);
    let phi_incoming_values = if opcode == "phi" && phi_incoming.is_none() {
        resolve_phi_incoming_values(line, &opcode)
    } else {
        None
    };
    let select_arms = resolve_select_arms(line, &opcode).map(Box::new);
    let load = resolve_load_inst(line, &opcode).map(Box::new);
    let store = resolve_store(line, &opcode).map(Box::new);
    let alias_call = resolve_alias_call(line).map(Box::new);
    let emit_scan_call = resolve_emit_scan_call(line);
    let void_call_line = if matches!(opcode.as_str(), "call" | "tail") && result.is_none() {
        Some(crate::native::lex::strip_comment(line).trim().to_string())
    } else {
        None
    };
    let value_call_error =
        if matches!(opcode.as_str(), "call" | "tail") && result.is_some() && call.is_none() {
            let cleaned = crate::native::lex::strip_comment(line).trim();
            let call_text = cleaned
                .split_once(" = ")
                .and_then(|(_, rhs)| strip_call_prefix(rhs.trim()));
            call_text.and_then(|text| parse_call(text).err())
        } else {
            None
        };
    let data = match opcode.as_str() {
        "icmp" | "fcmp" => TirInstData::Compare {
            predicate: cmp_predicate,
            rest: icmp_rest,
        },
        "load" | "store" => TirInstData::Memory {
            align: mem_align,
            load,
            store,
        },
        "getelementptr" => TirInstData::Gep {
            parsed: gep,
            pointee: pointer_pointee,
        },
        "call" | "tail" | "musttail" | "notail" => {
            let alias_from_parsed = alias_call.is_some() && call.is_some();
            let alias_override = (!alias_from_parsed).then_some(alias_call).flatten();
            let emit_scan = match emit_scan_call {
                Some(Ok(_)) if call.is_some() => EmitScanData::Parsed,
                Some(result) => EmitScanData::Owned(Box::new(result)),
                None => EmitScanData::None,
            };
            TirInstData::Call {
                parsed: call,
                void_line: void_call_line,
                value_error: value_call_error,
                alias_from_parsed,
                alias_override,
                emit_scan,
            }
        }
        "alloca" => TirInstData::Alloca(alloca_ty),
        "phi" => TirInstData::Phi {
            incoming: phi_incoming,
            incoming_values: phi_incoming_values,
            parse_error: phi_parse_error,
        },
        "extractvalue" | "insertvalue" => TirInstData::Aggregate(aggregate_indices),
        "extractelement" | "insertelement" | "shufflevector" => TirInstData::Element {
            diag_line,
            shuffle_mask,
        },
        "bitcast" => TirInstData::Bitcast {
            destination: bitcast.map(|parsed| parsed.1),
            identity: identity_ptr_bitcast.is_some(),
        },
        "select" => TirInstData::Select(select_arms),
        _ => TirInstData::Plain,
    };
    cur_insts.push(TirInst {
        result,
        result_ty,
        uses,
        operands,
        opcode: TirOpcode::new(opcode),
        data: Box::new(TirInstDetails {
            fast_math,
            float_math_mode,
            payload: data,
        }),
    });
}

fn granted_float_relaxations(rhs: &str) -> Option<FloatMathMode> {
    let mut tokens = rhs.split_whitespace();
    if !matches!(tokens.next(), Some("fmul" | "fadd" | "fsub" | "fdiv")) {
        return None;
    }
    let flags = tokens
        .take_while(|token| token.chars().all(|c| c.is_ascii_lowercase()))
        .collect::<Vec<_>>();
    if flags.contains(&"fast") {
        return None;
    }
    let granted = FloatMathMode::from_llvm_flags(&flags);
    (!granted.grants_every_rewrite()).then_some(granted)
}
