use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{MemoryAccess, Op, StorageClass, Word};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    WorkgroupStore,
    DeviceLoad,
    WorkgroupLoad,
    Pure,
    Hard,
}

const PURE: &[Op] = &[
    Op::IAdd,
    Op::ISub,
    Op::IMul,
    Op::UDiv,
    Op::SDiv,
    Op::UMod,
    Op::SRem,
    Op::SMod,
    Op::ShiftLeftLogical,
    Op::ShiftRightLogical,
    Op::ShiftRightArithmetic,
    Op::BitwiseAnd,
    Op::BitwiseOr,
    Op::BitwiseXor,
    Op::Not,
    Op::SNegate,
    Op::FAdd,
    Op::FSub,
    Op::FMul,
    Op::FDiv,
    Op::FNegate,
    Op::UConvert,
    Op::SConvert,
    Op::FConvert,
    Op::ConvertFToU,
    Op::ConvertFToS,
    Op::ConvertUToF,
    Op::ConvertSToF,
    Op::Bitcast,
    Op::CompositeExtract,
    Op::CompositeConstruct,
    Op::CompositeInsert,
    Op::VectorShuffle,
    Op::CopyObject,
    Op::Select,
    Op::IEqual,
    Op::INotEqual,
    Op::ULessThan,
    Op::ULessThanEqual,
    Op::UGreaterThan,
    Op::UGreaterThanEqual,
    Op::SLessThan,
    Op::SLessThanEqual,
    Op::SGreaterThan,
    Op::SGreaterThanEqual,
    Op::LogicalAnd,
    Op::LogicalOr,
    Op::LogicalNot,
    Op::LogicalEqual,
    Op::LogicalNotEqual,
    Op::AccessChain,
    Op::InBoundsAccessChain,
    Op::PtrAccessChain,
    Op::InBoundsPtrAccessChain,
    Op::ConvertUToPtr,
    Op::ConvertPtrToU,
    Op::BitFieldInsert,
    Op::BitFieldUExtract,
    Op::BitFieldSExtract,
    Op::BitCount,
    Op::BitReverse,
    Op::IAddCarry,
    Op::ISubBorrow,
    Op::UMulExtended,
    Op::SMulExtended,
    Op::Undef,
    Op::VectorTimesScalar,
    Op::Dot,
    Op::Line,
    Op::NoLine,
];

struct Classes {
    pointer_class: HashMap<Word, StorageClass>,
    value_type: HashMap<Word, Word>,
    glsl_sets: HashSet<Word>,
}

impl Classes {
    fn of(module: &Module) -> Self {
        let pointer_class = module
            .types_global_values
            .iter()
            .filter(|i| i.class.opcode == Op::TypePointer)
            .filter_map(|i| match i.operands.first() {
                Some(Operand::StorageClass(class)) => Some((i.result_id?, *class)),
                _ => None,
            })
            .collect();
        let value_type = module
            .all_inst_iter()
            .filter_map(|i| Some((i.result_id?, i.result_type?)))
            .collect();
        let glsl_sets = module
            .ext_inst_imports
            .iter()
            .filter(|i| {
                matches!(i.operands.first(), Some(Operand::LiteralString(s)) if s == "GLSL.std.450")
            })
            .filter_map(|i| i.result_id)
            .collect();
        Self {
            pointer_class,
            value_type,
            glsl_sets,
        }
    }

    fn class(&self, pointer: &Operand) -> Option<StorageClass> {
        let Operand::IdRef(id) = pointer else {
            return None;
        };
        self.pointer_class.get(self.value_type.get(id)?).copied()
    }

    fn kind(&self, i: &Instruction) -> Kind {
        let access = i
            .operands
            .iter()
            .find_map(|o| match o {
                Operand::MemoryAccess(a) => Some(*a),
                _ => None,
            })
            .unwrap_or(MemoryAccess::NONE);
        match i.class.opcode {
            Op::Store => match i.operands.first().map(|p| self.class(p)) {
                Some(Some(StorageClass::Workgroup))
                    if !access.intersects(
                        MemoryAccess::VOLATILE | MemoryAccess::MAKE_POINTER_AVAILABLE,
                    ) =>
                {
                    Kind::WorkgroupStore
                }
                _ => Kind::Hard,
            },
            Op::Load
                if access
                    .intersects(MemoryAccess::VOLATILE | MemoryAccess::MAKE_POINTER_VISIBLE) =>
            {
                Kind::Hard
            }
            Op::Load => match i.operands.first().map(|p| self.class(p)) {
                Some(Some(StorageClass::Workgroup)) => Kind::WorkgroupLoad,
                Some(Some(_)) => Kind::DeviceLoad,
                _ => Kind::Hard,
            },
            Op::ExtInst => match i.operands.first() {
                Some(Operand::IdRef(set)) if self.glsl_sets.contains(set) => Kind::Pure,
                _ => Kind::Hard,
            },
            op if PURE.contains(&op) => Kind::Pure,
            _ => Kind::Hard,
        }
    }
}

