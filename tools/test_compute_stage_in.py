#!/usr/bin/env python3
"""Run stage-input and workgroup regressions against an explicit isolated plugin.

Requires a release NVMTLDriver with its sibling translator/support libraries,
an NVK library directory, and a working NVIDIA accelerator. Does not install.
Logs, cache files and GPU readback results are kept in a new --output directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from test_translation_resource_reads import translate

ROOT = Path(__file__).resolve().parents[1]


def build(source, output, bundle=False):
    command = ["xcrun", "clang", "-arch", "x86_64", "-mmacosx-version-min=15.0",
               "-fobjc-arc", "-O1", str(ROOT / "tools" / source),
               "-framework", "Foundation", "-framework", "Metal", "-o", str(output)]
    if bundle:
        command += ["-dynamiclib", "-framework", "IOKit"]
    subprocess.run(command, check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", str(output)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plugin", required=True, type=Path)
    parser.add_argument("--nvk-directory", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    plugin = args.plugin.resolve()
    library = plugin.parent / "libnvmtl_translate.dylib"
    if not plugin.is_file() or not library.is_file() or not args.nvk_directory.is_dir():
        parser.error("plugin, sibling translator, and NVK directory must exist")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    interposer = output / "isolate-device.dylib"
    build("isolate_metal_device.m", interposer, bundle=True)
    binaries = {}
    for key, source in {
        "stage": "oracle_compute_stage_in.m",
        "variants": "oracle_compute_variants.m",
        "shared": "oracle_workgroup_byte_views.m",
        "arrays": "test_unknown_array_storage.m",
    }.items():
        binaries[key] = output / key
        build(source, binaries[key])
    fixture = ROOT / "translator/wrapper/tests/fixtures/workgroup_byte_record.ll"
    spv = output / "workgroup.spv"
    spv.write_bytes(translate(library, fixture, "kernel"))
    subprocess.run(["spirv-val", "--target-env", "vulkan1.2", str(spv)], check=True)
    report = {"plugin": str(plugin), "translator": str(library),
              "hashes": {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                         for p in (plugin, library)}, "runs": []}
    for name, key, warm in [("variants-cold", "variants", False),
                            ("variants-warm", "variants", True),
                            ("indexed-stage", "stage", False),
                            ("workgroup-record", "shared", False),
                            ("array-storage", "arrays", False)]:
        run = output / name
        run.mkdir()
        if warm:
            for directory in ("aircache", "spvcache"):
                shutil.copytree(output / "variants-cold" / directory, run / directory)
        else:
            (run / "aircache").mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith(
            ("NVMTL_", "NVK_", "NAK_", "DYLD_", "METAL2VULKAN_", "ISOLATED_"))}
        env.update(NVMTL_PROCESS_ENV="1", NVMTL_AIRCACHE=str(run / "aircache"),
                   NVMTL_LOG_PATH=str(run / "driver.log"), DYLD_PRINT_LIBRARIES="1",
                   DYLD_LIBRARY_PATH=str(args.nvk_directory.resolve()) + ":" + str(plugin.parent),
                   DYLD_INSERT_LIBRARIES=str(interposer), ISOLATED_NVMTL_PLUGIN=str(plugin))
        command = [str(binaries[key])]
        if key == "shared":
            command += [str(spv), str(fixture)]
        with (run / "run.log").open("w") as log:
            result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=120)
        text = (run / "run.log").read_text(errors="replace")
        driver = (run / "driver.log").read_text(errors="replace") if (run / "driver.log").exists() else ""
        errors = re.findall(r"^.*(?:DEVICE_LOST|FAULT_PTE|FAULT_PDE|Compile Error|specialization REFUSED).*$",
                            text + "\n" + driver, re.MULTILINE)
        row = dict(name=name, rc=result.returncode,
                   plugin_loaded="ISOLATED_DEVICE: " + str(plugin) in text,
                   translator_loaded=str(library.resolve()) in text,
                   variant_hits=driver.count("variant cache: hit"),
                   variant_misses=driver.count("variant cache: miss"),
                   pass_markers=len(re.findall(r"^PASS\b", text, re.MULTILINE)), errors=errors)
        report["runs"].append(row)
        (output / "summary.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(row), flush=True)
        if result.returncode or errors or not row["plugin_loaded"] or not row["translator_loaded"]:
            raise RuntimeError(f"isolated regression failed: {run}")
        if warm and row["variant_hits"] < 8:
            raise RuntimeError(f"persistent variant cache was not exercised: {run}")
    print("All isolated compute regressions passed.")


if __name__ == "__main__":
    main()
