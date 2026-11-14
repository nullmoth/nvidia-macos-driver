//! Tile shading: what a driver that emulates tile memory gets from `metal2vulkan::tile_mode`.
//!
//! The fixtures are the AIR Apple's compiler produces for the tile oracle shaders (tests/tile/tile.metal in the driver
//! repository): a tile function over an implicit imageblock, one over an explicit imageblock, tile functions with
//! threadgroup memory, and the fragment shaders that fill an explicit imageblock.
//!
//! Every switch is thread-local here, so tests that set one do not disturb a test that does not.

use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::tile_mode as tile;
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

const TILE_AVG: &str = include_str!("fixtures/tile/tile_avg.ll");
const TILE_IDS: &str = include_str!("fixtures/tile/tile_ids.ll");
const TILE_RESOLVE: &str = include_str!("fixtures/tile/tile_resolve.ll");
const TILE_TG_WRITE: &str = include_str!("fixtures/tile/tile_tg_write.ll");
const TILE_TG_HALF: &str = include_str!("fixtures/tile/tile_tg_half.ll");
const FS_GB2_ACC: &str = include_str!("fixtures/tile/fs_gb2_acc.ll");
const TILE_GB2_DUMP: &str = include_str!("fixtures/tile/tile_gb2_dump.ll");
const FS_GB_INIT: &str = include_str!("fixtures/tile/fs_gb_init.ll");
const FS_GB_ACC: &str = include_str!("fixtures/tile/fs_gb_acc.ll");

