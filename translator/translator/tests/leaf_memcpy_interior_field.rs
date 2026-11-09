use metal2vulkan::passes::{Stage, TransformOptions};

const AIR: &str = include_str!("fixtures/leaf_memcpy/sp1.ll");

fn translate() -> Result<Vec<u8>, String> {
    metal2vulkan::translate_native_no_retry_constructed_with_options(
        AIR,
        Stage::Kernel,
        TransformOptions::default(),
    )
}

#[test]
fn interior_field_memcpy_into_a_differently_typed_local_translates_and_the_control_fires() {
    assert!(
        AIR.contains("call void @llvm.memcpy.p0i8.p1i8.i64")
            && AIR.contains("alloca { i32, float, float, float }"),
        "the fixture no longer carries the interior-field memcpy shape this arm exists for"
    );
    let spv = match translate() {
        Ok(spv) => spv,
        Err(e) => panic!("the clean-room calculate_descriptors shape was refused: {e}"),
    };
    assert_eq!(
        &spv[..4],
        &0x0723_0203u32.to_le_bytes(),
        "output does not start with the SPIR-V magic number"
    );
    match std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
    {
        Ok(_) => {
            let module =
                std::env::temp_dir().join(format!("leaf_memcpy_sp1_{}.spv", std::process::id()));
            std::fs::write(&module, &spv).expect("write the module for spirv-val");
            let out = std::process::Command::new("spirv-val")
                .arg(&module)
                .output()
                .expect("run spirv-val");
            let _ = std::fs::remove_file(&module);
            assert!(
                out.status.success(),
                "spirv-val rejected the module: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => eprintln!("spirv-val not runnable ({e}) - the independent check did NOT run"),
    }
    std::env::set_var("METAL2VULKAN_NO_LEAF_MEMCPY", "1");
    let control = translate();
    std::env::remove_var("METAL2VULKAN_NO_LEAF_MEMCPY");
    match control {
        Ok(_) => panic!(
            "the control did not fire: the module translated with METAL2VULKAN_NO_LEAF_MEMCPY set"
        ),
        Err(e) => assert!(
            e.contains("OpFunctionCall") || e.contains("unresolved external shader function: llvm.memcpy"),
            "the control was refused for another reason: {e}"
        ),
    }
}
