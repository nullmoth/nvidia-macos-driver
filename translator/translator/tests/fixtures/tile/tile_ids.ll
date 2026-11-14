; ModuleID = 'tile_ids.metal'
source_filename = "tile_ids.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%"struct.metal::_imageblock_base" = type { %struct._imageblock_t addrspace(4)* }
%struct._imageblock_t = type opaque

; Function Attrs: mustprogress nounwind willreturn writeonly
define void @tile_ids(%"struct.metal::_imageblock_base" %0, <2 x i16> noundef %1, <2 x i16> noundef %2) local_unnamed_addr #0 {
  %4 = extractelement <2 x i16> %2, i64 0
  %5 = tail call fast half @air.convert.f.f16.u.i16(i16 %4) #3
  %6 = insertelement <4 x half> undef, half %5, i64 0
  %7 = extractelement <2 x i16> %2, i64 1
  %8 = tail call fast half @air.convert.f.f16.u.i16(i16 %7) #3
  %9 = insertelement <4 x half> %6, half %8, i64 1
  %10 = extractelement <2 x i16> %1, i64 0
  %11 = tail call fast half @air.convert.f.f16.u.i16(i16 %10) #3
  %12 = insertelement <4 x half> %9, half %11, i64 2
  %13 = extractelement <2 x i16> %1, i64 1
  %14 = tail call fast half @air.convert.f.f16.u.i16(i16 %13) #3
  %15 = insertelement <4 x half> %12, half %14, i64 3
  %16 = fmul fast <4 x half> %15, <half 0xH1C04, half 0xH1C04, half 0xH1C04, half 0xH1C04>
  tail call void @air.store.implicit_imageblock.v4f16(<4 x half> %16, i32 0, <2 x i16> %1, i32 0, i16 0) #4
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare half @air.convert.f.f16.u.i16(i16) local_unnamed_addr #1

; Function Attrs: mustprogress nounwind willreturn writeonly
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16) local_unnamed_addr #2

attributes #0 = { mustprogress nounwind willreturn writeonly "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="64" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { mustprogress nounwind willreturn writeonly }
attributes #3 = { nounwind readnone willreturn }
attributes #4 = { nounwind willreturn writeonly }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!16, !17, !18}
!llvm.ident = !{!19}
!air.version = !{!20}
!air.language_version = !{!21}
!air.source_file_name = !{!22}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (%"struct.metal::_imageblock_base", <2 x i16>, <2 x i16>)* @tile_ids, !10, !11}
!10 = !{}
!11 = !{!12, !14, !15}
!12 = !{i32 0, !"air.imageblock", !"implicit", !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<Color0, layout_implicit>", !"air.arg_name", !"blk"}
!13 = !{i32 0, i32 8, i32 0, !"half4", !"c", !"air.render_target", i32 0}
!14 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"lid"}
!15 = !{i32 2, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tile"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!20 = !{i32 2, i32 7, i32 0}
!21 = !{!"Metal", i32 3, i32 2, i32 0}
!22 = !{!"tile_ids.metal"}
