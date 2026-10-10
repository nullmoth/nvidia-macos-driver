# Shader reflection for consumers

When metal2vulkan translates Metal AIR or sanitized LLVM IR, it has two authoritative sources of
consumer information:

- AIR metadata plus the translator's shared descriptor ABI define resources and stage interfaces.
- Read-only analysis of the final constructed SPIR-V module defines conservative buffer byte
  footprints. It consumes the same owned module that supplies the returned bytes; it does not parse
  serialized output back into a second module.

Both are exposed as [`ShaderReflection`](../src/reflect/mod.rs), so a host does not need to reparse
AIR or build a second general-purpose SPIR-V reflection path. Reflection remains **byte-neutral**:
`translate_reflected` produces the same SPIR-V bytes as `translate` for identical input, stage, and
options.

For an end-to-end integration recipe, start with [How to translate and integrate a shader](HOWTO.md).

## Getting reflection

### Library

```rust
use metal2vulkan::passes::Stage;
use metal2vulkan::reflect::ShaderReflection;
use std::path::Path;

fn convert(
    air_or_ll_path: &str,
    caller_owned_scratch: &Path,
) -> Result<(Vec<u8>, ShaderReflection), String> {
    metal2vulkan::translate_reflected(
        air_or_ll_path,
        Stage::Kernel,
        caller_owned_scratch,
    )
}
```

Other entry points:

| API | Use when |
|---|---|
| `translate_reflected` / `translate_reflected_with_options` | Path to `.air` or `.ll` + stage |
| `translate_sanitized_native_reflected` | You already have sanitized LLVM IR text and `TransformOptions` |
| `reflect_sanitized` | You need metadata for link-time tooling even when executable translation is not yet possible; buffer footprints remain absent, and the `BufferAddressTable` binding is predicted rather than read off a module (it can be reported for a module that declares none, or missed for one that does) |
| `translate_sanitized_native_specialized_reflected_with_options` / `..._linked_specialized_reflected_with_options` | You are BINDING a function-constant-specialized (and possibly linked) module and need the reflection of that exact module |
| `reflect_sanitized_specialized` | You need metadata for exact function-constant values that can select resources or outputs; buffer footprints remain absent, and the texture shape, `BufferAddressTable` binding, and synthesized placeholder descriptors are the same AIR-text approximations `reflect_sanitized` documents above |
| `ShaderReflection::from_{fragment,vertex,kernel}` | You already have `meta::{Frag,Vert,Kern}Meta`; IR-derived fields and buffer footprints remain absent |

Path-based optional transforms use `translate_reflected_with_options`. The sanitized reflected
entry point takes `TransformOptions` directly.

Scratch space belongs to the caller. Give concurrent translations separate directories and remove
them on all return paths. The compiled `translate_reflected` example demonstrates that lifecycle.

### CLI JSON dump

Build with the `serde` feature and pass `--emit-meta`:

```sh
cargo install metal2vulkan --features serde
metal2vulkan in.ll out.spv --stage kernel --emit-meta out.json
```

`--emit-meta` is not supported for `--stage passthrough` (no Metal interface metadata).

`out.json` is a pretty-printed `ShaderReflection` with its schema version in `reflection_version`.

### Persist with `serde`

```toml
metal2vulkan = { version = "0.1", features = ["serde"] }
```

`ShaderReflection` and its nested types derive `Serialize`/`Deserialize` under that feature. The
current `REFLECTION_VERSION` is `49`. Serialized Rust enums use serde's externally tagged default:
unit variants are strings (for example `"Unbounded"`), while data variants are objects (for example
`{ "Object": { "bytes": 288 } }`). Optional fields serialize as `null`.

Persist SPIR-V and reflection together. Invalidate both when `reflection_version` changes. If a
cache may contain older schemas, inspect `reflection_version` as generic JSON—or store it in the
cache envelope—before deserializing the strongly typed current structure, because a newly required
field can make old JSON fail deserialization.

## Descriptor ABI (binding map)

