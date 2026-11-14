; ModuleID = 'fs_gb_acc.metal'
source_filename = "fs_gb_acc.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%struct.GB = type { <4 x half>, half }

; Function Attrs: mustprogress nofree norecurse nosync nounwind readnone willreturn
define <{ %struct.GB }> @fs_gb_acc(<4 x float> noundef %0, %struct.GB %1) local_unnamed_addr #0 {
  %3 = extractvalue %struct.GB %1, 1
  %4 = fadd fast half %3, 0xH3C00
  %5 = insertvalue %struct.GB %1, half %4, 1
  %6 = insertvalue <{ %struct.GB }> undef, %struct.GB %5, 0
  ret <{ %struct.GB }> %6
}

attributes #0 = { mustprogress nofree norecurse nosync nounwind readnone willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.fragment = !{!9}
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
!9 = !{<{ %struct.GB }> (<4 x float>, %struct.GB)* @fs_gb_acc, !10, !13}
!10 = !{!11}
!11 = !{!"air.imageblock_data", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !12, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"GB", !"air.arg_name", !"gb"}
!12 = !{i32 0, i32 8, i32 0, !"half4", !"a", !"air.raster_order_group", i32 0, i32 8, i32 2, i32 0, !"half", !"b", !"air.raster_order_group", i32 0}
!13 = !{!14, !15}
!14 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos", !"air.arg_unused"}
!15 = !{i32 1, !"air.imageblock_data", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !12, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"GB", !"air.arg_name", !"prev"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!20 = !{i32 2, i32 7, i32 0}
!21 = !{!"Metal", i32 3, i32 2, i32 0}
!22 = !{!"fs_gb_acc.metal"}
