use crate::fxhash::HashMap;
use crate::spirv_module::{Function, Instruction, Module, Operand};
use spirv::{Op, StorageClass, Word};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native) enum Value {
    Int(u32),
    Bool(bool),
    Pointer(Word),
    Undef,
}

impl Value {
    fn as_int(self) -> Result<u32, String> {
        match self {
            Value::Int(value) => Ok(value),
            Value::Bool(value) => Ok(u32::from(value)),
            other => Err(format!("expected an integer, got {other:?}")),
        }
    }

    fn as_bool(self) -> Result<bool, String> {
        match self {
            Value::Bool(value) => Ok(value),
            Value::Int(value) => Ok(value != 0),
            other => Err(format!("expected a boolean, got {other:?}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::native) enum Outcome {
    Returned(Value),
    ReturnedVoid,
    Terminated,
}

pub(in crate::native) fn run(
    module: &Module,
    function: &Function,
    arguments: &[u32],
    step_limit: usize,
) -> Result<Outcome, String> {
    Interpreter::new(module, function, arguments)?.run(step_limit)
}

pub(in crate::native) fn run_module(
    module: &Module,
    arguments: &[u32],
    step_limit: usize,
) -> Result<Outcome, String> {
    let function = module
        .functions
        .iter()
        .find(|function| !function.blocks.is_empty())
        .ok_or_else(|| "module has no function with a body".to_string())?;
    run(module, function, arguments, step_limit)
}

pub(in crate::native) fn run_module_to_global(
    module: &Module,
    slot: Word,
    step_limit: usize,
) -> Result<Value, String> {
    let function = module
        .functions
        .iter()
        .find(|function| !function.blocks.is_empty())
        .ok_or_else(|| "module has no function with a body".to_string())?;
    let mut interpreter = Interpreter::new(module, function, &[])?;
    match interpreter.run(step_limit)? {
        Outcome::ReturnedVoid => interpreter
            .memory
            .get(&slot)
            .copied()
            .ok_or_else(|| format!("%{slot} is not a variable of this module")),
        other => Err(format!("expected a void return, got {other:?}")),
    }
}

struct Interpreter<'a> {
    function: &'a Function,
    blocks: HashMap<Word, usize>,
    values: HashMap<Word, Value>,
    memory: HashMap<Word, Value>,
}

impl<'a> Interpreter<'a> {
    fn new(module: &Module, function: &'a Function, arguments: &[u32]) -> Result<Self, String> {
        let mut values = HashMap::default();
        let mut initial_memory = Vec::new();
        for instruction in &module.types_global_values {
            let Some(id) = instruction.result_id else {
                continue;
            };
            match instruction.class.opcode {
                Op::Constant => {
                    values.insert(id, Value::Int(literal(instruction)?));
                }
                Op::ConstantTrue => {
                    values.insert(id, Value::Bool(true));
                }
                Op::ConstantFalse => {
                    values.insert(id, Value::Bool(false));
                }
                Op::ConstantNull => {
                    values.insert(id, Value::Int(0));
                }
                Op::Undef => {
                    values.insert(id, Value::Undef);
                }
                Op::Variable => {
                    values.insert(id, Value::Pointer(id));
                    initial_memory.push((id, instruction.operands.get(1).cloned()));
                }
                _ => {}
            }
        }
        if function.parameters.len() != arguments.len() {
            return Err(format!(
                "function takes {} arguments, got {}",
                function.parameters.len(),
                arguments.len()
            ));
        }
        for (parameter, argument) in function.parameters.iter().zip(arguments) {
            let id = parameter
                .result_id
                .ok_or_else(|| "function parameter without a result id".to_string())?;
            values.insert(id, Value::Int(*argument));
        }
        let mut blocks = HashMap::default();
        for (index, block) in function.blocks.iter().enumerate() {
            let label = block
                .label
                .as_ref()
                .and_then(|label| label.result_id)
                .ok_or_else(|| "block without a label".to_string())?;
            if blocks.insert(label, index).is_some() {
                return Err(format!("label %{label} is defined twice"));
            }
        }
        let mut memory = HashMap::default();
        for (slot, initializer) in initial_memory {
            let value = match initializer {
                Some(operand) => match &operand {
                    Operand::IdRef(id) => *values
                        .get(id)
                        .ok_or_else(|| format!("%{slot} is initialized by unknown %{id}"))?,
                    other => return Err(format!("{other:?} is not a value initializer")),
                },
                None => Value::Undef,
            };
            memory.insert(slot, value);
        }
        Ok(Self {
            function,
            blocks,
            values,
            memory,
        })
    }

    fn run(&mut self, step_limit: usize) -> Result<Outcome, String> {
        let mut current = 0usize;
        let mut previous: Option<Word> = None;
        let mut steps = 0usize;
        loop {
            let block = self
                .function
                .blocks
                .get(current)
                .ok_or_else(|| "branched past the end of the function".to_string())?;
            let label = block
                .label
                .as_ref()
                .and_then(|label| label.result_id)
                .ok_or_else(|| "block without a label".to_string())?;

            let mut resolved = Vec::new();
            for instruction in &block.instructions {
                if instruction.class.opcode != Op::Phi {
                    break;
                }
                let from = previous.ok_or_else(|| {
                    format!("OpPhi in entry block %{label} has no predecessor to select")
                })?;
                let id = instruction
                    .result_id
                    .ok_or_else(|| "OpPhi without a result id".to_string())?;
                resolved.push((id, self.phi_incoming(instruction, from, label)?));
            }
            for (id, value) in resolved {
                self.values.insert(id, value);
            }

            for instruction in &block.instructions {
                steps += 1;
                if steps > step_limit {
                    return Err(format!(
                        "exceeded {step_limit} steps without returning (the function does not \
                         terminate on this input)"
                    ));
                }
                match instruction.class.opcode {
                    Op::Phi | Op::LoopMerge | Op::SelectionMerge | Op::Line | Op::NoLine => {}
                    Op::Branch => {
                        previous = Some(label);
                        current = self.target(instruction, 0)?;
                        break;
                    }
                    Op::BranchConditional => {
                        let condition = self.operand(instruction, 0)?.as_bool()?;
                        previous = Some(label);
                        current = self.target(instruction, if condition { 1 } else { 2 })?;
                        break;
                    }
                    Op::Switch => {
                        let selector = self.operand(instruction, 0)?.as_int()?;
                        previous = Some(label);
                        current = self.switch_target(instruction, selector)?;
                        break;
                    }
                    Op::Return => return Ok(Outcome::ReturnedVoid),
                    Op::ReturnValue => {
                        return Ok(Outcome::Returned(self.operand(instruction, 0)?));
                    }
                    Op::Unreachable | Op::Kill | Op::TerminateInvocation => {
                        return Ok(Outcome::Terminated)
                    }
                    _ => self.execute(instruction)?,
                }
            }
        }
    }

    fn phi_incoming(
        &self,
        instruction: &Instruction,
        from: Word,
        label: Word,
    ) -> Result<Value, String> {
        let mut pairs = instruction.operands.chunks_exact(2);
        let incoming = pairs.find_map(|pair| match (&pair[0], &pair[1]) {
            (value, Operand::IdRef(predecessor)) if *predecessor == from => Some(value),
            _ => None,
        });
        let incoming = incoming.ok_or_else(|| {
            format!("OpPhi in %{label} has no incoming value for predecessor %{from}")
        })?;
        self.value_of(incoming)
    }

    fn execute(&mut self, instruction: &Instruction) -> Result<(), String> {
        let opcode = instruction.class.opcode;
        let id = instruction.result_id;
        let value = match opcode {
            Op::Variable => {
                let id = id.ok_or_else(|| "OpVariable without a result id".to_string())?;
                match instruction.operands.first() {
                    Some(Operand::StorageClass(StorageClass::Function)) => {}
                    other => {
                        return Err(format!("unsupported OpVariable storage class {other:?}"));
                    }
                }
                let initial = match instruction.operands.get(1) {
                    Some(operand) => self.value_of(operand)?,
                    None => Value::Undef,
                };
                self.memory.insert(id, initial);
                Value::Pointer(id)
            }
            Op::Load => {
                let Value::Pointer(slot) = self.operand(instruction, 0)? else {
                    return Err("OpLoad from a non-variable pointer".to_string());
                };
                *self
                    .memory
                    .get(&slot)
                    .ok_or_else(|| format!("OpLoad from undeclared slot %{slot}"))?
            }
            Op::Store => {
                let Value::Pointer(slot) = self.operand(instruction, 0)? else {
                    return Err("OpStore through a non-variable pointer".to_string());
                };
                let value = self.operand(instruction, 1)?;
                self.memory.insert(slot, value);
                return Ok(());
            }
            Op::CopyObject => self.operand(instruction, 0)?,
            Op::Undef => Value::Undef,
            Op::Select => {
                let condition = self.operand(instruction, 0)?.as_bool()?;
                if condition {
                    self.operand(instruction, 1)?
                } else {
                    self.operand(instruction, 2)?
                }
            }
            Op::LogicalNot => Value::Bool(!self.operand(instruction, 0)?.as_bool()?),
            Op::LogicalAnd => Value::Bool(
                self.operand(instruction, 0)?.as_bool()?
                    && self.operand(instruction, 1)?.as_bool()?,
            ),
            Op::LogicalOr => Value::Bool(
                self.operand(instruction, 0)?.as_bool()?
                    || self.operand(instruction, 1)?.as_bool()?,
            ),
            Op::Not => Value::Int(!self.operand(instruction, 0)?.as_int()?),
            _ => self.binary(instruction)?,
        };
        if let Some(id) = id {
            self.values.insert(id, value);
        }
        Ok(())
    }

    fn binary(&self, instruction: &Instruction) -> Result<Value, String> {
        let opcode = instruction.class.opcode;
        let a = self.operand(instruction, 0)?;
        let b = self.operand(instruction, 1)?;
        if a == Value::Undef || b == Value::Undef {
            return Ok(Value::Undef);
        }
        let (a, b) = (a.as_int()?, b.as_int()?);
        let value = match opcode {
            Op::IAdd => Value::Int(a.wrapping_add(b)),
            Op::ISub => Value::Int(a.wrapping_sub(b)),
            Op::IMul => Value::Int(a.wrapping_mul(b)),
            Op::UDiv => match b {
                0 => Value::Undef,
                b => Value::Int(a / b),
            },
            Op::UMod => match b {
                0 => Value::Undef,
                b => Value::Int(a % b),
            },
            Op::BitwiseAnd => Value::Int(a & b),
            Op::BitwiseOr => Value::Int(a | b),
            Op::BitwiseXor => Value::Int(a ^ b),
            Op::ShiftLeftLogical => Value::Int(a.checked_shl(b).unwrap_or(0)),
            Op::ShiftRightLogical => Value::Int(a.checked_shr(b).unwrap_or(0)),
            Op::IEqual => Value::Bool(a == b),
            Op::INotEqual => Value::Bool(a != b),
            Op::ULessThan => Value::Bool(a < b),
            Op::ULessThanEqual => Value::Bool(a <= b),
            Op::UGreaterThan => Value::Bool(a > b),
            Op::UGreaterThanEqual => Value::Bool(a >= b),
            Op::SLessThan => Value::Bool((a as i32) < (b as i32)),
            Op::SGreaterThan => Value::Bool((a as i32) > (b as i32)),
            other => return Err(format!("{other:?} is outside the interpreted subset")),
        };
        Ok(value)
    }

    fn operand(&self, instruction: &Instruction, index: usize) -> Result<Value, String> {
        let operand = instruction
            .operands
            .get(index)
            .ok_or_else(|| format!("{:?} has no operand {index}", instruction.class.opcode))?;
        self.value_of(operand)
    }

    fn value_of(&self, operand: &Operand) -> Result<Value, String> {
        match operand {
            Operand::IdRef(id) => self
                .values
                .get(id)
                .copied()
                .ok_or_else(|| format!("%{id} is used before it is defined on this path")),
            other => Err(format!("{other:?} is not a value operand")),
        }
    }

    fn target(&self, instruction: &Instruction, index: usize) -> Result<usize, String> {
        let Some(Operand::IdRef(label)) = instruction.operands.get(index) else {
            return Err(format!(
                "{:?} operand {index} is not a label",
                instruction.class.opcode
            ));
        };
        self.blocks
            .get(label)
            .copied()
            .ok_or_else(|| format!("branch to %{label}, which is not a block of this function"))
    }

    fn switch_target(&self, instruction: &Instruction, selector: u32) -> Result<usize, String> {
        let mut index = 2;
        while index + 1 < instruction.operands.len() {
            let literal = match &instruction.operands[index] {
                Operand::LiteralBit32(value) => u64::from(*value),
                Operand::LiteralBit64(value) => *value,
                other => return Err(format!("OpSwitch case literal is {other:?}")),
            };
            if literal == u64::from(selector) {
                return self.target(instruction, index + 1);
            }
            index += 2;
        }
        self.target(instruction, 1)
    }
}

fn literal(instruction: &Instruction) -> Result<u32, String> {
    match instruction.operands.first() {
        Some(Operand::LiteralBit32(value)) => Ok(*value),
        Some(Operand::LiteralBit64(value)) => Ok(*value as u32),
        other => Err(format!("OpConstant operand is {other:?}")),
    }
}
