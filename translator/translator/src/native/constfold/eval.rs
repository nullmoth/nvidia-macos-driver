use super::*;
use crate::fxhash::HashMap;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

pub(in crate::native) fn forward_eval(
    f: &crate::spirv_module::Function,
    consts: &HashMap<Word, i128>,
    global_consts: &HashMap<Word, i128>,
    widths: &HashMap<Word, u32>,
    composites: &HashMap<Word, Vec<i128>>,
    vec_globals: &HashMap<Word, Vec<i128>>,
) -> HashMap<Word, i128> {
    let mut lat: HashMap<Word, Lat> = consts.iter().map(|(k, v)| (*k, Lat::Const(*v))).collect();
    let mut comp: HashMap<Word, Vec<i128>> = composites.clone();
    let get = |lat: &HashMap<Word, Lat>, op: Option<&Operand>| -> Option<Lat> {
        match op {
            Some(Operand::IdRef(id)) => lat.get(id).copied(),
            _ => None,
        }
    };
    let binop = |lat: &HashMap<Word, Lat>,
                 a: Option<&Operand>,
                 b: Option<&Operand>,
                 f: &dyn Fn(i128, i128) -> i128|
     -> Option<Lat> {
        match (get(lat, a), get(lat, b)) {
            (Some(Lat::Bottom), _) | (_, Some(Lat::Bottom)) => Some(Lat::Bottom),
            (Some(Lat::Const(x)), Some(Lat::Const(y))) => Some(Lat::Const(f(x, y))),
            _ => None,
        }
    };
    let mut changed = true;
    let mut guard = 0;
    while changed && guard < 256 {
        changed = false;
        guard += 1;
        for blk in &f.blocks {
            for inst in &blk.instructions {
                let Some(rid) = inst.result_id else { continue };
                if let std::collections::hash_map::Entry::Vacant(e) = comp.entry(rid) {
                    let cv: Option<Vec<i128>> = match inst.class.opcode {
                        Op::Load | Op::CopyObject => match inst.operands.first() {
                            Some(Operand::IdRef(p)) => vec_globals.get(p).cloned(),
                            _ => None,
                        },
                        Op::CompositeConstruct => inst
                            .operands
                            .iter()
                            .map(|op| match op {
                                Operand::IdRef(c) => consts.get(c).copied(),
                                _ => None,
                            })
                            .collect(),
                        _ => None,
                    };
                    if let Some(cv) = cv {
                        e.insert(cv);
                        changed = true;
                    }
                }
                let new: Option<Lat> = match inst.class.opcode {
                    Op::CompositeExtract => match (inst.operands.first(), inst.operands.get(1)) {
                        (Some(Operand::IdRef(src)), Some(Operand::LiteralBit32(idx))) => {
                            match comp.get(src) {
                                Some(v) => v.get(*idx as usize).map(|c| Lat::Const(*c)),
                                None => Some(Lat::Bottom),
                            }
                        }
                        _ => Some(Lat::Bottom),
                    },
                    Op::Phi => {
                        let mut acc: Option<Lat> = None;
                        let mut i = 0;
                        while i < inst.operands.len() {
                            if let Some(Operand::IdRef(v)) = inst.operands.get(i) {
                                let Some(value) = lat.get(v).copied() else {
                                    acc = None;
                                    break;
                                };
                                acc = meet(acc, Some(value));
                            }
                            i += 2;
                        }
                        acc
                    }
                    Op::Load => match inst.operands.first() {
                        Some(Operand::IdRef(p)) => global_consts.get(p).copied().map(Lat::Const),
                        _ => Some(Lat::Bottom),
                    },
                    Op::CopyObject => get(&lat, inst.operands.first()),
                    Op::UConvert | Op::SConvert | Op::Bitcast => {
                        match get(&lat, inst.operands.first()) {
                            Some(Lat::Const(0)) => Some(Lat::Const(0)),
                            Some(Lat::Bottom) => Some(Lat::Bottom),
                            _ => None,
                        }
                    }
                    Op::IEqual => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| (a == b) as i128,
                    ),
                    Op::INotEqual => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| (a != b) as i128,
                    ),
                    Op::ULessThan => ucmp_fold(
                        get(&lat, inst.operands.first()),
                        get(&lat, inst.operands.get(1)),
                        Cmp::Lt,
                    ),
                    Op::UGreaterThan => ucmp_fold(
                        get(&lat, inst.operands.first()),
                        get(&lat, inst.operands.get(1)),
                        Cmp::Gt,
                    ),
                    Op::ULessThanEqual => ucmp_fold(
                        get(&lat, inst.operands.first()),
                        get(&lat, inst.operands.get(1)),
                        Cmp::Le,
                    ),
                    Op::UGreaterThanEqual => ucmp_fold(
                        get(&lat, inst.operands.first()),
                        get(&lat, inst.operands.get(1)),
                        Cmp::Ge,
                    ),
                    Op::SLessThan
                    | Op::SGreaterThan
                    | Op::SLessThanEqual
                    | Op::SGreaterThanEqual => {
                        let operand_width =
                            inst.operands
                                .iter()
                                .take(2)
                                .find_map(|operand| match operand {
                                    Operand::IdRef(id) => widths.get(id).copied(),
                                    _ => None,
                                });
                        scmp_fold(
                            get(&lat, inst.operands.first()),
                            get(&lat, inst.operands.get(1)),
                            operand_width,
                            match inst.class.opcode {
                                Op::SLessThan => Cmp::Lt,
                                Op::SGreaterThan => Cmp::Gt,
                                Op::SLessThanEqual => Cmp::Le,
                                _ => Cmp::Ge,
                            },
                        )
                    }
                    Op::LogicalNot => match get(&lat, inst.operands.first()) {
                        Some(Lat::Const(a)) => Some(Lat::Const((a == 0) as i128)),
                        Some(Lat::Bottom) => Some(Lat::Bottom),
                        None => None,
                    },
                    Op::LogicalAnd => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| ((a != 0) && (b != 0)) as i128,
                    ),
                    Op::LogicalOr => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| ((a != 0) || (b != 0)) as i128,
                    ),
                    Op::BitwiseAnd => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| a & b,
                    ),
                    Op::BitwiseOr => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| a | b,
                    ),
                    Op::BitwiseXor => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| a ^ b,
                    ),
                    Op::ShiftRightLogical => binop(
                        &lat,
                        inst.operands.first(),
                        inst.operands.get(1),
                        &|a, b| {
                            if (0..128).contains(&b) {
                                ((a as u128) >> b) as i128
                            } else {
                                0
                            }
                        },
                    ),
                    Op::IAdd | Op::IMul | Op::ISub => match widths.get(&rid) {
                        Some(&w) => {
                            let mask = if w >= 128 {
                                u128::MAX
                            } else {
                                (1u128 << w) - 1
                            };
                            let op = |f: &dyn Fn(u128, u128) -> u128| -> Option<Lat> {
                                match (
                                    get(&lat, inst.operands.first()),
                                    get(&lat, inst.operands.get(1)),
                                ) {
                                    (Some(Lat::Bottom), _) | (_, Some(Lat::Bottom)) => {
                                        Some(Lat::Bottom)
                                    }
                                    (Some(Lat::Const(x)), Some(Lat::Const(y))) => {
                                        Some(Lat::Const((f(x as u128, y as u128) & mask) as i128))
                                    }
                                    _ => None,
                                }
                            };
                            match inst.class.opcode {
                                Op::IAdd => op(&|a, b| a.wrapping_add(b)),
                                Op::IMul => op(&|a, b| a.wrapping_mul(b)),
                                _ => op(&|a, b| a.wrapping_sub(b)),
                            }
                        }
                        None => None,
                    },
                    Op::Select => match get(&lat, inst.operands.first()) {
                        Some(Lat::Const(c)) => {
                            let arm = if c != 0 {
                                inst.operands.get(1)
                            } else {
                                inst.operands.get(2)
                            };
                            get(&lat, arm)
                        }
                        Some(Lat::Bottom) => Some(Lat::Bottom),
                        None => None,
                    },
                    _ => Some(Lat::Bottom),
                };
                if let Some(n) = new {
                    let cur = lat.get(&rid).copied();
                    let merged = match (cur, n) {
                        (None, n) => Some(n),
                        (Some(Lat::Bottom), _) => Some(Lat::Bottom),
                        (Some(Lat::Const(_)), Lat::Bottom) => Some(Lat::Bottom),
                        (Some(Lat::Const(a)), Lat::Const(b)) if a == b => Some(Lat::Const(a)),
                        (Some(Lat::Const(_)), Lat::Const(_)) => Some(Lat::Bottom),
                    };
                    if merged != cur {
                        lat.insert(rid, merged.unwrap());
                        changed = true;
                    }
                }
            }
        }
    }
    lat.into_iter()
        .filter_map(|(k, v)| match v {
            Lat::Const(c) => Some((k, c)),
            Lat::Bottom => None,
        })
        .collect()
}
