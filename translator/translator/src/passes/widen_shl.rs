use crate::fxhash::HashMap;
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Op, Word};

pub(crate) fn widen_masked_shifts(module: &mut Module) -> usize {
    let ints: HashMap<Word, (u32, u32)> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::TypeInt)
        .filter_map(|i| match i.operands.as_slice() {
            [Operand::LiteralBit32(w), Operand::LiteralBit32(s)] => Some((i.result_id?, (*w, *s))),
            _ => None,
        })
        .collect();
    let constants: HashMap<Word, (Word, u32)> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Constant)
        .filter_map(|i| match i.operands.as_slice() {
            [Operand::LiteralBit32(v)] => Some((i.result_id?, (i.result_type?, *v))),
            _ => None,
        })
        .collect();
    let mut sites = Vec::new();
    for (fi, function) in module.functions.iter().enumerate() {
        let defs: HashMap<Word, &Instruction> = function
            .blocks
            .iter()
            .flat_map(|b| b.instructions.iter())
            .filter_map(|i| Some((i.result_id?, i)))
            .collect();
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, widen) in block.instructions.iter().enumerate() {
                let (Op::UConvert, Some(wide), [Operand::IdRef(a)]) = (
                    widen.class.opcode,
                    widen.result_type,
                    widen.operands.as_slice(),
                ) else {
                    continue;
                };
                let Some(and) = defs.get(a).filter(|d| d.class.opcode == Op::BitwiseAnd) else {
                    continue;
                };
                let Some(narrow) = and.result_type else {
                    continue;
                };
                if ints.get(&wide).map(|t| t.0) != Some(32) || ints.get(&narrow) != Some(&(16, 0)) {
                    continue;
                }
                let [Operand::IdRef(p), Operand::IdRef(q)] = and.operands.as_slice() else {
                    continue;
                };
                for (shl, mask) in [(p, q), (q, p)] {
                    let Some(&(_, m)) = constants.get(mask).filter(|c| c.0 == narrow) else {
                        continue;
                    };
                    let Some(shift) = defs.get(shl).filter(|d| {
                        d.class.opcode == Op::ShiftLeftLogical && d.result_type == Some(narrow)
                    }) else {
                        continue;
                    };
                    let [Operand::IdRef(x), Operand::IdRef(amount)] = shift.operands.as_slice()
                    else {
                        continue;
                    };
                    let Some(&(_, c)) = constants.get(amount).filter(|c| (1..16).contains(&c.1))
                    else {
                        continue;
                    };
                    sites.push((fi, bi, ii, *x, narrow, (m & 0xffff) >> c, wide, c));
                    break;
                }
            }
        }
    }
    for &(fi, bi, ii, x, narrow, mask, wide, c) in sites.iter().rev() {
        let mask = constant(module, narrow, mask);
        let c = constant(module, wide, c);
        let (t16, t32) = (module.fresh_id(), module.fresh_id());
        let r = &module.functions[fi].blocks[bi].instructions[ii];
        let replacement = vec![
            Instruction::new(
                Op::BitwiseAnd,
                Some(narrow),
                Some(t16),
                vec![Operand::IdRef(x), Operand::IdRef(mask)],
            ),
            Instruction::new(
                Op::UConvert,
                Some(wide),
                Some(t32),
                vec![Operand::IdRef(t16)],
            ),
            Instruction::new(
                Op::ShiftLeftLogical,
                Some(wide),
                r.result_id,
                vec![Operand::IdRef(t32), Operand::IdRef(c)],
            ),
        ];
        module.functions[fi].blocks[bi]
            .instructions
            .splice(ii..=ii, replacement);
    }
    sites.len()
}

