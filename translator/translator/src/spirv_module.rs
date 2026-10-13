use crate::fxhash::{HashMap, HashSet};
use crate::spirv_binary::{self, Error as ParseError};
use spirv::{Decoration, Op, Word};

pub(crate) use crate::spirv_operand::Operand;
use crate::spirv_operand::{Assemble, Disassemble};

pub(crate) fn descriptor_bindings_in_set(module: &Module, set: u32) -> HashSet<u32> {
    let targets = module
        .annotations
        .iter()
        .filter_map(|instruction| {
            if instruction.class.opcode != Op::Decorate
                || instruction.operands.get(1)
                    != Some(&Operand::Decoration(Decoration::DescriptorSet))
                || instruction.operands.get(2) != Some(&Operand::LiteralBit32(set))
            {
                return None;
            }
            match instruction.operands.first() {
                Some(Operand::IdRef(target)) => Some(*target),
                _ => None,
            }
        })
        .collect::<HashSet<_>>();
    module
        .annotations
        .iter()
        .filter_map(|instruction| {
            if instruction.class.opcode != Op::Decorate
                || instruction.operands.get(1) != Some(&Operand::Decoration(Decoration::Binding))
            {
                return None;
            }
            let Some(Operand::IdRef(target)) = instruction.operands.first() else {
                return None;
            };
            if !targets.contains(target) {
                return None;
            }
            match instruction.operands.get(2) {
                Some(Operand::LiteralBit32(binding)) => Some(*binding),
                _ => None,
            }
        })
        .collect()
}

fn is_location_debug(opcode: Op) -> bool {
    matches!(opcode, Op::Line | Op::NoLine)
}

pub(crate) fn block_successors(block: &Block) -> Vec<Word> {
    fn id_ref(operand: &Operand) -> Option<Word> {
        match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        }
    }
    let Some(inst) = block.instructions.last() else {
        return Vec::new();
    };
    match inst.class.opcode {
        Op::Branch => inst.operands.first().and_then(id_ref).into_iter().collect(),
        Op::BranchConditional => inst
            .operands
            .iter()
            .skip(1)
            .take(2)
            .filter_map(id_ref)
            .collect(),
        Op::Switch => {
            let mut out = Vec::new();
            if let Some(default) = inst.operands.get(1).and_then(id_ref) {
                out.push(default);
            }
            let mut idx = 3;
            while idx < inst.operands.len() {
                if let Some(target) = inst.operands.get(idx).and_then(id_ref) {
                    out.push(target);
                }
                idx += 2;
            }
            out.sort_unstable();
            out.dedup();
            out
        }
        _ => Vec::new(),
    }
}

pub(crate) fn block_successors_by_label(blocks: &[Block]) -> HashMap<Word, Vec<Word>> {
    blocks
        .iter()
        .filter_map(|block| Some((block.label.as_ref()?.result_id?, block_successors(block))))
        .collect()
}

