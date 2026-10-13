use crate::fxhash::HashMap;
use crate::spirv_module::{Instruction, Operand};
use spirv::{Op, Word};

#[derive(Clone, Copy)]
pub(super) enum ScalarType {
    Integer(u32),
    Float(u32),
}

#[derive(Default)]
pub(super) struct TypeTracker {
    types: HashMap<Word, ScalarType>,
}

impl TypeTracker {
    pub(super) fn track(&mut self, instruction: &Instruction) {
        let Some(id) = instruction.result_id else {
            return;
        };
        let scalar = match instruction.class.opcode {
            Op::TypeInt => match instruction.operands.as_slice() {
                [Operand::LiteralBit32(bits), Operand::LiteralBit32(_)] => {
                    Some(ScalarType::Integer(*bits))
                }
                _ => None,
            },
            Op::TypeFloat => match instruction.operands.as_slice() {
                [Operand::LiteralBit32(bits)] => Some(ScalarType::Float(*bits)),
                _ => None,
            },
            _ => instruction
                .result_type
                .and_then(|result_type| self.types.get(&result_type).copied()),
        };
        if let Some(scalar) = scalar {
            self.types.insert(id, scalar);
        }
    }

    pub(super) fn resolve(&self, id: Word) -> Option<ScalarType> {
        self.types.get(&id).copied()
    }
}
