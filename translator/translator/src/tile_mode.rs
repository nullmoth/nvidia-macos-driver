//! Per-thread switches for shaders that touch tile memory (imageblocks, tile threadgroup memory).
//!
//! Vulkan has no tile memory, so a driver that emulates Metal's tile shading keeps an imageblock in storage images and
//! runs a tile function as an ordinary compute dispatch between two parts of a render pass. These switches tell the
//! translator which of those choices the driver made. They are thread-local, like the other per-call settings the
//! cdylib wrapper keeps, and default to off: a translation made without them is the translation made before they
//! existed.
//!
//! * [`BAND`]: imageblock planes live inside the storage-image band of the driver's descriptor set, not in the
//!   bindings the default layout reserves for them (which that set does not have).
//! * [`NO_INTERLOCK`]: the device has no fragment shader interlock, so a fragment shader that reads or writes an
//!   imageblock is not wrapped in one. The driver orders such draws itself.
//! * [`TILE_KERNEL`]: the kernel runs as a tile function inside a render pass, so an explicit-layout imageblock
//!   argument holds what the render pass's fragment shaders left in it and is written back when the kernel ends.
//! * [`PERSISTENT_TG`]: a tile function's `[[threadgroup(n)]]` arguments are slices of per-tile memory that lives for
//!   the whole render pass: they are loaded from it when the kernel starts and stored to it when the kernel ends.
use crate::reflect::{DescriptorBindingRange, DescriptorLayout};
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

pub const BAND: u32 = 1;
pub const NO_INTERLOCK: u32 = 2;
pub const TILE_KERNEL: u32 = 4;
pub const PERSISTENT_TG: u32 = 8;

/// First binding of the storage-image band the driver's descriptor set offers (`STORAGE_TEXTURE_BINDING_BASE`).
const STORAGE_BAND_START: u32 = 480;
/// The band holds `TEXTURE_ARGUMENT_COUNT` (128) bindings; the top [`PLANE_BINDINGS`] belong to imageblock planes.
const STORAGE_BAND_END: u32 = 480 + 128;
/// Implicit planes (8 attachments x 3 data rates) followed by 8 explicit-imageblock members.
pub const IMPLICIT_PLANE_BINDINGS: u32 = 24;
pub const EXPLICIT_PLANE_BINDINGS: u32 = 8;
pub const PLANE_BINDINGS: u32 = IMPLICIT_PLANE_BINDINGS + EXPLICIT_PLANE_BINDINGS;
/// First binding of the implicit planes: `IMPLICIT_PLANE_BASE + 3 * attachment + data_rate`.
pub const IMPLICIT_PLANE_BASE: u32 = STORAGE_BAND_END - PLANE_BINDINGS;
/// First binding of the explicit imageblock members: `EXPLICIT_PLANE_BASE + member`.
pub const EXPLICIT_PLANE_BASE: u32 = IMPLICIT_PLANE_BASE + IMPLICIT_PLANE_BINDINGS;
/// Binding the driver binds a tile function's persistent per-tile memory to (a storage buffer, descriptor-set binding
/// of the highest Metal buffer slot the driver reserves for itself).
pub const TILE_MEMORY_BUFFER_SLOT: u32 = 28;

thread_local! { static FLAGS: Cell<u32> = const { Cell::new(0) }; }
// Byte offset of each `[[threadgroup(n)]]` argument inside the tile's memory, for the translation under way.
thread_local! { static TG_OFFSETS: Cell<[u32; 32]> = const { Cell::new([0; 32]) }; }

pub fn set_tg_offsets(offsets: &[u32]) {
    let mut all = [0u32; 32];
    for (slot, value) in all.iter_mut().zip(offsets) {
        *slot = *value;
    }
    TG_OFFSETS.with(|o| o.set(all));
}

pub fn tg_offset(index: u32) -> u32 {
    TG_OFFSETS.with(|o| o.get()).get(index as usize).copied().unwrap_or(0)
}
/// Switches every translation in the process has, whichever thread makes it: what the device is (no interlock, planes in
/// the storage band), as opposed to what one call is (a tile function).
static DEFAULTS: AtomicU32 = AtomicU32::new(0);

/// Set the switches of this thread's next translations. A call that is not a tile function sets 0 afterwards.
pub fn set_flags(flags: u32) {
    FLAGS.with(|f| f.set(flags));
}

pub fn set_defaults(flags: u32) {
    DEFAULTS.store(flags, Ordering::Relaxed);
}

pub fn flags() -> u32 {
    FLAGS.with(|f| f.get()) | DEFAULTS.load(Ordering::Relaxed)
}

pub fn has(flag: u32) -> bool {
    flags() & flag != 0
}

/// True when the AIR text names an imageblock anywhere: an `air.imageblock` argument, an implicit imageblock
/// intrinsic, or the `_imageblock_base` argument type. Only such shaders get the remapped layout.
pub fn mentions_imageblock(ll: &str) -> bool {
    ll.contains("imageblock")
}

/// The default layout with the imageblock planes moved into the top of the storage-image band and the storage
/// textures given what is left below them.
pub fn band_layout(base: DescriptorLayout) -> DescriptorLayout {
    DescriptorLayout {
        storage_textures: DescriptorBindingRange { start: STORAGE_BAND_START, end: IMPLICIT_PLANE_BASE },
        imageblocks: DescriptorBindingRange { start: IMPLICIT_PLANE_BASE, end: EXPLICIT_PLANE_BASE },
        fragment_imageblocks: DescriptorBindingRange { start: EXPLICIT_PLANE_BASE, end: STORAGE_BAND_END },
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_layout_is_a_valid_layout_inside_the_storage_band() {
        let layout = band_layout(DescriptorLayout::default());
        layout.validate().expect("band layout must validate");
        assert_eq!(layout.imageblock_binding(0, 0), Some(IMPLICIT_PLANE_BASE));
        assert_eq!(layout.imageblock_binding(7, 2), Some(IMPLICIT_PLANE_BASE + 23));
        assert_eq!(layout.imageblock_binding(8, 0), None);
        assert_eq!(layout.fragment_imageblock_binding(0), Some(EXPLICIT_PLANE_BASE));
        assert_eq!(layout.fragment_imageblock_binding(7), Some(STORAGE_BAND_END - 1));
        assert_eq!(layout.fragment_imageblock_binding(8), None);
        assert_eq!(layout.storage_texture_binding(0), Some(480));
        assert_eq!(layout.storage_texture_binding(IMPLICIT_PLANE_BASE - 480), None);
    }

    #[test]
    fn call_flags_are_per_thread_and_device_defaults_are_shared() {
        // one test touches the process-wide defaults, so no other test sees them change
        assert_eq!(flags(), 0);
        set_flags(TILE_KERNEL);
        assert!(has(TILE_KERNEL) && !has(BAND));
        std::thread::spawn(|| assert_eq!(flags(), 0)).join().unwrap();
        set_defaults(BAND | NO_INTERLOCK);
        assert!(has(BAND) && has(NO_INTERLOCK) && has(TILE_KERNEL));
        std::thread::spawn(|| assert_eq!(flags(), BAND | NO_INTERLOCK)).join().unwrap();
        set_defaults(0);
        set_flags(0);
    }

    #[test]
    fn only_imageblock_modules_are_recognised() {
        assert!(mentions_imageblock("call @air.load.implicit_imageblock.v4f16(i32 0"));
        assert!(mentions_imageblock("%\"struct.metal::_imageblock_base\" = type"));
        assert!(!mentions_imageblock("define void @k() { ret void }"));
    }
}
