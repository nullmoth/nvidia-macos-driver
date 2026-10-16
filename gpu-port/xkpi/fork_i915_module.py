# fork_i915_module.py <drm-kmod> <out>: i915_module.c with each init step's result printed. i915_init stops at the
# first step that returns non-zero, and a positive return ends it early as a success, so a step that refuses leaves
# i915 unregistered with nothing in the log (Intel iGPU uploads: "no driver took 8086:<id>" and no other line).
import pathlib, sys
src = pathlib.Path(sys.argv[1]) / "drivers/gpu/drm/i915/i915_module.c"
s = src.read_text()
anchor = "\t\terr = init_funcs[i].init();\n"
assert s.count(anchor) == 1
s = s.replace(anchor, anchor + "\t\tif (err != 0)\n"
              "\t\t\tprintf(\"NullMoth i915: init step %d of %d returned %d\\n\", i, (int)ARRAY_SIZE(init_funcs), err);\n", 1)
end = "\tinit_progress = i;\n"
assert s.count(end) == 1
s = s.replace(end, end + "\tprintf(\"NullMoth i915: %d of %d init steps ran\\n\", i, (int)ARRAY_SIZE(init_funcs));\n", 1)
pathlib.Path(sys.argv[2]).write_text(s)
print("i915_module.c fork applied")