pub(crate) fn array_type(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<(Word, u32)> {
    let def = defs.get(&ty)?;
    if def.class.opcode != Op::TypeArray {
        return None;
    }
    let elem = match def.operands.first()? {
        Operand::IdRef(elem) => *elem,
        _ => return None,
    };
    let len_const = match def.operands.get(1)? {
        Operand::IdRef(len_const) => *len_const,
        _ => return None,
    };
    let len = defs
        .get(&len_const)
        .and_then(|constant| match constant.operands.first() {
            Some(Operand::LiteralBit32(len)) => Some(*len),
            _ => None,
        })?;
    Some((elem, len))
}

pub(crate) fn type_int_width(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let def = defs.get(&ty)?;
    (def.class.opcode == Op::TypeInt).then(|| match def.operands.first() {
        Some(Operand::LiteralBit32(width)) => *width,
        _ => 32,
    })
}

pub(crate) fn type_float_width(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let def = defs.get(&ty)?;
    (def.class.opcode == Op::TypeFloat).then(|| match def.operands.first() {
        Some(Operand::LiteralBit32(width)) => *width,
        _ => 32,
    })
}

pub(crate) fn ptr_access_chain_allowed_storage(storage: spirv::StorageClass) -> bool {
    matches!(
        storage,
        spirv::StorageClass::Workgroup
            | spirv::StorageClass::StorageBuffer
            | spirv::StorageClass::PhysicalStorageBuffer
    )
}

pub(crate) fn is_block_terminator(opcode: Op) -> bool {
    matches!(
        opcode,
        Op::Branch
            | Op::BranchConditional
            | Op::Switch
            | Op::Return
            | Op::ReturnValue
            | Op::Kill
            | Op::TerminateInvocation
            | Op::TerminateRayKHR
            | Op::IgnoreIntersectionKHR
            | Op::EmitMeshTasksEXT
            | Op::Unreachable
    )
}

const EXTENDED_INSTRUCTION_SETS: &[&str] = &[
    "Arm.MotionEngine.100",
    "DebugInfo",
    "GLSL.std.450",
    "NonSemantic.ClspvReflection",
    "NonSemantic.DebugBreak",
    "NonSemantic.DebugPrintf",
    "NonSemantic.Shader.DebugInfo.100",
    "NonSemantic.VkspReflection",
    "OpenCL.DebugInfo.100",
    "OpenCL.std",
    "SPV_AMD_gcn_shader",
    "SPV_AMD_shader_ballot",
    "SPV_AMD_shader_explicit_vertex_parameter",
    "SPV_AMD_shader_trinary_minmax",
    "TOSA.001000.1",
];

fn is_extended_instruction_set(name: &str) -> bool {
    EXTENDED_INSTRUCTION_SETS.contains(&name)
}

fn extended_instruction_name(set: &str, opcode: u32) -> Option<String> {
    macro_rules! name {
        ($opcode:ty) => {
            <$opcode>::from_u32(opcode).map(|opcode| format!("{opcode:?}"))
        };
    }

    match set {
        "Arm.MotionEngine.100" => name!(spirv::ArmMotionEngine100Op),
        "DebugInfo" => name!(spirv::DebuginfoOp),
        "GLSL.std.450" => name!(spirv::GlslStd450Op),
        "NonSemantic.ClspvReflection" => name!(spirv::NonsemanticClspvreflectionOp),
        "NonSemantic.DebugBreak" => name!(spirv::NonsemanticDebugbreakOp),
        "NonSemantic.DebugPrintf" => name!(spirv::NonsemanticDebugprintfOp),
        "NonSemantic.Shader.DebugInfo.100" => name!(spirv::NonsemanticShaderDebuginfo100Op),
        "NonSemantic.VkspReflection" => name!(spirv::NonsemanticVkspreflectionOp),
        "OpenCL.DebugInfo.100" => name!(spirv::OpenclDebuginfo100Op),
        "OpenCL.std" => name!(spirv::OpenclStd100Op),
        "SPV_AMD_gcn_shader" => name!(spirv::SpvAmdGcnShaderOp),
        "SPV_AMD_shader_ballot" => name!(spirv::SpvAmdShaderBallotOp),
        "SPV_AMD_shader_explicit_vertex_parameter" => {
            name!(spirv::SpvAmdShaderExplicitVertexParameterOp)
        }
        "SPV_AMD_shader_trinary_minmax" => name!(spirv::SpvAmdShaderTrinaryMinmaxOp),
        "TOSA.001000.1" => name!(spirv::Tosa0010001Op),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleHeader {
    pub(crate) magic_number: Word,
    pub(crate) version: Word,
    pub(crate) generator: Word,
    pub(crate) bound: Word,
    pub(crate) reserved_word: Word,
}

impl ModuleHeader {
    pub(crate) fn new(bound: Word) -> Self {
        Self {
            magic_number: spirv::MAGIC_NUMBER,
            version: Self::version_word(spirv::MAJOR_VERSION, spirv::MINOR_VERSION),
            generator: 0x000f_0000,
            bound,
            reserved_word: 0,
        }
    }

    pub(crate) fn set_version(&mut self, major: u8, minor: u8) {
        self.version = Self::version_word(major, minor);
    }

    pub(crate) fn version(&self) -> (u8, u8) {
        ((self.version >> 16) as u8, (self.version >> 8) as u8)
    }

    fn version_word(major: u8, minor: u8) -> Word {
        (Word::from(major) << 16) | (Word::from(minor) << 8)
    }

    fn generator(&self) -> (&'static str, u16) {
        let tool = self.generator >> 16;
        let version = self.generator as u16;
        let tool = match tool {
            0 => "The Khronos Group",
            1 => "LunarG",
            2 => "Valve",
            3 => "Codeplay",
            4 => "NVIDIA",
            5 => "ARM",
            6 => "LLVM/SPIR-V Translator",
            7 => "SPIR-V Tools Assembler",
            8 => "Glslang",
            9 => "Qualcomm",
            10 => "AMD",
            11 => "Intel",
            12 => "Imagination",
            13 => "Shaderc",
            14 => "spiregg",
            15 => concat!("rspi", "rv"),
            _ => "Unknown",
        };
        (tool, version)
    }

    fn disassemble(&self) -> String {
        let (major, minor) = self.version();
        let (vendor, _) = self.generator();
        format!(
            "; SPIR-V\n; Version: {major}.{minor}\n; Generator: {vendor}\n; Bound: {}",
            self.bound
        )
    }

    fn assemble_into(&self, words: &mut Vec<Word>) {
        words.extend([
            self.magic_number,
            self.version,
            self.generator,
            self.bound,
            self.reserved_word,
        ]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstructionClass {
    pub(crate) opcode: Op,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Instruction {
    pub(crate) class: InstructionClass,
    pub(crate) result_type: Option<Word>,
    pub(crate) result_id: Option<Word>,
    pub(crate) operands: Vec<Operand>,
}

impl Instruction {
    pub(crate) fn new(
        opcode: Op,
        result_type: Option<Word>,
        result_id: Option<Word>,
        operands: Vec<Operand>,
    ) -> Self {
        Self {
            class: InstructionClass { opcode },
            result_type,
            result_id,
            operands,
        }
    }
}

impl Assemble for Instruction {
    fn assemble_into(&self, words: &mut Vec<Word>) {
        let start = words.len();
        words.push(self.class.opcode as Word);
        words.extend(self.result_type);
        words.extend(self.result_id);
        for operand in &self.operands {
            operand.assemble_into(words);
        }
        let word_count = words.len() - start;
        words[start] |= (word_count as Word) << 16;
    }
}

impl Disassemble for Instruction {
    fn disassemble(&self) -> String {
        let operands = self
            .operands
            .iter()
            .map(Disassemble::disassemble)
            .collect::<Vec<_>>()
            .join(" ");
        disassemble_instruction_with_operands(self, operands)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Function {
    pub(crate) def: Option<Instruction>,
    pub(crate) end: Option<Instruction>,
    pub(crate) parameters: Vec<Instruction>,
    pub(crate) blocks: Vec<Block>,
}

impl Function {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn all_inst_iter(&self) -> impl Iterator<Item = &Instruction> {
        self.def
            .iter()
            .chain(&self.parameters)
            .chain(
                self.blocks
                    .iter()
                    .flat_map(|block| block.label.iter().chain(&block.instructions)),
            )
            .chain(&self.end)
    }

    pub(crate) fn all_inst_iter_mut(&mut self) -> impl Iterator<Item = &mut Instruction> {
        self.def
            .iter_mut()
            .chain(&mut self.parameters)
            .chain(
                self.blocks
                    .iter_mut()
                    .flat_map(|block| block.label.iter_mut().chain(&mut block.instructions)),
            )
            .chain(&mut self.end)
    }

    fn assemble_into(&self, words: &mut Vec<Word>) {
        for instruction in self.all_inst_iter() {
            instruction.assemble_into(words);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Block {
    pub(crate) label: Option<Instruction>,
    pub(crate) instructions: Vec<Instruction>,
}

impl Block {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SpirvModule {
    next_id: Word,
    pub(crate) header: Option<ModuleHeader>,
    pub(crate) capabilities: Vec<Instruction>,
    pub(crate) extensions: Vec<Instruction>,
    pub(crate) ext_inst_imports: Vec<Instruction>,
    pub(crate) memory_model: Option<Instruction>,
    pub(crate) entry_points: Vec<Instruction>,
    pub(crate) execution_modes: Vec<Instruction>,
    pub(crate) debug_string_source: Vec<Instruction>,
    pub(crate) debug_names: Vec<Instruction>,
    pub(crate) debug_module_processed: Vec<Instruction>,
    pub(crate) annotations: Vec<Instruction>,
    pub(crate) types_global_values: Vec<Instruction>,
    pub(crate) functions: Vec<Function>,
}

impl Default for SpirvModule {
    fn default() -> Self {
        Self {
            next_id: 1,
            header: None,
            capabilities: Vec::new(),
            extensions: Vec::new(),
            ext_inst_imports: Vec::new(),
            memory_model: None,
            entry_points: Vec::new(),
            execution_modes: Vec::new(),
            debug_string_source: Vec::new(),
            debug_names: Vec::new(),
            debug_module_processed: Vec::new(),
            annotations: Vec::new(),
            types_global_values: Vec::new(),
            functions: Vec::new(),
        }
    }
}

impl SpirvModule {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn fresh_id(&mut self) -> Word {
        self.catch_up_to_header_bound();
        let id = self.next_id;
        self.next_id += 1;
        self.update_header_bound();
        id
    }

    pub(crate) fn reserve_ids(&mut self, count: Word) -> std::ops::Range<Word> {
        self.catch_up_to_header_bound();
        let start = self.next_id;
        self.next_id += count;
        self.update_header_bound();
        start..self.next_id
    }

    pub(crate) fn id_bound(&self) -> Word {
        self.header
            .as_ref()
            .map_or(self.next_id, |header| self.next_id.max(header.bound))
    }

    pub(crate) fn sync_id_bound_from_header(&mut self) {
        if let Some(header) = &self.header {
            self.next_id = header.bound;
        }
    }

    pub(crate) fn sync_id_bound_from_instructions(&mut self) {
        let definitions_bound = self
            .all_inst_iter()
            .filter_map(|instruction| instruction.result_id)
            .max()
            .map_or(1, |id| id.saturating_add(1));
        let header_bound = self.header.as_ref().map_or(1, |header| header.bound);
        self.next_id = self.next_id.max(header_bound).max(definitions_bound);
        self.update_header_bound();
    }

    pub(crate) fn set_id_bound(&mut self, bound: Word) {
        self.next_id = bound;
        self.update_header_bound();
    }

    fn update_header_bound(&mut self) {
        if let Some(header) = self.header.as_mut() {
            header.bound = self.next_id;
        }
    }

    fn catch_up_to_header_bound(&mut self) {
        if let Some(header) = &self.header {
            self.next_id = self.next_id.max(header.bound);
        }
    }

    pub(crate) fn global_inst_iter(&self) -> impl Iterator<Item = &Instruction> {
        self.capabilities
            .iter()
            .chain(&self.extensions)
            .chain(&self.ext_inst_imports)
            .chain(&self.memory_model)
            .chain(&self.entry_points)
            .chain(&self.execution_modes)
            .chain(&self.debug_string_source)
            .chain(&self.debug_names)
            .chain(&self.debug_module_processed)
            .chain(&self.annotations)
            .chain(&self.types_global_values)
    }

    pub(crate) fn all_inst_iter(&self) -> impl Iterator<Item = &Instruction> {
        self.global_inst_iter()
            .chain(self.functions.iter().flat_map(Function::all_inst_iter))
    }

    pub(crate) fn all_inst_iter_mut(&mut self) -> impl Iterator<Item = &mut Instruction> {
        self.capabilities
            .iter_mut()
            .chain(&mut self.extensions)
            .chain(&mut self.ext_inst_imports)
            .chain(&mut self.memory_model)
            .chain(&mut self.entry_points)
            .chain(&mut self.execution_modes)
            .chain(&mut self.debug_string_source)
            .chain(&mut self.debug_names)
            .chain(&mut self.debug_module_processed)
            .chain(&mut self.annotations)
            .chain(&mut self.types_global_values)
            .chain(
                self.functions
                    .iter_mut()
                    .flat_map(Function::all_inst_iter_mut),
            )
    }

    pub(crate) fn assemble(&self) -> Vec<Word> {
        let mut words = Vec::new();
        self.assemble_into(&mut words);
        words
    }

    fn assemble_into(&self, words: &mut Vec<Word>) {
        if let Some(header) = &self.header {
            header.assemble_into(words);
        }
        for instruction in self.global_inst_iter() {
            instruction.assemble_into(words);
        }
        for function in &self.functions {
            function.assemble_into(words);
        }
    }

    pub(crate) fn disassemble(&self) -> String {
        let mut extended_sets = HashMap::default();
        for instruction in &self.ext_inst_imports {
            let (Some(id), [Operand::LiteralString(name)]) =
                (instruction.result_id, instruction.operands.as_slice())
            else {
                continue;
            };
            if is_extended_instruction_set(name) {
                extended_sets.insert(id, name.clone());
            } else {
                eprintln!("ERROR: Extended instruction set `{name}` not recognized");
            }
        }

        let mut literal_types = LiteralTypeTracker::default();
        for instruction in &self.types_global_values {
            literal_types.track(instruction);
        }

        let mut lines = Vec::new();
        if let Some(header) = &self.header {
            push_nonempty(&mut lines, header.disassemble());
        }
        push_nonempty(
            &mut lines,
            self.global_inst_iter()
                .map(|instruction| {
                    if instruction.class.opcode == Op::Constant {
                        disassemble_constant(instruction, &literal_types)
                    } else {
                        instruction.disassemble()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );

        for function in &self.functions {
            push_nonempty(
                &mut lines,
                function
                    .def
                    .as_ref()
                    .map_or(String::new(), Disassemble::disassemble),
            );
            push_nonempty(
                &mut lines,
                function
                    .parameters
                    .iter()
                    .map(Disassemble::disassemble)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            for block in &function.blocks {
                push_nonempty(
                    &mut lines,
                    block
                        .label
                        .as_ref()
                        .map_or(String::new(), Disassemble::disassemble),
                );
                for instruction in &block.instructions {
                    let line = if instruction.class.opcode == Op::ExtInst {
                        disassemble_extended_instruction(instruction, &extended_sets)
                    } else {
                        instruction.disassemble()
                    };
                    push_nonempty(&mut lines, line);
                }
            }
            push_nonempty(
                &mut lines,
                function
                    .end
                    .as_ref()
                    .map_or(String::new(), Disassemble::disassemble),
            );
        }
        lines.join("\n")
    }
}

pub(crate) type Module = SpirvModule;

#[derive(Clone, Copy)]
enum LiteralType {
    Integer { signed: bool },
    Float { width: u32 },
}

#[derive(Default)]
struct LiteralTypeTracker {
    types: HashMap<Word, LiteralType>,
}

impl LiteralTypeTracker {
    fn track(&mut self, instruction: &Instruction) {
        let Some(id) = instruction.result_id else {
            return;
        };
        let literal_type = match instruction.class.opcode {
            Op::TypeInt => match instruction.operands.as_slice() {
                [Operand::LiteralBit32(_), Operand::LiteralBit32(signed)] => {
                    Some(LiteralType::Integer {
                        signed: *signed == 1,
                    })
                }
                _ => None,
            },
            Op::TypeFloat => match instruction.operands.as_slice() {
                [Operand::LiteralBit32(width), ..] => Some(LiteralType::Float { width: *width }),
                _ => None,
            },
            _ => instruction
                .result_type
                .and_then(|result_type| self.types.get(&result_type).copied()),
        };
        if let Some(literal_type) = literal_type {
            self.types.insert(id, literal_type);
        }
    }

    fn resolve(&self, id: Word) -> Option<LiteralType> {
        self.types.get(&id).copied()
    }
}

fn push_nonempty(lines: &mut Vec<String>, line: String) {
    if !line.is_empty() {
        lines.push(line);
    }
}

fn disassemble_instruction_with_operands(instruction: &Instruction, operands: String) -> String {
    let space = if operands.is_empty() { "" } else { " " };
    let opcode = format!("{:?}", instruction.class.opcode);
    format!(
        "{result_id}Op{opcode}{result_type}{space}{operands}",
        result_id = instruction
            .result_id
            .map_or(String::new(), |id| format!("%{id} = ")),
        result_type = instruction
            .result_type
            .map_or(String::new(), |id| format!("  %{id}{space}")),
    )
}

fn disassemble_constant(instruction: &Instruction, literal_types: &LiteralTypeTracker) -> String {
    debug_assert_eq!(instruction.class.opcode, Op::Constant);
    debug_assert_eq!(instruction.operands.len(), 1);
    let literal_type = instruction
        .result_type
        .and_then(|id| literal_types.resolve(id))
        .expect("OpConstant result type must resolve to an integer or float");
    let operand = match instruction.operands[0] {
        Operand::LiteralBit32(value) => match literal_type {
            LiteralType::Integer { signed: true } => (value as i32).to_string(),
            LiteralType::Integer { signed: false } => value.to_string(),
            LiteralType::Float { width: 16 } => {
                crate::float16::f16_bits_to_f32(value as u16).to_string()
            }
            LiteralType::Float { .. } => f32::from_bits(value).to_string(),
        },
        Operand::LiteralBit64(value) => match literal_type {
            LiteralType::Integer { signed: true } => (value as i64).to_string(),
            LiteralType::Integer { signed: false } => value.to_string(),
            LiteralType::Float { .. } => f64::from_bits(value).to_string(),
        },
        _ => return instruction.disassemble(),
    };
    disassemble_instruction_with_operands(instruction, operand)
}

fn disassemble_extended_instruction(
    instruction: &Instruction,
    extended_sets: &HashMap<Word, String>,
) -> String {
    let [Operand::IdRef(set), Operand::LiteralExtInstInteger(opcode), rest @ ..] =
        instruction.operands.as_slice()
    else {
        return instruction.disassemble();
    };
    let Some(name) = extended_sets
        .get(set)
        .and_then(|name| extended_instruction_name(name, *opcode))
    else {
        return instruction.disassemble();
    };

    let operands = std::iter::once(format!("%{set}"))
        .chain(std::iter::once(name))
        .chain(rest.iter().map(Disassemble::disassemble))
        .collect::<Vec<_>>()
        .join(" ");
    disassemble_instruction_with_operands(instruction, operands)
}

#[derive(Default)]
struct Loader {
    module: SpirvModule,
    function: Option<Function>,
    block: Option<Block>,
}

#[derive(Debug)]
pub(crate) enum LoadError {
    Parse(ParseError),
    NestedFunction,
    UnclosedFunction,
    MismatchedFunctionEnd,
    DetachedFunctionParameter,
    DetachedBlock,
    NestedBlock,
    UnclosedBlock,
    MismatchedTerminator,
    DetachedInstruction(Instruction),
}

impl From<ParseError> for LoadError {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(formatter, "{error}"),
            Self::NestedFunction => write!(formatter, "found nested function"),
            Self::UnclosedFunction => write!(formatter, "found unclosed function"),
            Self::MismatchedFunctionEnd => write!(formatter, "found mismatched OpFunctionEnd"),
            Self::DetachedFunctionParameter => {
                write!(formatter, "found OpFunctionParameter outside a function")
            }
            Self::DetachedBlock => write!(formatter, "found block outside a function"),
            Self::NestedBlock => write!(formatter, "found nested block"),
            Self::UnclosedBlock => write!(formatter, "found block without terminator"),
            Self::MismatchedTerminator => write!(formatter, "found mismatched terminator"),
            Self::DetachedInstruction(instruction) => write!(
                formatter,
                "found {:?} instruction outside a block",
                instruction.class.opcode
            ),
        }
    }
}

impl std::error::Error for LoadError {}

macro_rules! reject_if {
    ($condition:expr, $error:ident) => {
        if $condition {
            return Err(LoadError::$error);
        }
    };
}

impl Loader {
    fn finalize(&mut self) -> Result<(), LoadError> {
        reject_if!(self.block.is_some(), UnclosedBlock);
        reject_if!(self.function.is_some(), UnclosedFunction);
        Ok(())
    }

    fn consume_header(&mut self, header: ModuleHeader) {
        self.module.header = Some(header);
        self.module.sync_id_bound_from_header();
    }

    fn consume_instruction(&mut self, instruction: Instruction) -> Result<(), LoadError> {
        let opcode = instruction.class.opcode;
        match opcode {
            Op::Capability => self.module.capabilities.push(instruction),
            Op::Extension => self.module.extensions.push(instruction),
            Op::ExtInstImport => self.module.ext_inst_imports.push(instruction),
            Op::MemoryModel => self.module.memory_model = Some(instruction),
            Op::EntryPoint => self.module.entry_points.push(instruction),
            Op::ExecutionMode | Op::ExecutionModeId => {
                self.module.execution_modes.push(instruction)
            }
            Op::String | Op::SourceExtension | Op::Source | Op::SourceContinued => {
                self.module.debug_string_source.push(instruction);
            }
            Op::Name | Op::MemberName => self.module.debug_names.push(instruction),
            Op::ModuleProcessed => self.module.debug_module_processed.push(instruction),
            opcode if is_location_debug(opcode) => {
                if let Some(block) = &mut self.block {
                    block.instructions.push(instruction);
                } else {
                    self.module.types_global_values.push(instruction);
                }
            }
            opcode if opcode.is_annotation() => self.module.annotations.push(instruction),
            opcode if opcode.is_type() || opcode.is_constant() => {
                self.module.types_global_values.push(instruction);
            }
            Op::Variable | Op::Undef if self.function.is_none() => {
                self.module.types_global_values.push(instruction);
            }
            Op::Function => {
                reject_if!(self.function.is_some(), NestedFunction);
                let mut function = Function::new();
                function.def = Some(instruction);
                self.function = Some(function);
            }
            Op::FunctionEnd => {
                reject_if!(self.function.is_none(), MismatchedFunctionEnd);
                reject_if!(self.block.is_some(), UnclosedBlock);
                let mut function = self.function.take().expect("checked above");
                function.end = Some(instruction);
                self.module.functions.push(function);
            }
            Op::FunctionParameter => {
                reject_if!(self.function.is_none(), DetachedFunctionParameter);
                self.function
                    .as_mut()
                    .expect("checked above")
                    .parameters
                    .push(instruction);
            }
            Op::Label => {
                reject_if!(self.function.is_none(), DetachedBlock);
                reject_if!(self.block.is_some(), NestedBlock);
                let mut block = Block::new();
                block.label = Some(instruction);
                self.block = Some(block);
            }
            opcode if is_block_terminator(opcode) => {
                reject_if!(self.block.is_none(), MismatchedTerminator);
                let mut block = self.block.take().expect("checked above");
                block.instructions.push(instruction);
                self.function
                    .as_mut()
                    .expect("a block requires a function")
                    .blocks
                    .push(block);
            }
            _ => {
                if self.block.is_none() {
                    return Err(LoadError::DetachedInstruction(instruction));
                }
                self.block
                    .as_mut()
                    .expect("checked above")
                    .instructions
                    .push(instruction);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static LOAD_BYTES_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_load_bytes_count() {
    LOAD_BYTES_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn load_bytes_count() -> usize {
    LOAD_BYTES_COUNT.with(std::cell::Cell::get)
}

pub(crate) fn load_bytes(bytes: impl AsRef<[u8]>) -> Result<SpirvModule, LoadError> {
    #[cfg(test)]
    LOAD_BYTES_COUNT.with(|count| count.set(count.get() + 1));
    let parsed = spirv_binary::parse_bytes(bytes.as_ref())?;
    let mut loader = Loader::default();
    loader.consume_header(parsed.header);
    for instruction in parsed.instructions {
        loader.consume_instruction(instruction)?;
    }
    loader.finalize()?;
    Ok(loader.module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spirv::{
        AddressingModel, Capability, ExecutionModel, FunctionControl, GlslStd450Op, MemoryModel,
    };

    #[test]
    fn constants_disassemble_at_the_width_of_their_type() {
        let mut module = SpirvModule::new();
        module.header = Some(ModuleHeader::new(9));
        let mut ty = |id: Word, opcode: Op, operands: Vec<Operand>| {
            module
                .types_global_values
                .push(Instruction::new(opcode, None, Some(id), operands));
        };
        ty(1, Op::TypeFloat, vec![Operand::LiteralBit32(16)]);
        ty(2, Op::TypeFloat, vec![Operand::LiteralBit32(32)]);
        ty(
            3,
            Op::TypeInt,
            vec![Operand::LiteralBit32(16), Operand::LiteralBit32(1)],
        );
        for (id, ty_id, bits) in [
            (4u32, 1u32, 0x7bffu32),
            (5, 1, 0x7c00),
            (6, 1, 0xbc00),
            (7, 2, 0x3f80_0000),
            (8, 3, 0xffff_ffff),
        ] {
            module.types_global_values.push(Instruction::new(
                Op::Constant,
                Some(ty_id),
                Some(id),
                vec![Operand::LiteralBit32(bits)],
            ));
        }
        let asm = module.disassemble();
        assert!(asm.contains("%4 = OpConstant  %1  65504"), "{asm}");
        assert!(asm.contains("%5 = OpConstant  %1  inf"), "{asm}");
        assert!(asm.contains("%6 = OpConstant  %1  -1"), "{asm}");
        assert!(asm.contains("%7 = OpConstant  %2  1"), "{asm}");
        assert!(asm.contains("%8 = OpConstant  %3  -1"), "{asm}");
    }

    fn fixture_words() -> Vec<Word> {
        let mut module = SpirvModule::new();
        let mut header = ModuleHeader::new(5);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.memory_model = Some(Instruction::new(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.entry_points.push(Instruction::new(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(ExecutionModel::GLCompute),
                Operand::IdRef(3),
                Operand::LiteralString("main".to_string()),
            ],
        ));
        module.execution_modes.push(Instruction::new(
            Op::ExecutionModeId,
            None,
            None,
            vec![
                Operand::IdRef(3),
                Operand::ExecutionMode(spirv::ExecutionMode::SubgroupsPerWorkgroupId),
                Operand::IdRef(4),
            ],
        ));
        module
            .types_global_values
            .push(Instruction::new(Op::TypeVoid, None, Some(1), vec![]));
        module.types_global_values.push(Instruction::new(
            Op::TypeFunction,
            None,
            Some(2),
            vec![Operand::IdRef(1)],
        ));
        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(1),
            Some(3),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(2),
            ],
        ));
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(4), vec![]));
        block
            .instructions
            .push(Instruction::new(Op::Return, None, None, vec![]));
        function.blocks.push(block);
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        module.functions.push(function);
        module.assemble()
    }

    fn words_to_bytes(words: &[Word]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn opcode_classification_covers_the_locked_contract() {
        let terminators = [
            Op::Branch,
            Op::BranchConditional,
            Op::Switch,
            Op::Return,
            Op::ReturnValue,
            Op::Kill,
            Op::TerminateInvocation,
            Op::TerminateRayKHR,
            Op::IgnoreIntersectionKHR,
            Op::EmitMeshTasksEXT,
            Op::Unreachable,
        ];
        for raw in 0..=u16::MAX {
            let Some(opcode) = Op::from_u32(u32::from(raw)) else {
                continue;
            };
            assert_eq!(
                is_location_debug(opcode),
                matches!(opcode, Op::Line | Op::NoLine)
            );
            assert_eq!(is_block_terminator(opcode), terminators.contains(&opcode));
        }
    }

    #[test]
    fn extended_instruction_registry_covers_every_locked_set() {
        assert!(EXTENDED_INSTRUCTION_SETS
            .iter()
            .all(|set| is_extended_instruction_set(set)));
        assert_eq!(
            extended_instruction_name("GLSL.std.450", GlslStd450Op::FAbs as u32).as_deref(),
            Some("FAbs")
        );
        assert_eq!(extended_instruction_name("GLSL.std.450", 0), None);
        assert_eq!(extended_instruction_name("Unknown.Set", 1), None);
    }

    #[test]
    fn module_load_and_assemble_preserve_exact_bytes_and_sections() {
        let expected = fixture_words();
        let mut module = load_bytes(words_to_bytes(&expected)).expect("load fixture");
        assert_eq!(module.header.as_ref().map(|header| header.bound), Some(5));
        assert_eq!(module.capabilities.len(), 1);
        assert!(module.memory_model.is_some());
        assert_eq!(module.entry_points.len(), 1);
        assert_eq!(module.execution_modes.len(), 1);
        assert_eq!(module.types_global_values.len(), 2);
        assert_eq!(module.functions.len(), 1);
        assert_eq!(module.functions[0].blocks.len(), 1);
        assert_eq!(module.assemble(), expected);
        assert_eq!(module.fresh_id(), 5);
        assert_eq!(module.id_bound(), 6);
    }

    #[test]
    fn loader_classifies_detached_terminator() {
        let mut words = fixture_words();
        let function_word = words
            .iter()
            .position(|word| (*word & 0xffff) == Op::Function as u32)
            .expect("function instruction");
        words.insert(function_word, (1_u32 << 16) | Op::Return as u32);
        assert!(matches!(
            load_bytes(words_to_bytes(&words)),
            Err(LoadError::MismatchedTerminator)
        ));
    }
}
