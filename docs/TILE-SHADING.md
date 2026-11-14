# Tile shading

Metal's tile shading (tile functions, imageblocks, threadgroup memory per tile) runs on NVIDIA GPUs, which have no tile
memory, by splitting a render pass around each tile function and keeping the imageblock in storage images. This file
describes what is supported, how it works, and where it stops.

## What works

* `newRenderPipelineStateWithTileDescriptor:` (all forms) and the Metal 4 route to the same code
  (`nvmtl_tile_pipeline_create`, used by the Metal 4 compiler for `MTL4TileRenderPipelineDescriptor`).
* On the render encoder: `setTileBuffer/Buffers/BufferOffset/Bytes/Texture/Textures/SamplerState(s)`,
  `setThreadgroupMemoryLength:offset:atIndex:`, `dispatchThreadsPerTile:`, `tileWidth`, `tileHeight`. The pass descriptor's
  `tileWidth`, `tileHeight`, `threadgroupMemoryLength` and `imageblockSampleLength` are honoured. With no tile size given,
  the pass gets the largest of 32x32, 32x16, 16x16, 16x8, 8x8 whose imageblock fits a 32 KB tile (what Apple GPUs choose).
* Tile functions that read and write the colour attachments (implicit imageblock, `blk.read/write`, any attachment
  format with a storage-image stand-in, sRGB and BGRA included).
* Explicit-layout imageblocks written by fragment shaders (`[[imageblock_data]]`, `raster_order_group`) and read or
  written by tile functions, with members of type half, half4, float, float4, uint, uint4, int, int4, uchar, uchar4,
  ushort (at most eight members).
* Threadgroup memory of a tile function that outlives the dispatch: what one dispatch leaves in `[[threadgroup(n)]]`
  memory the next dispatch of the same pass finds there, with draws in between. Elements of 8, 16 and 32-bit scalars,
  vectors, arrays and structures of them.
* `imageblockSampleLength` and `imageblockMemoryLengthForDimensions:` of tile and draw pipelines (attachment bytes
  plus the explicit structure, rounded to 8; Apple answers 24 for a 16-byte structure beside one RGBA8 attachment).

## How

A tile dispatch inside a render pass:

1. ends the Vulkan render pass (a pass that has not begun first applies its Clear actions once);
2. copies each colour attachment the function reads into a storage image padded to whole tiles, unless that image
   already holds the newest contents, and binds it where the translated function expects its imageblock;
3. replays the tile bindings into a compute encoder that exists only for this dispatch, over a saved copy of the
   vertex-stage binding table, so the draws around the tile function keep the bindings they made;
4. dispatches one workgroup per tile (the tile size and tile count ride in push constants after the dispatch region);
5. copies planes the function wrote back into their attachments before the next draw begins a pass, or before the
   encoder ends.

The next `setRenderPipelineState:` with a draw pipeline (Metal requires one after a tile pipeline) begins a new Vulkan
pass with every attachment loaded.

The translator (`translator/translator/src/tile_mode.rs`) is told what the device and the function are:

| switch | meaning |
|---|---|
| `BAND` | imageblock planes live in the top 32 bindings of the driver's storage-image band: implicit planes at 576 + 3 * attachment, explicit members at 600 + member. Planes are formatless (the attachment's own format) and not arrayed. |
| `NO_INTERLOCK` | the device has no fragment shader interlock: a fragment shader with an imageblock is not wrapped in one |
| `TILE_KERNEL` | the function is a tile function: tile-local coordinates get the tile origin added; an explicit imageblock argument is staged from and to the member planes |
| `PERSISTENT_TG` | `[[threadgroup(n)]]` arguments are loaded from and stored to per-tile memory (a storage buffer at Metal buffer slot 28) around the body |

The driver sets `BAND | NO_INTERLOCK` for the process (`nvmtl_translate_set_tile_defaults`) and the rest around the
translation of a tile function (`nvmtl_translate_set_tile_flags`, `nvmtl_translate_set_tile_tg_offsets`).
`METAL2VULKAN_TILE` (bits as above) and `METAL2VULKAN_TILE_TG_OFFSETS` do the same for the command-line tool.

## Where it stops

* Multisampled attachments (a per-sample imageblock) and imageblock data rates other than pixel: refused, logged as
  unsupported.
* Fragment functions used as tile shaders, and threadgroup arguments in fragment functions: refused.
* Raster order of overlapping primitives in one draw is kept for triangle-list draws of at most 256 triangles (each
  triangle is drawn behind a barrier, as framebuffer fetch does); larger draws are not ordered between overlapping
  primitives, because NVK has no fragment shader interlock.
* A tile function that keeps threadgroup memory across dispatches must reach one return with every invocation: early
  returns on some invocations make the final copy unsafe, and the translator refuses a function with several returns.
* A tile function whose threads per tile are fewer than the tile's pixels cannot use an explicit imageblock (one cell
  per thread is staged).
* Tile buffer index 28 and tile texture indices from 96 up collide with the planes and tile memory the driver adds.
* Attachments that are reinterpreting format views, slices of 3D textures, or converting stand-ins that are also
  mip levels or array slices.
* Cost: each tile dispatch after draws copies the attachments in, and back if the function wrote them.

## Tests

`tests/tile` holds the oracles: `tile_oracle` (implicit imageblock, tile bindings, threadgroup memory, persistence),
`tile_oracle_explicit` (fragment-written explicit imageblock, raster order, sample length) and `tile_oracle_mtl4` (the
Metal 4 route). Each prints named arms with the measured value and a negative arm that must fail. They run on Apple
hardware for the truth and unchanged on the NVIDIA machine; `tests/tile/build.sh <dir>` builds the metallib and the arm64
and x86_64 binaries. The translator's side is `translator/translator/tests/tile_shading.rs`.