fn id_operands(i: &Instruction) -> impl Iterator<Item = Word> + '_ {
    i.operands.iter().filter_map(|o| match o {
        Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => Some(*id),
        _ => None,
    })
}

fn address_slice(
    insts: &[Instruction],
    kinds: &[Kind],
    start: usize,
    load: usize,
) -> Option<Vec<usize>> {
    let mut need: HashSet<Word> = id_operands(&insts[load]).collect();
    let mut slice = Vec::new();
    for q in (start..load).rev() {
        if !insts[q].result_id.is_some_and(|id| need.contains(&id)) {
            continue;
        }
        if matches!(kinds[q], Kind::WorkgroupLoad | Kind::Hard) {
            return None;
        }
        slice.push(q);
        need.extend(id_operands(&insts[q]));
    }
    slice.reverse();
    Some(slice)
}

fn hoist_block(insts: &mut Vec<Instruction>, classes: &Classes) -> usize {
    let mut kinds: Vec<Kind> = insts.iter().map(|i| classes.kind(i)).collect();
    let mut moved = 0;
    let mut i = 0;
    while i < insts.len() {
        if kinds[i] != Kind::WorkgroupStore {
            i += 1;
            continue;
        }
        let mut at = i;
        let mut j = i + 1;
        while j < insts.len() && kinds[j] != Kind::Hard {
            if kinds[j] == Kind::DeviceLoad {
                if let Some(mut group) = address_slice(insts, &kinds, at, j) {
                    group.push(j);
                    let mut taken: Vec<(Instruction, Kind)> = group
                        .iter()
                        .rev()
                        .map(|&q| (insts.remove(q), kinds.remove(q)))
                        .collect();
                    taken.reverse();
                    let n = taken.len();
                    let (ti, tk): (Vec<_>, Vec<_>) = taken.into_iter().unzip();
                    insts.splice(at..at, ti);
                    kinds.splice(at..at, tk);
                    at += n;
                    moved += 1;
                    j += 1;
                    continue;
                }
            }
            j += 1;
        }
        i = j.max(i + 1);
    }
    moved
}

