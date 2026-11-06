use metal2vulkan::{disassemble, passes::{Stage, TransformOptions}};
use std::{fs, path::PathBuf, process::Command};

fn lower(ll: &str) -> Result<Vec<u8>, String> {
    metal2vulkan::translate_native_no_retry_constructed_with_options(ll, Stage::Kernel, TransformOptions::default())
}
fn fixture(name: &str, ty: &str, args: &str, values: &str) -> String {
    format!(r#"target triple = "spirv-unknown-vulkan1.2"
define void @main({args}) {{
entry:
  %r = call {ty} @{name}({values})
  ret void
}}
declare {ty} @{name}({args_types})
"#, args_types=args.split(", ").map(|a| a.rsplit_once(" %").unwrap().0).collect::<Vec<_>>().join(", "))
}
fn validate(bytes: &[u8], name: &str) -> String {
    let root = PathBuf::from(std::env::var("POWR_TEST_OUTPUT").expect("bounded test output directory"));
    fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{name}.spv"));fs::write(&path,bytes).unwrap();
    let val = std::env::var("POWR_SPIRV_VAL").expect("pinned independent validator required");
    let result=Command::new(val).args(["--target-env","vulkan1.2"]).arg(&path).output().expect("CPU validator");
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
    let asm=disassemble(bytes).unwrap();fs::write(root.join(format!("{name}.spvasm")),&asm).unwrap();asm
}
#[test]
fn native_powr_scalar_and_vector_lanes_are_componentwise_pow() {
    for (suffix,ty) in [("f32","float"),("v2f32","<2 x float>"),("v3f32","<3 x float>"),("v4f32","<4 x float>")] {
        let ll=fixture(&format!("air.native_powr.{suffix}"),ty,&format!("{ty} %x, {ty} %y"),&format!("{ty} %x, {ty} %y"));
        let bytes=lower(&ll).unwrap_or_else(|e|panic!("{suffix}: {e}"));let asm=validate(&bytes,suffix);
        assert_eq!(asm.matches(" Pow ").count(),1,"{asm}");
        assert!(!asm.contains(" FAbs ")&&!asm.contains("OpFNegate"),"native_powr must not use negative-base Metal pow sign path: {asm}");
        assert!(!asm.contains("OpFunctionCall"),"{asm}");
    }
}
#[test]
fn native_powr_rejects_malformed_signatures_and_unknown_families() {
    for (suffix,ty,args,values) in [
      ("v8f32","<8 x float>","<8 x float> %x, <8 x float> %y","<8 x float> %x, <8 x float> %y"),
      ("v16f32","<16 x float>","<16 x float> %x, <16 x float> %y","<16 x float> %x, <16 x float> %y"),
      ("f16","half","half %x, half %y","half %x, half %y"),
      ("i32","i32","i32 %x, i32 %y","i32 %x, i32 %y"),
      ("v4f32","float","float %x, float %y","float %x, float %y"),
      ("f32","<2 x float>","<2 x float> %x, <2 x float> %y","<2 x float> %x, <2 x float> %y"),
      ("f32","float","float %x","float %x"),
      ("f32","float","float %x, float %y, float %z","float %x, float %y, float %z"),
      ("f32","float","float %x, <2 x float> %y","float %x, <2 x float> %y"),
    ] {let ll=fixture(&format!("air.native_powr.{suffix}"),ty,args,values);assert!(lower(&ll).is_err(),"must reject {ll}");}
    // 2.7.2 rebase: upstream 2.7 deliberately lowers Metal's other native_* math (native_pow, native_sin, ...) with
    // fast_'s contract (air_intrinsics.rs, "Geekbench's resolve_radiance used native_powr"). Only an unknown family
    // stays refused here; the strict native_powr signature checks above are the local part kept on top of upstream.
    for name in ["air.native_powr_future.f32"] {
        let ll=fixture(name,"float","float %x, float %y","float %x, float %y");assert!(lower(&ll).is_err(),"must reject {name}");
    }
}
#[test]
fn existing_powr_and_pow_remain_distinct() {
    for name in ["air.powr.f32","air.fast_powr.f32","air.pow.f32"] {
      let ll=fixture(name,"float","float %x, float %y","float %x, float %y");
      let asm=validate(&lower(&ll).unwrap(),name);
      assert!(asm.contains(" Pow "),"{asm}");
      assert_eq!(asm.contains(" FAbs "),name=="air.pow.f32","{asm}");
    }
}