By default, every descriptor-backed Metal-facing resource uses **descriptor set 0**
(`RESOURCE_DESCRIPTOR_SET`). Most default bindings are a fixed base plus Metal resource index `n`:

| Metal resource | Kind | SPIR-V binding |
|---|---|---|
| `[[buffer(n)]]` (device / constant) | `Buffer` | `BUFFER_BINDING_BASE + n` → **`n`** (buffer band `0`–`31`) |
| `[[buffer(n)]]` (threadgroup) | `ThreadgroupBuffer` | **no descriptor** (`descriptor: None`) |
| Kernel `[[stage_in]]` attribute | `KernelStageInput` | first free buffer binding; AIR location is `stage_input_location` |
| sampled `[[texture(n)]]` | `Texture` / sampled `TextureArray` | `TEXTURE_BINDING_BASE + n` → **`32 + n`** (sampled-texture band `32`–`159`) |
| writable `[[texture(n)]]` | `StorageImage` / storage `TextureArray` | `STORAGE_TEXTURE_BINDING_BASE + n` → **`480 + n`** (storage-texture band `480`–`607`) |
| `[[sampler(n)]]` | `Sampler` | `SAMPLER_BINDING_BASE + n` → **`160 + n`** (runtime indices `0`–`15`) |
| AIR `constexpr sampler` | `StaticSampler` | first free binding in **`160..192`** |
| `[[color(n)]]` (framebuffer fetch) | `ColorInput` | `COLOR_INPUT_BINDING_BASE + n` → **`192 + n`** (color band `192`–`199`) |
| Implicit imageblock attachment `n`, data rate `r` | `implicit_imageblock_attachments` | `IMAGEBLOCK_BINDING_BASE + 3*n + r` → **`200 + 3*n + r`** |
| Custom fragment imageblock master member `n` | `fragment_imageblock.members[n]` | `FRAGMENT_IMAGEBLOCK_BINDING_BASE + n` → **`224 + n`** when projected |
| Acceleration-structure shadow buffer | `AccelerationStructureShadow` | selected buffer range at Metal index `n` |
| Primitive acceleration structure | `PrimitiveAccelerationStructure` | selected buffer range at Metal index `n`; descriptor only when AIR intersection lowering consumes its geometry shadow |
| Authored visible/intersection function table | `VisibleFunctionTable` / `IntersectionFunctionTable` | **no descriptor** (`descriptor: None`); `metal_index` and `param_index` identify static linkage |
| Texture embedded in argument buffer | `EmbeddedArgBufferTexture` | selected sampled- or storage-texture range at `synthetic_index` |
| Device buffer embedded in argument buffer | `EmbeddedArgBufferBuffer` | **no descriptor**; owner field contains its Vulkan device address |
| Native ray instance user-ID table | `RayInstanceUserIdTable` | reserved binding **31** in the selected descriptor set; tightly packed `uint` entries indexed by Vulkan instance index, populated with full Metal user instance IDs. No Metal argument corresponds to it (`param_index` is absent). A real resource at binding 31 is rejected. This internal binding stays 31 even under a shifted descriptor layout |
| Synthesized direct-buffer address table | `BufferAddressTable` | first free binding in the selected synthetic range (default starts at `640`); one `u64` address per Metal buffer slot. Reflected translation reads it off the finished module; `reflect_sanitized` predicts it with the emitter's device-address predicate and can differ in either direction |
| Placeholder image for `air.get_null_texture_*()` | `SynthesizedNullTexture` | first binding in the sampled-texture band no Metal texture claims; reported only when the shader reads through the handle |
| Placeholder sampler for `air.get_read_sampler()` | `SynthesizedReadSampler` | first binding in the sampler band no Metal sampler claims; reported only when something consumes the value |

Constants live in `metal2vulkan::reflect`:

```text
RESOURCE_DESCRIPTOR_SET = 0
BUFFER_BINDING_BASE     = 0
TEXTURE_BINDING_BASE    = 32
SAMPLER_BINDING_BASE    = 160
COLOR_INPUT_BINDING_BASE = 192
IMAGEBLOCK_BINDING_BASE = 200
IMAGEBLOCK_DATA_RATE_STRIDE = 3
FRAGMENT_IMAGEBLOCK_BINDING_BASE = 224
STORAGE_TEXTURE_BINDING_BASE = 480
SYNTHETIC_BINDING_BASE = 640
```

