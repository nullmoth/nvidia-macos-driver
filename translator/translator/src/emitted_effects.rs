use crate::fxhash::HashMap;
use crate::spirv_module::Module;
use crate::spirv_operand::Operand;
use spirv::{Op, StorageClass, Word};

const AIR_OBSERVABLE_WRITE_MARKERS: &[&str] = &[
    "@air.write_texture",
    "@air.write_imageblock_slice_to_texture",
    "@air.atomic_fetch_",
    "@air.atomic_exchange_explicit_texture",
    "@air.atomic_store_explicit_texture",
    "@air.atomic_compare_exchange_weak_explicit_texture",
    "@air.store.device_coherent",
    "@air.store.system_coherent",
    "@llvm.agx3.store.with.emask.global",
    "@llvm.memcpy.p1.",
    "@llvm.memset.p1.",
];

pub(crate) fn air_declares_an_observable_write(air_ll: &str) -> bool {
    air_ll.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with("declare ") {
            return false;
        }
        if line.starts_with("store ") && line.contains("ptr addrspace(1)") {
            return true;
        }
        AIR_OBSERVABLE_WRITE_MARKERS
            .iter()
            .any(|marker| line.contains(marker))
            || calls_a_device_atomic_write(line)
    })
}

fn calls_a_device_atomic_write(line: &str) -> bool {
    const MARKER: &str = "@air.atomic.global.";
    line.match_indices(MARKER)
        .any(|(start, _)| !line[start + MARKER.len()..].starts_with("load"))
}

pub(crate) fn air_declares_an_imageblock_write(air_ll: &str) -> bool {
    air_ll.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with("declare ") {
            return false;
        }
        (line.starts_with("store ") && line.contains("ptr addrspace(4)"))
            || line.contains("@llvm.memcpy.p4.")
            || line.contains("@llvm.memset.p4.")
    })
}

pub(crate) fn module_has_an_observable_effect(module: &Module) -> bool {
    let mut pointer_storage: HashMap<Word, StorageClass> = HashMap::default();
    for instruction in module.types_global_values.iter() {
        if instruction.class.opcode != Op::TypePointer {
            continue;
        }
        if let (Some(id), Some(Operand::StorageClass(storage))) =
            (instruction.result_id, instruction.operands.first())
        {
            pointer_storage.insert(id, *storage);
        }
    }
    let mut value_storage: HashMap<Word, StorageClass> = HashMap::default();
    for instruction in module.all_inst_iter() {
        if let (Some(id), Some(result_type)) = (instruction.result_id, instruction.result_type) {
            if let Some(storage) = pointer_storage.get(&result_type) {
                value_storage.insert(id, *storage);
            }
        }
    }
    module.all_inst_iter().any(|instruction| {
        let opcode = instruction.class.opcode;
        if opcode == Op::ImageWrite {
            return true;
        }
        if !matches!(opcode, Op::Store | Op::CopyMemory | Op::AtomicStore)
            && !is_atomic_read_modify_write(opcode)
        {
            return false;
        }
        let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
            return true;
        };
        !matches!(
            value_storage.get(pointer),
            Some(StorageClass::Function | StorageClass::Private | StorageClass::Workgroup)
        )
    })
}

