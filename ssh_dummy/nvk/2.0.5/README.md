# NVK changes in 2.0.5

The Vulkan driver shipped through 2.0.4 was a debug build: Mesa's `assert()`s were live, and one of them
(`desc_index > 0` in `nvk_descriptor_set.c`) stopped Safari and `mediaanalysisd` on every GPU series. 2.0.5 builds
the same tree as a release build (`-Dbuildtype=release -Db_ndebug=true`); its C exports are the same as 2.0.4's.

Changes on top of the tree 2.0.4 shipped from:

- The native-shader dispatch (v2/v3) and its handle exports, as shipped in 2.0.0, are now part of the tree itself.
- The upload queue picks the chip's own async copy engine before Blackwell: the first copy engine that is not the
  graphics engine's partner, named in NVB0B5 VERSION_1 allocation parameters, as NVIDIA's driver does. Blackwell
  keeps COPY4. Before this, uploads on Turing, Ampere and Ada fell back onto the graphics channel.
- A shader that writes the stencil reference no longer reaches an unreachable branch in NAK: the write is dropped with
  a warning and the pipeline's stencil reference is used (GH #193).