`DEFAULT_DESCRIPTOR_LAYOUT` explicitly versions that map with `DESCRIPTOR_LAYOUT_VERSION`. A caller
can pass a complete non-overlapping `DescriptorLayout` through
`TransformOptions::with_descriptor_layout`; this selects the set and every resource-class range for
one independently translated stage. Construct base/count ranges with
`DescriptorBindingRange::from_base_count` so overflow is a typed `DescriptorLayoutError`.
The effective layout is returned in `ShaderReflection::descriptor_layout`, including for the
default, so persisted modules remain self-describing.

The stage-input / stage-output passes decorate the module with **exactly the selected** numbers. Use
reflection—not list positions or recomputed synthetic indices—to allocate descriptor sets and write
descriptor updates. Multiple AIR parameters may intentionally alias one Metal index. When building
a Vulkan descriptor-set layout, group reflected entries by `(set, binding, descriptor type)` and use
the maximum reflected `count`; incompatible descriptor types are assigned different ABI bands and
are rejected by translation if they ever collide. That maximum is guaranteed to EQUAL the largest
array the module declares at the binding — `tests/reflection_covers_declared_bindings.rs` checks it,
since a smaller layout is one the shader indexes past and a larger one demands descriptors nothing
reads. A fixed `array<texture..., N>` therefore reports `N`; only a runtime `array_ref<texture...>`,
whose length is a function constant rather than part of the type name, reports the descriptor-ABI
capacity (`TEXTURE_HANDLE_ARRAY_DESCRIPTOR_COUNT`, 128) and expects `descriptorBindingPartiallyBound`
or 128 valid descriptors.

Descriptor types for `bindings`:

| Kind | Vulkan descriptor type |
|---|---|
| `Buffer`, `KernelStageInput`, `AccelerationStructureShadow`, `PrimitiveAccelerationStructure`, `BufferAddressTable`, `RayInstanceUserIdTable` | Storage buffer |
| `Texture` | Sampled image, or uniform texel buffer when `texture_shape.dimension` is `Buffer` |
| `EmbeddedArgBufferTexture` | Sampled image or storage image according to `access`; a reflected `Buffer` dimension uses the corresponding texel-buffer type |
| `TextureArray` | Sampled-image or storage-image array according to `access`; a reflected `Buffer` dimension uses the corresponding texel-buffer type |
| `StorageImage` | Storage image, or storage texel buffer when `texture_shape.dimension` is `Buffer` |
| `SynthesizedNullTexture` | Sampled image of `texture_shape`; contents are never observed |
| `Sampler`, `StaticSampler`, `SynthesizedReadSampler` | Sampler |
| `ColorInput` | Input attachment |

`implicit_imageblock_attachments` and projected `fragment_imageblock.members` are additional
single storage-image descriptors in the effective layout's set; they are not duplicated in
`bindings`. Entries whose
`descriptor` is `None` consume no descriptor.

AIR address spaces (on buffer bindings when present):

| Value | Meaning |
|---|---|
| `ADDRESS_SPACE_DEVICE` (1) | Device memory — descriptor-backed storage buffer |
| `ADDRESS_SPACE_CONSTANT` (2) | Constant / `const device` — typically read-only |
| `ADDRESS_SPACE_THREADGROUP` (3) | Threadgroup — Workgroup variable, no descriptor |

## Reading `ShaderReflection`

Top-level fields:

