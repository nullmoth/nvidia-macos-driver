use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Instruction;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

pub(in crate::native) fn compute_nonzero(
    f: &crate::spirv_module::Function,
    consts: &HashMap<Word, i128>,
    numworkgroups: &HashSet<Word>,
) -> HashSet<Word> {
    let mut nz: HashSet<Word> = consts
        .iter()
        .filter(|(_, v)| **v != 0)
        .map(|(k, _)| *k)
        .collect();
    let is_nz = |nz: &HashSet<Word>, op: Option<&Operand>| -> bool {
        matches!(op, Some(Operand::IdRef(id)) if nz.contains(id))
    };
    loop {
        let mut changed = false;
        for b in &f.blocks {
            for inst in &b.instructions {
                let Some(rid) = inst.result_id else { continue };
                if nz.contains(&rid) {
                    continue;
                }
                let seed = match inst.class.opcode {
                    Op::Load => is_nz_load(inst, numworkgroups),
                    Op::CompositeExtract | Op::VectorExtractDynamic => {
                        is_nz(&nz, inst.operands.first())
                    }
                    Op::UConvert | Op::SConvert | Op::CopyObject | Op::Bitcast => {
                        is_nz(&nz, inst.operands.first())
                    }
                    Op::IMul => {
                        is_nz(&nz, inst.operands.first()) && is_nz(&nz, inst.operands.get(1))
                    }
                    _ => false,
                };
                if seed {
                    nz.insert(rid);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    nz
}

pub(in crate::native) fn is_nz_load(inst: &Instruction, numworkgroups: &HashSet<Word>) -> bool {
    matches!(inst.operands.first(), Some(Operand::IdRef(p)) if numworkgroups.contains(p))
}

pub(in crate::native) fn affine(
    id: Word,
    def: &HashMap<Word, &Instruction>,
    consts: &HashMap<Word, i128>,
    widths: &HashMap<Word, u32>,
) -> (Word, u128) {
    let mask = |w: u32| -> u128 {
        if w >= 128 {
            u128::MAX
        } else {
            (1u128 << w) - 1
        }
    };
    let mut base = id;
    let mut off: u128 = 0;
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        let Some(inst) = def.get(&base) else { break };
        let w = match inst.result_id.and_then(|r| widths.get(&r)) {
            Some(w) => *w,
            None => break,
        };
        let m = mask(w);
        let a = inst.operands.first();
        let b = inst.operands.get(1);
        let cst = |o: Option<&Operand>| -> Option<u128> {
            match o {
                Some(Operand::IdRef(c)) => consts.get(c).map(|v| (*v as u128) & m),
                _ => None,
            }
        };
        match inst.class.opcode {
            Op::IAdd => {
                if let (Some(Operand::IdRef(x)), Some(c)) = (a, cst(b)) {
                    off = off.wrapping_add(c) & m;
                    base = *x;
                } else if let (Some(c), Some(Operand::IdRef(x))) = (cst(a), b) {
                    off = off.wrapping_add(c) & m;
                    base = *x;
                } else {
                    break;
                }
            }
            Op::ISub => {
                if let (Some(Operand::IdRef(x)), Some(c)) = (a, cst(b)) {
                    off = off.wrapping_sub(c) & m;
                    base = *x;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    (base, off)
}

pub(in crate::native) fn nonzero_self_minus_one_guards(
    f: &crate::spirv_module::Function,
    consts: &HashMap<Word, i128>,
    widths: &HashMap<Word, u32>,
    numworkgroups: &HashSet<Word>,
) -> HashMap<Word, i128> {
    if numworkgroups.is_empty() {
        return HashMap::default();
    }
    let nz = compute_nonzero(f, consts, numworkgroups);
    let mut def: HashMap<Word, &Instruction> = HashMap::default();
    for b in &f.blocks {
        for inst in &b.instructions {
            if let Some(r) = inst.result_id {
                def.insert(r, inst);
            }
        }
    }
    let mut out = HashMap::default();
    for b in &f.blocks {
        for inst in &b.instructions {
            if inst.class.opcode != Op::UGreaterThan {
                continue;
            }
            let Some(rid) = inst.result_id else { continue };
            let (Some(Operand::IdRef(x)), Some(Operand::IdRef(y))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let (bx, ox) = affine(*x, &def, consts, widths);
            let (by, oy) = affine(*y, &def, consts, widths);
            let w = widths.get(x).copied();
            let Some(w) = w else { continue };
            let mask = if w >= 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            };
            if bx == by && ox == 0 && oy == mask && nz.contains(&bx) {
                out.insert(rid, 1);
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
pub(in crate::native) enum Cmp {
    Lt,
    Gt,
    Le,
    Ge,
}

pub(in crate::native) fn ucmp_fold(x: Option<Lat>, y: Option<Lat>, kind: Cmp) -> Option<Lat> {
    let is_zero = |v: Option<Lat>| matches!(v, Some(Lat::Const(0)));
    match kind {
        Cmp::Lt if is_zero(y) => return Some(Lat::Const(0)),
        Cmp::Gt if is_zero(x) => return Some(Lat::Const(0)),
        Cmp::Le if is_zero(x) => return Some(Lat::Const(1)),
        Cmp::Ge if is_zero(y) => return Some(Lat::Const(1)),
        _ => {}
    }
    match (x, y) {
        (Some(Lat::Bottom), _) | (_, Some(Lat::Bottom)) => Some(Lat::Bottom),
        (Some(Lat::Const(a)), Some(Lat::Const(b))) => {
            let (a, b) = (a as u128, b as u128);
            let r = match kind {
                Cmp::Lt => a < b,
                Cmp::Gt => a > b,
                Cmp::Le => a <= b,
                Cmp::Ge => a >= b,
            };
            Some(Lat::Const(r as i128))
        }
        _ => None,
    }
}

pub(in crate::native) fn scmp_fold(
    x: Option<Lat>,
    y: Option<Lat>,
    width: Option<u32>,
    kind: Cmp,
) -> Option<Lat> {
    match (x, y) {
        (Some(Lat::Bottom), _) | (_, Some(Lat::Bottom)) => Some(Lat::Bottom),
        (Some(Lat::Const(a)), Some(Lat::Const(b))) => {
            let width = width.filter(|w| (1..=128).contains(w))?;
            let shift = 128 - width;
            let sign_extend = |v: i128| (v << shift) >> shift;
            let (a, b) = (sign_extend(a), sign_extend(b));
            let result = match kind {
                Cmp::Lt => a < b,
                Cmp::Gt => a > b,
                Cmp::Le => a <= b,
                Cmp::Ge => a >= b,
            };
            Some(Lat::Const(result as i128))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::native) enum Lat {
    Const(i128),
    Bottom,
}

pub(in crate::native) fn meet(a: Option<Lat>, b: Option<Lat>) -> Option<Lat> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(Lat::Bottom), _) | (_, Some(Lat::Bottom)) => Some(Lat::Bottom),
        (Some(Lat::Const(x)), Some(Lat::Const(y))) => {
            Some(if x == y { Lat::Const(x) } else { Lat::Bottom })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_comparison_reads_the_operands_at_their_own_width() {
        let c = |v: i128| Some(Lat::Const(v));
        assert_eq!(
            scmp_fold(c(0), c(65534), Some(32), Cmp::Lt),
            Some(Lat::Const(1)),
            "at 32 bits 65534 is 65534"
        );
        assert_eq!(
            scmp_fold(c(0), c(65534), Some(16), Cmp::Lt),
            Some(Lat::Const(0)),
            "at 16 bits 65534 is -2"
        );
        assert_eq!(
            scmp_fold(c(0), c(65534), Some(16), Cmp::Gt),
            Some(Lat::Const(1)),
            "and 0 > -2"
        );
        assert_eq!(
            scmp_fold(c(0xFFFF_FFFF), c(0), Some(32), Cmp::Lt),
            Some(Lat::Const(1))
        );
        assert_eq!(
            scmp_fold(c(0xFFFF_FFFF), c(0), Some(64), Cmp::Lt),
            Some(Lat::Const(0))
        );
        assert_eq!(
            scmp_fold(c(0x8000_0000), c(0x7FFF_FFFF), Some(32), Cmp::Lt),
            Some(Lat::Const(1))
        );
        assert_eq!(
            scmp_fold(c(7), c(7), Some(32), Cmp::Le),
            Some(Lat::Const(1))
        );
        assert_eq!(
            scmp_fold(c(7), c(7), Some(32), Cmp::Ge),
            Some(Lat::Const(1))
        );
    }

    #[test]
    fn signed_comparison_refuses_what_it_cannot_prove() {
        let c = |v: i128| Some(Lat::Const(v));
        assert_eq!(
            scmp_fold(None, c(0), Some(32), Cmp::Ge),
            None,
            "x >= 0 is not a signed tautology"
        );
        assert_eq!(scmp_fold(c(0), None, Some(32), Cmp::Lt), None);
        assert_eq!(
            scmp_fold(c(0), c(1), None, Cmp::Lt),
            None,
            "an unknown width refuses"
        );
        assert_eq!(scmp_fold(c(0), c(1), Some(0), Cmp::Lt), None);
        assert_eq!(scmp_fold(c(0), c(1), Some(129), Cmp::Lt), None);
        assert_eq!(
            scmp_fold(Some(Lat::Bottom), c(1), Some(32), Cmp::Lt),
            Some(Lat::Bottom),
            "a poisoned operand poisons the result"
        );
        assert_eq!(ucmp_fold(None, c(0), Cmp::Ge), Some(Lat::Const(1)));
    }
}