fn constant(module: &mut Module, ty: Word, value: u32) -> Word {
    let found = module.types_global_values.iter().find(|i| {
        i.class.opcode == Op::Constant
            && i.result_type == Some(ty)
            && i.operands == [Operand::LiteralBit32(value)]
    });
    if let Some(id) = found.and_then(|i| i.result_id) {
        return id;
    }
    let id = module.fresh_id();
    module.types_global_values.push(Instruction::new(
        Op::Constant,
        Some(ty),
        Some(id),
        vec![Operand::LiteralBit32(value)],
    ));
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function};

    const UINT: Word = 1;
    const USHORT: Word = 2;
    const SHORT: Word = 3;
    const C3: Word = 4;
    const MASK: Word = 5;
    const X: Word = 10;
    const SHL: Word = 11;
    const AND: Word = 12;
    const R: Word = 13;

    fn int(id: Word, width: u32, signed: u32) -> Instruction {
        Instruction::new(
            Op::TypeInt,
            None,
            Some(id),
            vec![Operand::LiteralBit32(width), Operand::LiteralBit32(signed)],
        )
    }

    fn constant_of(ty: Word, id: Word, value: u32) -> Instruction {
        Instruction::new(
            Op::Constant,
            Some(ty),
            Some(id),
            vec![Operand::LiteralBit32(value)],
        )
    }

    fn ids(opcode: Op, ty: Word, id: Word, operands: &[Word]) -> Instruction {
        Instruction::new(
            opcode,
            Some(ty),
            Some(id),
            operands.iter().map(|o| Operand::IdRef(*o)).collect(),
        )
    }

    fn module_with(narrow: Word, c: u32, m: u32, mask_first: bool) -> Module {
        let mut module = Module::new();
        module.types_global_values = vec![
            int(UINT, 32, 0),
            int(USHORT, 16, 0),
            int(SHORT, 16, 1),
            constant_of(narrow, C3, c),
            constant_of(narrow, MASK, m),
        ];
        let and = if mask_first { [MASK, SHL] } else { [SHL, MASK] };
        module.functions.push(Function {
            def: Some(Instruction::new(Op::Function, Some(UINT), Some(20), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![Instruction::new(
                Op::FunctionParameter,
                Some(narrow),
                Some(X),
                vec![],
            )],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(21), vec![])),
                instructions: vec![
                    ids(Op::ShiftLeftLogical, narrow, SHL, &[X, C3]),
                    ids(Op::BitwiseAnd, narrow, AND, &and),
                    ids(Op::UConvert, UINT, R, &[AND]),
                    Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(R)]),
                ],
            }],
        });
        module.set_id_bound(30);
        module
    }

    fn eval(module: &Module, x: u32) -> u32 {
        let width = |ty: Word| -> u32 {
            module
                .types_global_values
                .iter()
                .find(|i| i.result_id == Some(ty))
                .and_then(|i| match i.operands.first() {
                    Some(Operand::LiteralBit32(w)) => Some(*w),
                    _ => None,
                })
                .unwrap()
        };
        let mut values: HashMap<Word, u32> = module
            .types_global_values
            .iter()
            .filter(|i| i.class.opcode == Op::Constant)
            .map(|i| match i.operands.as_slice() {
                [Operand::LiteralBit32(v)] => (i.result_id.unwrap(), *v),
                _ => unreachable!(),
            })
            .collect();
        values.insert(X, x & 0xffff);
        for i in &module.functions[0].blocks[0].instructions {
            let arg = |n: usize| match &i.operands[n] {
                Operand::IdRef(v) => values[v],
                _ => unreachable!(),
            };
            let v = match i.class.opcode {
                Op::UConvert => arg(0),
                Op::BitwiseAnd => arg(0) & arg(1),
                Op::ShiftLeftLogical => arg(0) << arg(1),
                Op::ReturnValue => return arg(0),
                other => panic!("eval: {other:?}"),
            };
            let bits = width(i.result_type.unwrap());
            values.insert(
                i.result_id.unwrap(),
                if bits == 32 { v } else { v & ((1 << bits) - 1) },
            );
        }
        unreachable!("no OpReturnValue")
    }

    fn body(module: &Module) -> Vec<(Op, Option<Word>)> {
        module.functions[0].blocks[0]
            .instructions
            .iter()
            .map(|i| (i.class.opcode, i.result_id))
            .collect()
    }

    #[test]
    fn a_masked_16_bit_shift_under_a_zero_extend_becomes_a_32_bit_shift() {
        let mut module = module_with(USHORT, 3, 0xfff8, false);
        assert_eq!(widen_masked_shifts(&mut module), 1);
        let b = body(&module);
        assert_eq!(b[2].0, Op::BitwiseAnd);
        assert_eq!(b[3].0, Op::UConvert);
        assert_eq!(b[4], (Op::ShiftLeftLogical, Some(R)));
        let shl = &module.functions[0].blocks[0].instructions[4];
        assert_eq!(
            shl.result_type,
            Some(UINT),
            "the shift happens AFTER the widening"
        );
    }

    #[test]
    fn every_16_bit_input_widens_bit_for_bit() {
        for c in 1..16u32 {
            for (k, m) in [0xffff, 0xfff8, 0x00f0, 0x8001, 0x5a5a, 0x0001]
                .into_iter()
                .enumerate()
            {
                let original = module_with(USHORT, c, m, k % 2 == 1);
                let mut widened = original.clone();
                assert_eq!(widen_masked_shifts(&mut widened), 1, "c {c} m {m:#x}");
                for x in 0..=0xffffu32 {
                    let (want, got) = (eval(&original, x), eval(&widened, x));
                    assert_eq!(want, got, "c {c} m {m:#x} x {x:#x}: {want:#x} vs {got:#x}");
                }
            }
        }
    }

    #[test]
    fn a_signed_short_is_left_alone() {
        let mut module = module_with(SHORT, 3, 0xfff8, false);
        assert_eq!(widen_masked_shifts(&mut module), 0);
    }

    #[test]
    fn a_shift_of_zero_or_sixteen_is_left_alone() {
        for c in [0, 16] {
            let mut module = module_with(USHORT, c, 0xfff8, false);
            assert_eq!(widen_masked_shifts(&mut module), 0, "c {c}");
        }
    }

    #[test]
    fn a_zero_extend_of_anything_else_is_left_alone() {
        let mut module = module_with(USHORT, 3, 0xfff8, false);
        module.functions[0].blocks[0].instructions[2] = ids(Op::UConvert, UINT, R, &[SHL]);
        assert_eq!(widen_masked_shifts(&mut module), 0);
    }

    #[test]
    fn existing_constants_are_reused_not_duplicated() {
        let mut module = module_with(USHORT, 3, 0xfff8, false);
        module.types_global_values.push(constant_of(UINT, 7, 3));
        module
            .types_global_values
            .push(constant_of(USHORT, 8, 0xfff8 >> 3));
        let before = module.types_global_values.len();
        assert_eq!(widen_masked_shifts(&mut module), 1);
        assert_eq!(module.types_global_values.len(), before);
        let i = &module.functions[0].blocks[0].instructions;
        assert_eq!(i[2].operands[1], Operand::IdRef(8));
        assert_eq!(i[4].operands[1], Operand::IdRef(7));
    }
}