| Field | Meaning |
|---|---|
| `reflection_version` | Schema version for cache invalidation |
| `descriptor_layout` | Versioned effective set and resource-class binding ranges used by the SPIR-V |
| `stage` | `Vertex`, `TessellationEvaluation`, `Fragment`, or `Kernel` |
| `entry_point` | **Original Metal entry name** (SPIR-V `OpEntryPoint` is always `"main"`) |
| `bindings` | AIR entry resources followed by translator-synthesized resources |
| `argument_buffer_fields` | Resource-handle fields inside argument buffers, with owner and Metal argument-encoder coordinates |
| `vertex_attributes` | Vertex `[[attribute(n)]]` / stage-in locations |
| `varyings` | Fragment stage-in or vertex user varyings (location, AIR type, field name, linker semantic) |
| `render_targets` | Fragment color attachments (member index + location + type name) |
| `depth_members` / `stencil_members` | Fragment return members tagged depth/stencil |
| `depth_qualifier` | Fragment depth comparison contract (`Any`, `Less`, or `Greater`) |
| `local_size` | Nominal Metal kernel local size `[x,y,z]`; exact-thread regions specialize boundary dimensions |
| `max_work_group_size` | `[[max_total_threads_per_threadgroup(N)]]` when the kernel declares one; translation refuses a `local_size` whose product exceeds it |
| `kernel_dispatch` | Whole-workgroup launch or the fixed/dynamic exact-thread region-planning contract |
| `vertex_builtins` | Whether vertex uses `VertexIndex` / `InstanceIndex` / writes `Position` |
| `tessellation` | Post-tessellation patch domain, control-point count, locations, and synthesized system-value carriers |
| `imageblock_layouts` | Kernel `[[imageblock]]` tiles (param index + AIR struct layout; no descriptor) |
| `implicit_imageblock_attachments` | Attachment/data-rate plane, maximum referenced index, format, access, and descriptor binding for implicit imageblock load/store calls |
| `fragment_imageblock` | Custom fragment `[[imageblock_data]]` sample size, exact master fields (offset/type/semantic/raster-order group/access/binding), and semantic-matched input/output projections |
| `datalayout` | Source LLVM `target datalayout` when path-based translation captured it during sanitization |
| `runtime_sampler_specializations` | Pipeline-provided state, keyed by Metal sampler index, that was applied to the returned executable module |
| `function_constants` | `[[function_constant(N)]]` index, name, LLVM type, and exact Metal ABI type encoding |

Custom fragment imageblock fields currently lower exactly as `half` → R16f, `half4` → RGBA16f,
`uchar4` → RGBA8ui, and `ushort` → R16ui storage planes. AIR may provide either an explicit
`air.imageblock_master` for narrow projections or a direct full `air.imageblock_data` struct layout;
both forms preserve the same reflected member contract. Other field types remain an honest fallback.

Each `argument_buffer_fields` entry records the owning entry parameter and Metal buffer index plus
its struct-member ordinal, byte offset, and Metal argument index. A nested buffer additionally
carries `resource_buffer_index`; consumers encode the native Metal buffer through
`MTLArgumentEncoder` and write its Vulkan device address into the same owner byte offset. Embedded
textures, buffers, and authored function tables share this coordinate, so consumers do not need to
reparse `air.struct_type_info`.

Metal post-tessellation vertex entries use `ShaderStage::TessellationEvaluation`. Their
control-point locations are arrays of `control_point_count` values; other listed locations carry
the `Patch` decoration. Hosts connect these values from a tessellation-control stage rather than
binding them as ordinary vertex attributes.

### Per-binding: `ResourceBinding`

