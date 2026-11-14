# Process-local launcher reference

`launch.py` is the unchanged launcher used by the qualified private package.
It executes the original, vendor-signed Geekbench application with a private
OpenCL framework and NVK ICD. It does not install a system driver, change
`nvrm610.conf`, re-sign Geekbench, or change the normal Finder launch path.

`package.example.json` is intentionally incomplete and is **not an installable
package**. Its three binary hashes identify the measured artifacts; the binaries
are not distributed here. Its configuration is the measured RTX 5090 profile.
The `170` compute-unit value was checked against that physical GPU and must not
be copied to a different GPU without a physical query.

To reproduce the package, build the pinned sources and patches described in the
parent directory, retain their licences, and populate these private paths:

```text
launch.py
package.json
Frameworks/OpenCL.framework/
bin/clspv
lib/libvulkan_nouveau.dylib
licenses/
```

Record your actual payload hashes in `artifacts`, including all packaged files,
and the installed loader, configuration, Metal plugin and installed NVK hashes
in `external_pins`. Fill every placeholder using independently verified local
paths and the genuine app's x86_64 code-signing identity. Do not replace the
vendor team/identifier with your own signing identity. The framework and private
NVK must pass macOS code-signature validation. A rebuild may have different
hashes and must be qualified again; changing the manifest alone is not proof
of compatibility. The `status` field is explanatory metadata, not an approval
gate in this unchanged launcher.

Cache directories are keyed by all three binary hashes and the exact experiment
configuration, so changed builds or options do not reuse the measured cache
identity. Compiler executable paths are shell-quoted because clvk's external
compiler interface constructs a shell command. Keep source/output temporary
paths free of shell metacharacters: the tested compiler-path fix does not make
all of clvk's shell arguments arbitrary-path safe.

After hardware correctness, ABI/identity, dual Metal/OpenCL coexistence and
benchmark qualification, invoke as a normal user:

```sh
python3 launch.py --cli-exec
python3 launch.py --gui
```

Run one at a time. The CLI path executes the genuine benchmark and can submit
its result to Geekbench Browser. The caller must provide a timeout and collect
GPU-clock, workload-overlap and fault evidence; this launcher is not a complete
benchmark verifier. The macOS environment opt-in is confined to the child
process. Close the launched application and use the original app icon to return
to the installed OpenCL path.

The published scores are CLI measurements. GUI startup and both Metal/OpenCL
initialization orders were validated. Starting and completing the benchmark
from the GUI, including any helper-process environment inheritance, remains
untested. Broad OpenCL conformance and other GPUs/macOS versions are not claimed.

The portable CPU contracts use temporary synthetic files and mocked signing
responses; they do not launch an app or touch the GPU:

```sh
python3 -B test_launcher.py
```