/// Translate as the driver does: the switches set, the band layout for a module that names an imageblock.
fn translate(ll: &str, stage: Stage, flags: u32) -> Result<String, String> {
    tile::set_flags(flags);
    let mut options = TransformOptions::default();
    if flags & tile::BAND != 0 && tile::mentions_imageblock(ll) {
        options.descriptor_layout = tile::band_layout(options.descriptor_layout);
    }
    let result = translate_native_no_retry_constructed_with_options(ll, stage, options);
    tile::set_flags(0);
    // the disassembly is column aligned and names nothing: one space between words makes it greppable
    disassemble(&result?).map(|asm| {
        asm.lines().map(|line| line.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n")
    })
}

const DEVICE: u32 = tile::BAND | tile::NO_INTERLOCK;
const TILE_FUNCTION: u32 = DEVICE | tile::TILE_KERNEL;

fn count(asm: &str, needle: &str) -> usize {
    asm.matches(needle).count()
}

#[test]
fn without_the_switches_translation_is_what_it_was() {
    let asm = translate(TILE_AVG, Stage::Kernel, 0).unwrap();
    // the default layout: implicit planes at 200 + 3 * attachment, declared in the data's own format, arrayed
    assert!(asm.contains("Binding 200"), "{asm}");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(!asm.contains("Offset 60"), "{asm}");
    let frag = translate(FS_GB_INIT, Stage::Fragment, 0).unwrap();
    assert!(frag.contains("PixelInterlockOrderedEXT"), "{frag}");
}

#[test]
fn a_tile_function_binds_the_render_target_as_a_formatless_plane_in_the_storage_band() {
    let asm = translate(TILE_AVG, Stage::Kernel, TILE_FUNCTION).unwrap();
    // 576 = the top of the storage-image band, minus the 32 bindings the planes take, plus 3 * attachment 0
    assert!(asm.contains("Binding 576"), "{asm}");
    assert!(!asm.contains("Binding 200"), "{asm}");
    // no format: the plane is the attachment itself, in whatever format the render pass gave it
    assert!(asm.contains("2D 0 0 0 2 Unknown"), "{asm}");
    assert!(asm.contains("OpCapability StorageImageReadWithoutFormat"), "{asm}");
    assert!(asm.contains("OpCapability StorageImageWriteWithoutFormat"), "{asm}");
}

#[test]
fn a_tile_function_addresses_the_whole_render_target_by_adding_its_tile_origin() {
    let asm = translate(TILE_IDS, Stage::Kernel, TILE_FUNCTION).unwrap();
    // the push block is the dispatch region (12 words) and the four words of the tile block, and the function reads
    // the tile coordinate (WorkgroupId plus the group base, members 6 and 7) times the tile size (members 12 and 13)
    assert!(asm.contains("12 Offset 48") && asm.contains("15 Offset 60"), "{asm}");
    for member in [6, 7, 12, 13] {
        assert!(asm.contains(&format!("OpConstant %4 {member}")) || asm.contains(&format!(" OpConstant %")) && asm.contains(&format!(" {member}\n")), "{member}\n{asm}");
    }
    assert!(asm.contains("OpImageWrite"), "{asm}");
}

#[test]
fn a_fragment_shader_with_an_imageblock_needs_no_interlock_on_a_device_without_it() {
    let asm = translate(FS_GB_ACC, Stage::Fragment, DEVICE).unwrap();
    assert!(!asm.contains("Interlock"), "{asm}");
    // members half4 and half: planes 600 and 601, a read and a write of each
    assert!(asm.contains("Binding 600") && asm.contains("Binding 601"), "{asm}");
    assert!(count(&asm, "OpImageRead") == 2 && count(&asm, "OpImageWrite") == 2, "{asm}");
    let init = translate(FS_GB_INIT, Stage::Fragment, DEVICE).unwrap();
    assert!(!init.contains("Interlock") && count(&init, "OpImageWrite") == 2, "{init}");
}

#[test]
fn an_explicit_imageblock_of_a_tile_function_is_staged_through_the_fragment_planes() {
    let asm = translate(TILE_RESOLVE, Stage::Kernel, TILE_FUNCTION).unwrap();
    // read from planes 600 and 601 into the threadgroup cells, written back when the function ends
    assert!(asm.contains("Binding 600") && asm.contains("Binding 601"), "{asm}");
    assert!(count(&asm, "OpImageRead") == 2, "{asm}");
    assert!(count(&asm, "OpImageWrite") >= 3, "{asm}");
    assert_eq!(count(&asm, "OpControlBarrier"), 2, "{asm}");
    assert!(asm.contains("Workgroup"), "{asm}");
    // not a tile function: the imageblock is the compute kernel's own scratch, untouched by planes
    let plain = translate(TILE_RESOLVE, Stage::Kernel, DEVICE).unwrap();
    assert!(!plain.contains("Binding 600"), "{plain}");
}

#[test]
fn tile_memory_that_outlives_the_dispatch_is_loaded_and_stored_around_the_body() {
    tile::set_tg_offsets(&[1024]);
    let asm = translate(TILE_TG_WRITE, Stage::Kernel, TILE_FUNCTION | tile::PERSISTENT_TG).unwrap();
    tile::set_tg_offsets(&[]);
    // two loops over the elements (in, out), the tile memory buffer at the slot the driver reserves, a barrier between
    // the body and each copy
    assert_eq!(count(&asm, "OpLoopMerge"), 2, "{asm}");
    assert!(asm.contains(&format!("Binding {}", tile::TILE_MEMORY_BUFFER_SLOT)), "{asm}");
    assert!(asm.contains("OpTypePointer StorageBuffer"), "{asm}");
    // the offset of argument 0 in the tile's memory, 1024 bytes = 256 words, is in the address arithmetic
    assert!(asm.lines().any(|line| line.contains("OpConstant") && line.ends_with(" 256")), "{asm}");
    // the same function without the switch has neither
    let plain = translate(TILE_TG_WRITE, Stage::Kernel, TILE_FUNCTION).unwrap();
    assert_eq!(count(&plain, "OpLoopMerge"), 0, "{plain}");
}

#[test]
fn narrow_scalars_of_tile_memory_take_a_word_each() {
    let asm = translate(TILE_TG_HALF, Stage::Kernel, TILE_FUNCTION | tile::PERSISTENT_TG).unwrap();
    assert_eq!(count(&asm, "OpLoopMerge"), 2, "{asm}");
    // half -> its 16 bits -> a word, and back
    assert!(asm.contains("OpTypeInt 16 0"), "{asm}");
    assert!(count(&asm, "OpUConvert") >= 2, "{asm}");
}

#[test]
fn every_member_type_has_a_plane_for_fragment_shaders_and_tile_functions() {
    // float4, int, uint, uchar4, uchar: planes 600..604, each read and written by the fragment shader
    let frag = translate(FS_GB2_ACC, Stage::Fragment, DEVICE).unwrap();
    for member in 0..5 {
        assert!(frag.contains(&format!("Binding {}", 600 + member)), "member {member}
{frag}");
    }
    assert_eq!(count(&frag, "OpImageRead"), 5, "{frag}");
    // same width as its texel component: a bit copy, never a conversion between equal widths
    assert!(count(&frag, "OpBitcast") >= 1, "{frag}");
    let kernel = translate(TILE_GB2_DUMP, Stage::Kernel, TILE_FUNCTION).unwrap();
    for member in 0..5 {
        assert!(kernel.contains(&format!("Binding {}", 600 + member)), "member {member}
{kernel}");
    }
}