| Field | Use for |
|---|---|
| `kind` | Choose descriptor type using the mapping above |
| `metal_index` | Guest Metal slot `n`, or synthetic index for embedded textures |
| `descriptor` | `{ set, binding, count }` or `None` if no descriptor (threadgroup / some locals) |
| `param_index` | SPIR-V `OpFunctionParameter` order, if any |
| `address_space` / `declared_size` / `type_layout` | Buffer address space, argument/pointee size, and aggregate layout. An argument-buffer member marked `air.indirect_argument` with any role but `air.indirect_constant` is a resource the buffer references, so it is reported at the handle size it occupies rather than as the pointee type AIR names it with (`char` for a `device char *`, `texture2d<...>` for a texture). A member AIR names without describing -- a user struct or class mentioned by name only -- is reported as `Opaque { size }` at its declared byte size, because that size is the whole of what the metadata states. A layout therefore reaches exactly as far as `declared_size`. |
| `extent` | Buffer reachability: `Object { bytes }`, `Unbounded`, or `Unknown` |
| `footprint` | Final-module static byte ranges, invocation-strided accesses, and an explicit unbounded-access flag |
| `type_name` | AIR type string when metadata carried it |
| `texture_shape` | Dim / arrayed / MS / component / writable / storage format, plus fixed handle-array length when present (`descriptor.count` is derived from it, so the two never disagree). Reflected translation corrects these to the `OpTypeImage` the module declares at the binding, so the view a consumer creates matches the image variable the descriptor is read through — a texel-read `texturecube` binds as a `D2` array, since SPIR-V has no cube texel fetch. A binding whose image variables do not all declare the same type (a function-constant-gated texture argument can produce that) keeps the type-name-derived shape, as does `reflect_sanitized`, which builds no module. |
| `embedded_source` | For arg-buffer textures: owning buffer index, field byte offset, and Metal `[[id(n)]]` argument-encoder index |
| `access` | When known: `Unused` / `ReadOnly` / `WriteOnly` / `ReadWrite` / `Sampled` / `Storage`. For a descriptor-backed buffer, reflected translation widens the declared classification to cover the loads and stores the finished module performs through it, so it is safe to barrier and stage from. It only ever widens: the analysis follows the pointer graph it can attribute, so a buffer it saw no write through may still be written via a device address. `reflect_sanitized` builds no module and reports the declared classification alone. |
| `static_sampler` | Decoded immutable state for `StaticSampler`; `None` for other kinds. A `StaticSampler` binding is invented by translation, not declared by Metal, so reflected translation reports only the ones the finished module binds -- a constexpr sampler state no sample reads is dropped rather than asked for. `reflect_sanitized` builds no module and reports every state the AIR declares. |

### Buffer extent and access

`extent` is a conservative sizing contract:

- `Object { bytes }` is emitted only when AIR carries `air.buffer_size`, which proves that the
  argument is one bounded reference-like object. A consumer may narrow staging to `bytes`.
- `Unbounded` means AIR identifies a pointer/pointee type but does not carry an array length.
- `Unknown` means the metadata does not distinguish a bounded object from an unbounded pointer.

Treat both `Unbounded` and `Unknown` as “retain the complete caller-provided window.” An incorrect
narrowing is silent data corruption, so reflection never infers an object extent merely from
`declared_size`: for pointer arguments that field can be only the pointee size. Device-space
bindings expose `declared_size` when AIR carries `air.arg_type_size`; a `void *` with no size
metadata remains `None`.

Buffer `access` begins with AIR's declared `air.read` / `air.write` / `air.read_write` qualifier.
The reflected translate paths then tighten that declaration using the specialized LLVM entry:
an unused parameter becomes `Unused`, while sound `readonly` and `writeonly` parameter attributes
become `ReadOnly` and `WriteOnly`. When neither source proves a narrower result, the broader AIR
classification is retained.

### Buffer byte footprints

Successful reflected translation derives each supported descriptor-backed buffer's `footprint`
from the **final constructed SPIR-V module**. This is intentionally later than AIR metadata
reflection: if owned structural facts select a raw-buffer, pointer-value, or CFG representation,
the footprint describes the bytes that representation actually executes. Metadata-only
`reflect_sanitized` leaves `footprint: null` because no executable module exists to audit.

Footprints are populated for `Buffer`, `KernelStageInput`, and `AccelerationStructureShadow` when
they have a descriptor. Other resource kinds leave the field null.

`static_ranges` is a sorted, coalesced list of half-open byte intervals. Each item is serialized as
`{ "offset": N, "size": M }` and denotes `[N, N + M)`. Loads, stores, atomics, and memory copies all
contribute their complete access width; adjacent and overlapping intervals are merged.

`strided_accesses` represents an address of the form:

```text
base_offset + sum(index_source * stride), spanning access_size bytes
```