pub(crate) fn hoist_device_loads(module: &mut Module) -> usize {
    let classes = Classes::of(module);
    module
        .functions
        .iter_mut()
        .flat_map(|f| f.blocks.iter_mut())
        .map(|b| hoist_block(&mut b.instructions, &classes))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function};

    const UINT: Word = 1;
    const PTR_WG: Word = 2;
    const PTR_SB: Word = 3;
    const PTR_PSB: Word = 4;
    const C1: Word = 5;
    const C2: Word = 6;
    const TILE: Word = 7;
    const BUF: Word = 8;
    const X: Word = 9;

    fn ptr(id: Word, class: StorageClass) -> Instruction {
        Instruction::new(
            Op::TypePointer,
            None,
            Some(id),
            vec![Operand::StorageClass(class), Operand::IdRef(UINT)],
        )
    }

    fn ids(opcode: Op, ty: Option<Word>, id: Option<Word>, operands: &[Word]) -> Instruction {
        Instruction::new(
            opcode,
            ty,
            id,
            operands.iter().map(|o| Operand::IdRef(*o)).collect(),
        )
    }

    fn load(id: Word, pointer: Word) -> Instruction {
        Instruction::new(
            Op::Load,
            Some(UINT),
            Some(id),
            vec![
                Operand::IdRef(pointer),
                Operand::MemoryAccess(MemoryAccess::ALIGNED),
                Operand::LiteralBit32(4),
            ],
        )
    }

    fn store(pointer: Word, value: Word) -> Instruction {
        ids(Op::Store, None, None, &[pointer, value])
    }

    fn copy(n: Word, c: Word, buffer_class: Word) -> Vec<Instruction> {
        let (a, p, v, s) = (20 + 4 * n, 21 + 4 * n, 22 + 4 * n, 23 + 4 * n);
        vec![
            ids(Op::IAdd, Some(UINT), Some(a), &[X, c]),
            ids(
                Op::InBoundsAccessChain,
                Some(buffer_class),
                Some(p),
                &[BUF, a],
            ),
            load(v, p),
            ids(Op::InBoundsAccessChain, Some(PTR_WG), Some(s), &[TILE, v]),
            store(s, v),
        ]
    }

    fn module_with(body: Vec<Instruction>) -> Module {
        let mut module = Module::new();
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(UINT),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            ptr(PTR_WG, StorageClass::Workgroup),
            ptr(PTR_SB, StorageClass::StorageBuffer),
            ptr(PTR_PSB, StorageClass::PhysicalStorageBuffer),
            Instruction::new(
                Op::Constant,
                Some(UINT),
                Some(C1),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(UINT),
                Some(C2),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::Variable,
                Some(PTR_WG),
                Some(TILE),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::Variable,
                Some(PTR_SB),
                Some(BUF),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        let mut instructions = body;
        instructions.push(Instruction::new(Op::Return, None, None, vec![]));
        module.functions.push(Function {
            def: Some(Instruction::new(Op::Function, Some(UINT), Some(10), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![Instruction::new(
                Op::FunctionParameter,
                Some(UINT),
                Some(X),
                vec![],
            )],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(11), vec![])),
                instructions,
            }],
        });
        module.set_id_bound(100);
        module
    }

    fn steel() -> Vec<Instruction> {
        [copy(0, C1, PTR_SB), copy(1, C2, PTR_SB)].concat()
    }

    fn body(module: &Module) -> &[Instruction] {
        &module.functions[0].blocks[0].instructions
    }

    fn position(module: &Module, op: Op) -> Vec<usize> {
        body(module)
            .iter()
            .enumerate()
            .filter(|(_, i)| i.class.opcode == op)
            .map(|(n, _)| n)
            .collect()
    }

    fn ssa_order_holds(module: &Module) -> bool {
        let defined: HashSet<Word> = body(module).iter().filter_map(|i| i.result_id).collect();
        let mut seen = HashSet::default();
        body(module).iter().all(|i| {
            let ok = id_operands(i).all(|id| !defined.contains(&id) || seen.contains(&id));
            seen.extend(i.result_id);
            ok
        })
    }

    #[test]
    fn both_device_loads_end_up_above_the_first_workgroup_store() {
        let mut module = module_with(steel());
        assert_eq!(
            hoist_device_loads(&mut module),
            1,
            "the second copy's load is the one that moves"
        );
        let (loads, stores) = (position(&module, Op::Load), position(&module, Op::Store));
        assert!(
            loads.iter().max() < stores.iter().min(),
            "loads {loads:?} stores {stores:?}"
        );
    }

    #[test]
    fn the_workgroup_stores_keep_their_order_and_ssa_order_holds() {
        let original = module_with(steel());
        let mut module = original.clone();
        hoist_device_loads(&mut module);
        let values = |m: &Module| -> Vec<Operand> {
            body(m)
                .iter()
                .filter(|i| i.class.opcode == Op::Store)
                .map(|i| i.operands[1].clone())
                .collect()
        };
        assert_eq!(values(&module), values(&original));
        assert!(ssa_order_holds(&module));
        let mut a: Vec<_> = body(&original).iter().map(|i| format!("{i:?}")).collect();
        let mut b: Vec<_> = body(&module).iter().map(|i| format!("{i:?}")).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "the multiset of instructions is unchanged");
    }

    #[test]
    fn a_physical_storage_buffer_load_moves_too() {
        let mut module = module_with([copy(0, C1, PTR_SB), copy(1, C2, PTR_PSB)].concat());
        assert_eq!(hoist_device_loads(&mut module), 1);
    }

    #[test]
    fn a_load_never_crosses_a_control_barrier() {
        let mut b = copy(0, C1, PTR_SB);
        b.push(ids(Op::ControlBarrier, None, None, &[C2, C2, C1]));
        b.extend(copy(1, C2, PTR_SB));
        let mut module = module_with(b);
        assert_eq!(hoist_device_loads(&mut module), 0);
    }

    #[test]
    fn a_store_to_a_device_buffer_blocks_the_hoist() {
        let mut b = copy(0, C1, PTR_SB);
        b.push(ids(
            Op::InBoundsAccessChain,
            Some(PTR_SB),
            Some(90),
            &[BUF, X],
        ));
        b.push(store(90, 22));
        b.extend(copy(1, C2, PTR_SB));
        let mut module = module_with(b);
        assert_eq!(hoist_device_loads(&mut module), 0, "it may alias the load");
    }

    #[test]
    fn a_load_whose_address_comes_from_a_workgroup_load_stays() {
        let mut b = copy(0, C1, PTR_SB);
        b.push(ids(
            Op::InBoundsAccessChain,
            Some(PTR_WG),
            Some(90),
            &[TILE, X],
        ));
        b.push(load(91, 90));
        let mut second = copy(1, C2, PTR_SB);
        second[0] = ids(Op::IAdd, Some(UINT), Some(24), &[91, C2]);
        b.extend(second);
        let mut module = module_with(b);
        assert_eq!(hoist_device_loads(&mut module), 0);
    }

    #[test]
    fn a_volatile_workgroup_store_starts_no_segment() {
        let mut b = steel();
        for i in b.iter_mut().filter(|i| i.class.opcode == Op::Store) {
            i.operands
                .push(Operand::MemoryAccess(MemoryAccess::VOLATILE));
        }
        let mut module = module_with(b);
        assert_eq!(hoist_device_loads(&mut module), 0);
    }

    #[test]
    fn a_pointer_of_unknown_class_blocks() {
        let mut b = steel();
        b[7] = load(26, 99);
        let mut module = module_with(b);
        assert_eq!(hoist_device_loads(&mut module), 0);
    }

    #[test]
    fn a_block_with_no_workgroup_store_is_unchanged() {
        let original = module_with(copy(0, C1, PTR_SB)[..3].to_vec());
        let mut module = original.clone();
        assert_eq!(hoist_device_loads(&mut module), 0);
        assert_eq!(
            format!("{:?}", body(&module)),
            format!("{:?}", body(&original))
        );
    }

    #[test]
    fn a_translated_kernel_issues_its_device_loads_before_its_threadgroup_stores() {
        let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@tg = internal addrspace(3) global [128 x i32] undef, align 4

define void @k(ptr addrspace(1) noundef readonly "air-buffer-no-alias" %0, ptr addrspace(1) noundef "air-buffer-no-alias" %1, i32 noundef %2) local_unnamed_addr {
  %l = and i32 %2, 63
  %lx = zext i32 %l to i64
  %g = zext i32 %2 to i64
  %p0 = getelementptr inbounds i32, ptr addrspace(1) %0, i64 %g
  %v0 = load i32, ptr addrspace(1) %p0, align 4
  %t0 = getelementptr inbounds [128 x i32], ptr addrspace(3) @tg, i64 0, i64 %lx
  store i32 %v0, ptr addrspace(3) %t0, align 4
  %g1 = add i64 %g, 64
  %p1 = getelementptr inbounds i32, ptr addrspace(1) %0, i64 %g1
  %v1 = load i32, ptr addrspace(1) %p1, align 4
  %lx1 = add i64 %lx, 64
  %t1 = getelementptr inbounds [128 x i32], ptr addrspace(3) @tg, i64 0, i64 %lx1
  store i32 %v1, ptr addrspace(3) %t1, align 4
  %r = load i32, ptr addrspace(3) %t1, align 4
  %d = getelementptr inbounds i32, ptr addrspace(1) %1, i64 %g
  store i32 %r, ptr addrspace(1) %d, align 4
  ret void
}

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19, !20}
!18 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"src"}
!19 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"dst"}
!20 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
        let tmp = std::env::temp_dir().join(format!("metal2vulkan_ldhoist_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let spv = crate::translate_sanitized_native(ll, crate::passes::Stage::Kernel, &tmp)
            .expect("translate");
        let module = crate::spirv_module::load_bytes(&spv).expect("load translated module");
        let classes = Classes::of(&module);
        let (mut stores, mut loads, mut below) = (0, 0, 0);
        for block in module.functions.iter().flat_map(|f| &f.blocks) {
            let mut open = false;
            for kind in block.instructions.iter().map(|i| classes.kind(i)) {
                match kind {
                    Kind::WorkgroupStore => (open, stores) = (true, stores + 1),
                    Kind::Hard => open = false,
                    Kind::DeviceLoad => (loads, below) = (loads + 1, below + usize::from(open)),
                    _ => {}
                }
            }
        }
        eprintln!("ldhoist e2e: threadgroup stores {stores}, device loads {loads}, inside a store segment {below}");
        assert!(
            stores >= 2 && loads >= 2,
            "the kernel must reach the pass as two copies (stores {stores}, loads {loads})"
        );
        assert_eq!(
            below,
            usize::from(crate::env_vars::no_load_hoist()),
            "device loads below the first threadgroup store (knob off: 0, METAL2VULKAN_NO_LOAD_HOIST: 1)"
        );
        if std::process::Command::new("spirv-val")
            .arg("--version")
            .output()
            .is_ok()
        {
            crate::tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
        }
    }
}
