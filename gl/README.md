# OpenGL for the releases that composite through OpenGL (10.6 to 10.13)

Status: under construction. The engine bundle forwards and records; nothing here renders yet and nothing here is
part of a package.

## What is in this folder

| path | what it is |
|---|---|
| `names/gld-<release>.txt` | the entry points OpenGL.framework resolves by name in a vendor renderer bundle, read from the framework binaries of that release |
| `names/gli-framework-<release>.txt` | the entry points OpenGL.framework resolves in an engine bundle |
| `names/gli-windowserver-<release>.txt` | the entry points the window server resolves in the engine bundle it opens by absolute path |
| `skeleton/gld_skeleton.c` | a renderer bundle that exports exactly the names of one release and declines to load |
| `skeleton/gli_skeleton.c` | an engine bundle that exports exactly the names of one release and declines to load |
| `engine/nm_gli.h` | the engine-level contract as read from the binaries: parameter numbers, argument blocks, drawable types, renderer-info and pixel-format layouts, with compile-time offset checks |
| `engine/nm_engine.c` | the engine bundle: forwards every entry to Apple's renamed engine at a fixed path. A default build reads nothing from the environment and writes no file; `NM_ENGINE_SYSLOG` records each call to the system log; `NM_ENGINE_TEST` (host tests only, never packaged) accepts a stand-in engine and a record file from two variables |
| `engine/build_engine.sh` | builds the bundle for x86_64 and i386 against the 10.6 SDK, compares exports with the measured lists, runs the host tests, checks imports |
| `glue/gen_dispatch.py` | generates the file that fills Apple's per-context function table for one release: a thunk into Mesa, one of our Apple-extension entries, or a refusal per slot |
| `glue/nm_apple_ext.c` | the Apple-extension entries Mesa lacks (fences, object tests, purgeable objects, texture range, flush-render) |
| `glue/build_glue.sh` | generates, compiles and link-checks the table for a release and runs the extension tests |
| `mesa/patch_zink_no_moltenvk.py`, `mesa/zink-no-moltenvk.patch` | lets zink build on this platform without the MoltenVK headers (script and the resulting diff) |
| `kext/nm_accel_contract.h` | the kernel accelerator's user-client contract: argument shapes of the 18 public surface methods and our private client |
| `kext/NMLegacyAccel.cpp` | accelerator stub that checks call shapes against the contract and answers unsupported; cannot match hardware |
| `kext/build_kext.sh` | builds the stub against the 10.7 and 10.11 kernel headers and checks its imports against each release |
| `tools/usersym.py` | import checker used by the build scripts |
| `build_skeleton.sh` | builds both skeletons for every release and both user architectures against the 10.6 SDK and compares exports |

## Building the skeletons

    bash build_skeleton.sh <path to MacOSX10.6.sdk> <out dir> [folder holding <release>.softrenderer_exports.txt]

The script prints one line per bundle and ends with `result: PASS` when every bundle exports exactly its name list.
It includes a control: a build with one name removed must be reported as exactly that name missing.

## Why two skeletons

OpenGL on these releases has two plug-in levels. The engine level owns the whole OpenGL API of a context (the public
function table in `OpenGL/gliDispatch.h`); the renderer level sits below Apple's own engine and is handed Apple's
private state structures. The design notes and the evidence for choosing between them are in the lane's map
document (`OPENGL-LEGACY.md`).