`index_source` is one stable draw/dispatch domain: `VertexIndex`, `InstanceIndex`, an X/Y/Z component
of `GlobalInvocationId`, `LocalInvocationId`, or `WorkgroupId`, or `LocalInvocationIndex`. Terms are
sorted and repeated sources are combined. A consumer supplies the invocation bounds from its draw or
dispatch and unions the resulting ranges with `static_ranges`. Bound calculations must use checked
arithmetic; overflow has the same meaning as an unbounded access and requires the complete window.

`has_unbounded_access` is the soundness gate. It becomes true for data-dependent runtime-array
indices, pointer/integer escapes, unsupported aggregate transfer widths, arithmetic overflow, or any
other rooted dereference the affine schema cannot prove. When true, consumers must retain the whole
caller-provided window; the other entries remain useful diagnostics but do not authorize narrowing.
When false, the union of the static and bounded strided ranges is a conservative staging footprint.

A reflected descriptor the finished module never declares is a special case, because there is no
variable to walk. Whether its absence proves anything is a property of the module's ADDRESSING
MODEL, not of what AIR declared about the buffer: under `Logical` addressing every access reaches
memory through a declared variable, so a binding with no variable executes zero bytes and its
footprint is bounded and empty. A module that lowered any pointer to a device address carries
`PhysicalStorageBuffer64`, and there a `PhysicalStorageBuffer` access can reach a buffer that has no
descriptor, so the footprint stays unbounded. `access` is unaffected either way — it is the declared
classification widened by what the module was seen doing, and there is nothing to widen it with.

Pointer-select/phi alternatives are analyzed structurally across every descriptor arm. To preserve
the per-translation memory bound, an adversarial expression with more than 4096 address alternatives
is compressed to one unbounded result per affected binding rather than allowed to grow
exponentially. A binding with more than 16,384 distinct pre-coalescing footprint records likewise
becomes unbounded instead of allowing reflection size to grow without limit.

**Gaps consumers should expect:**

- **Device buffer R/W:** ambiguous parameters retain their conservative AIR declaration; malformed
  or unusually sparse metadata can still leave `access: None`, which consumers must treat as
  read-write.
- **Function constants from meta-only builders:** `from_*` constructors leave
  `function_constants` empty; populate via the reflected translate paths (they scan sanitized IR).
- **Datalayout:** only filled when translating from unsanitized `.air`/`.ll` via
  `translate_reflected*` (sanitization strips the line; the reflected path captures it first).
- **Static samplers:** reflected translate paths and `reflect_sanitized` scan
  `!air.sampler_states`; direct `from_*` builders do not. The state includes typed filter, address,
  coordinate, compare, anisotropy, LOD, border, and reduction fields plus the original two AIR
  words.

### Runtime sampler specialization

AIR identifies dynamically bound samplers by Metal index but does not encode the state selected by
the pipeline. Pass that state through `TransformOptions::with_runtime_sampler` when it affects
shader legality or semantics, especially for pixel-coordinate samplers:

```rust
use metal2vulkan::passes::TransformOptions;
use metal2vulkan::reflect::{
    RuntimeSamplerState, SamplerAddressMode, SamplerBorderColor, SamplerCompareFunction,
    SamplerCoordinates, SamplerFilter, SamplerMipFilter, SamplerReduction,
};

let options = TransformOptions::default().with_runtime_sampler(
    0,
    RuntimeSamplerState {
        min_filter: SamplerFilter::Linear,
        mag_filter: SamplerFilter::Linear,
        mip_filter: SamplerMipFilter::None,
        address_mode_s: SamplerAddressMode::ClampToZero,
        address_mode_t: SamplerAddressMode::ClampToZero,
        address_mode_r: SamplerAddressMode::ClampToZero,
        coordinates: SamplerCoordinates::Pixel,
        compare_function: SamplerCompareFunction::None,
        max_anisotropy: 1,
        lod_min_clamp: 0.0,
        lod_max_clamp: 0.0,
        border_color: SamplerBorderColor::TransparentBlack,
        reduction: SamplerReduction::WeightedAverage,
        lod_bias: 0.0,
    },
)?;
```

