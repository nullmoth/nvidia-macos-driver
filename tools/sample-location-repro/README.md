# OpenGL MSAA sample-location reproducer

Build on macOS:

```sh
clang -fobjc-arc -O1 -Wno-deprecated-declarations -framework Foundation -framework Metal -framework OpenGL opengl.m -o opengl-probe
./opengl-probe
```

The probe renders a green full-screen triangle to 4x and 8x multisampled RGBA8 framebuffers, resolves, and checks all 1024 pixels by CPU readback. It also observes the Metal render-pass sample-position getter without changing its return value. Expected: zero mismatched pixels. GL error alone is insufficient: the failing baseline reports GL_NO_ERROR despite missing output.

Do not use the optional plugin-path argument for release qualification: preloading another plugin can produce duplicate Objective-C classes. Run against a single installed driver for acceptance testing.

RTX 5070 Ti / macOS 15.8.1 / locally patched 1.7.0 baseline: both sizes had 1024 mismatched pixels. The installed candidate produced zero mismatches at both sizes without duplicate Objective-C class warnings. The same pre-existing local shader-translation workaround was retained in baseline and candidate. Runtime validation of a complete main-based build and MSAA depth/stencil qualification remain pending. This probe does not test edge coverage or arbitrary programmable sample patterns.