fn is_atomic_read_modify_write(opcode: Op) -> bool {
    matches!(
        opcode,
        Op::AtomicExchange
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
            | Op::AtomicFMaxEXT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_store_is_an_observable_write() {
        assert!(air_declares_an_observable_write(
            "  store i32 %v, ptr addrspace(1) %p, align 4\n"
        ));
        assert!(air_declares_an_observable_write(
            "  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %t)\n"
        ));
    }

    #[test]
    fn a_device_write_spelled_as_an_intrinsic_is_an_observable_write() {
        for call in [
            "  %r = call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %p, i32 1)\n",
            "  call void @air.atomic.global.store.u.i32(ptr addrspace(1) %p, i32 1)\n",
            "  %r = call i32 @air.atomic.global.max.s.i32(ptr addrspace(1) %p, i32 1)\n",
            "  call void @air.write_imageblock_slice_to_texture_2d.i16.f16(ptr addrspace(1) %t)\n",
            "  call void @air.store.device_coherent.i32(ptr addrspace(1) %p, i32 1)\n",
            "  call void @air.store.system_coherent.volatile.i32(ptr addrspace(1) %p, i32 1)\n",
            "  %r = call i32 @air.atomic_fetch_max_explicit_texture_2d.u.i32(ptr addrspace(1) %t)\n",
        ] {
            assert!(
                air_declares_an_observable_write(call),
                "{call} names a write the dispatch's caller owns"
            );
        }
    }

    #[test]
    fn an_llvm_intrinsic_write_is_read_from_its_destination_address_space() {
        assert!(air_declares_an_observable_write(
            "  tail call void @llvm.agx3.store.with.emask.global.v4i8(ptr addrspace(1) %p, \
             <4 x i8> %v, i16 15, i16 15, i16 1)\n"
        ));
        assert!(air_declares_an_observable_write(
            "  call void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) %dst, ptr %src, i64 16, i1 false)\n"
        ));
        assert!(air_declares_an_observable_write(
            "  call void @llvm.memset.p1.i64(ptr addrspace(1) %dst, i8 0, i64 16, i1 false)\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  call void @llvm.memcpy.p0.p1.i64(ptr %dst, ptr addrspace(1) %src, i64 16, i1 false)\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  tail call void @llvm.agx3.store.with.emask.local.v4i8(ptr addrspace(3) %p, \
             <4 x i8> %v, i16 15, i16 15, i16 1)\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  call void @llvm.memset.p4.i64(ptr addrspace(4) %dst, i8 0, i64 8, i1 false)\n"
        ));
        assert!(air_declares_an_imageblock_write(
            "  call void @llvm.memset.p4.i64(ptr addrspace(4) %dst, i8 0, i64 8, i1 false)\n"
        ));
    }

    #[test]
    fn a_device_atomic_load_is_not_an_observable_write() {
        assert!(!air_declares_an_observable_write(
            "  %r = call i32 @air.atomic.global.load.u.i32(ptr addrspace(1) %p)\n"
        ));
        assert!(air_declares_an_observable_write(
            "  %r = call i32 @air.atomic.global.load.u.i32(ptr addrspace(1) %p)\n  \
             %s = call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %p, i32 1)\n"
        ));
    }

    #[test]
    fn a_local_atomic_is_not_an_observable_write() {
        assert!(!air_declares_an_observable_write(
            "  %r = call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) %p, i32 1)\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  call void @air.store.implicit_imageblock.v4f16(ptr addrspace(4) %p)\n"
        ));
    }

    #[test]
    fn an_imageblock_store_is_recognised_by_its_address_space() {
        assert!(air_declares_an_imageblock_write(
            "  store <4 x half> zeroinitializer, ptr addrspace(4) %5, align 8\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  store <4 x half> zeroinitializer, ptr addrspace(4) %5, align 8\n"
        ));
        assert!(!air_declares_an_imageblock_write(
            "  store i32 %v, ptr addrspace(1) %p, align 4\n"
        ));
        assert!(!air_declares_an_imageblock_write(
            "  %v = load <4 x half>, ptr addrspace(4) %p, align 8\n"
        ));
    }

    #[test]
    fn a_threadgroup_or_stack_store_is_not_an_observable_write() {
        assert!(!air_declares_an_observable_write(
            "  store i32 %v, ptr addrspace(3) %p, align 4\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  store i32 %v, ptr %alloca, align 4\n"
        ));
        assert!(!air_declares_an_observable_write(
            "  %v = load i32, ptr addrspace(1) %p, align 4\n"
        ));
    }
}