The index is the Metal `[[sampler(n)]]` index, not the Vulkan descriptor binding (by default,
`160 + n`). A
successful reflected translation copies every applied state into
`runtime_sampler_specializations`; consumers should create the descriptor sampler from that same
state. Pixel-coordinate operations are rewritten to texel fetches and shader-side filtering and
addressing where the translator has an exact model. State or operation combinations that need
unknown derivative, mip, comparison, anisotropy, or dimensional behavior fail translation rather
than emitting an invalid unnormalized-sampler instruction.

### Runtime storage-image specialization

AIR fixes a storage texture's texel component class, but the pipeline chooses the concrete format
bound at runtime. Supply that format and the target device's format features before translation:

```rust
use metal2vulkan::passes::TransformOptions;
use metal2vulkan::reflect::{
    RuntimeStorageImageCapabilities, RuntimeStorageImageFormat, RuntimeStorageImageState,
};

let options = TransformOptions::default().with_runtime_storage_image(
    0,
    RuntimeStorageImageState {
        format: RuntimeStorageImageFormat::Rgba8Unorm,
        capabilities: RuntimeStorageImageCapabilities {
            storage_image: true,
            storage_image_atomic: false,
            read_without_format: false,
            write_without_format: false,
        },
    },
)?;
```

For a top-level binding, the index is the Metal `[[texture(n)]]` index, not the default Vulkan
binding (`480 + n`). For an `EmbeddedArgBufferTexture`, pass that binding's reflected `metal_index`; this is
a translator-assigned synthetic index, so consumers must not reconstruct it from argument-buffer
field positions. Translation selects the compatible explicit SPIR-V image format and independently
specializes bindings that previously shared an AIR image type. When the runtime format has no exact
SPIR-V token, translation uses `Unknown` only if the supplied device features cover every operation
the final shader performs; it declares only the required read-without-format and/or
write-without-format capabilities. Atomics require a scalar 32-bit integer format and storage-image
atomic support. Component-class mismatches, absent bindings, missing format features, and
unsupported atomic formats fail visibly.

Successful reflected translation records the applied states in
`runtime_storage_image_specializations`. Its `spirv_format` is `Some` for an explicit format and
`None` for `Unknown`; the corresponding binding's `texture_shape.storage_format` reports the same
choice. Create the image view and descriptor from the same runtime state used for translation.

### Nil texture bindings are resolved at translation time

Metal decides whether a texture slot is nil when the encoder binds, and `air.is_null_texture_*`
observes that decision at run time. A Logical SPIR-V module has no equivalent query: a descriptor
either exists in the module or it does not, and nothing in the shader can ask the host what it
bound. Translation therefore answers the predicate from the module it is building.

A texture argument that translation surfaces as a descriptor answers `false`. The predicate answers
`true` only for a handle translation itself synthesized (`air.get_null_texture_*`, reported as
`SynthesizedNullTexture`) or for an argument the module does not surface at all — for example a
function-constant-gated argument the selected variant disables. Reads through an absent handle fold
to zero and writes through one are dropped, matching Metal's contract for an unbound resource.

**Consequence for the consumer:** a translated module COMMITS to "this slot is bound". Binding nil to
that Metal index at run time does not take the shader's nil branch; the shader reads whatever the
descriptor holds. If a pipeline can leave the slot nil, that is a second pipeline configuration and
needs its own translation, in the same way a different runtime sampler or storage-image format does.

Measured over the 14,579-source local corpus: 583 sources name `air.is_null_texture_*`, and 581 of
them contain no `air.get_null_texture_*` handle at all — so for those the predicate is entirely a
question about host binding, and translation answers it as "bound".

## Typical consumer flow

1. Call `translate_reflected` (or a serde-enabled CLI with `--emit-meta`).
2. Cache `(spv_bytes, reflection)` keyed by input hash + `reflection_version` + translator version.
3. Create a Vulkan pipeline with entry point `"main"`.
4. Build the ordinary portion of the effective `descriptor_layout.set` from every `bindings` entry
   with `Some(descriptor)`:
   - Map `kind` → descriptor type.
   - Write `set` / `binding` from `descriptor`.
   - Use `metal_index` to pick the host resource that was bound as Metal slot `n`.
   - For buffers, narrow staging from `footprint` only when `has_unbounded_access` is false; bound
     every strided term from the current draw/dispatch before taking the union.
5. Add one storage-image descriptor in that same effective set for every
   `implicit_imageblock_attachments` entry and every projected `fragment_imageblock` member, using
   their reported `binding` values.
6. Populate runtime storage images, static samplers, and argument-buffer fields from their
   reflected state/coordinates; resources with `descriptor: None` require no descriptor write.
7. For `ThreadgroupBuffer` / `imageblock_layouts`, allocate Workgroup / tile storage from
   `declared_size` / `type_layout` (no descriptor write).
8. For vertex: bind attributes from `vertex_attributes` and respect `vertex_builtins`.
9. For fragment: attach color targets from `render_targets`; attach the reflected depth/stencil
   aspects and derive depth comparison from `depth_qualifier`.
10. For kernels: obey `kernel_dispatch`. `ThreadsDynamic` and `ThreadsFixed` require
    `KernelDispatch::plan`. Create a pipeline for each distinct region `local_size`, specializing
    the three `KERNEL_LOCAL_SIZE_SPEC_IDS`; write the region's
    `KernelDispatchPlan::push_constants` payload into the reflected
    `KERNEL_DISPATCH_PUSH_CONSTANT_SIZE` range; then dispatch its `group_count`. Execute every
    returned region. `Workgroups` is the only single-pipeline path and appears only when the caller
    explicitly proves every launch is complete. When the host supplies function constants, use
    `translate_sanitized_native_specialized_with_options` so exact scalar/vector payloads are baked
    before metadata, resource-interface, and CFG construction. Post-SPIR-V specialization cannot
    restore a resource or branch already removed under the default value. Specialized reflection
    exposes true-gated resources and omits false-gated resources from the descriptor contract.

## Related APIs

| Module | Role |
|---|---|
| `metal2vulkan::reflect` | Public facade (`ShaderReflection`, ABI constants) |
| `metal2vulkan::meta` | Lower-level AIR metadata parsers (`FragMeta` / `VertMeta` / `KernMeta`) |
| `metal2vulkan::passes::Stage` | Stage enum for translate |
| `metal2vulkan::reflect_sanitized_specialized` | Reflect the same function-constant-specialized AIR resource contract used by translation |
| `metal2vulkan::translate_sanitized_native_specialized_with_options` | Translate exact-width scalar/vector FC payloads before structural lowering |
| `metal2vulkan::translate_sanitized_native_linked_specialized_with_options` | Apply the same AIR-level specialization after authored linkage resolution |
| `metal2vulkan::translate_sanitized_native_specialized_reflected_with_options` | Translate FC payloads AND get the reflection of the module you are about to bind |
| `metal2vulkan::translate_sanitized_native_linked_specialized_reflected_with_options` | The same, after authored linkage resolution |
| `metal2vulkan::specialize_function_constant_bytes` | Repoint FC initializers only in an already emitted module that retained the complete specialized structure |

Unit coverage for binding numbers lives in `src/reflect/tests.rs` (the default ABI uses set 0, bases
0/32/160/192/200/224/480, and a synthetic range beginning at 640; configurable layouts are covered
separately).

## What reflection is *not*

- Not a general-purpose SPIR-V reflector. Final-module inspection is deliberately limited to the
  descriptor/type/value graph required for conservative buffer footprints.
- Not a substitute for `spirv-val` or runtime pipeline creation.
- Not populated for passthrough vertex generation (`translate_passthrough`).
- Not a description of run-time binding choices. `air.is_null_texture_*` is resolved when the module
  is built, not when the encoder binds — see *Nil texture bindings are resolved at translation
  time*.

Rasterization-rate-map coordinate decoder AIR calls currently return a visible unsupported-input
error. Runtime map parameter bytes and the layer determine both directions; neither is lowered to
an unconditional identity. Authored execution reports the shared typed
`rasterization_rate_map_parameter_data` requirement until both executors have a literal map contract.
