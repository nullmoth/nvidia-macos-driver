; ModuleID = 'fs_gb2_acc.metal'
source_filename = "fs_gb2_acc.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%struct.GB2 = type { <4 x float>, i32, i32, <4 x i8>, i8 }

; Function Attrs: mustprogress nofree norecurse nosync nounwind readnone willreturn
define <{ %struct.GB2 }> @fs_gb2_acc(<4 x float> noundef %0, %struct.GB2 %1) local_unnamed_addr #0 {
  %3 = extractvalue %struct.GB2 %1, 0
  %4 = extractvalue %struct.GB2 %1, 1
  %5 = extractvalue %struct.GB2 %1, 2
  %6 = extractvalue %struct.GB2 %1, 3
  %7 = extractvalue %struct.GB2 %1, 4
  %8 = fadd fast <4 x float> %3, <float 1.000000e+00, float 1.000000e+00, float 1.000000e+00, float 1.000000e+00>
  %9 = add nsw i32 %4, -3
  %10 = add i32 %5, 5
  %11 = extractelement <4 x i8> %6, i64 0
  %12 = add i8 %11, 1
  %13 = insertelement <4 x i8> %6, i8 %12, i64 0
  %14 = add i8 %7, 2
  %15 = insertvalue %struct.GB2 poison, <4 x float> %8, 0
  %16 = insertvalue %struct.GB2 %15, i32 %9, 1
  %17 = insertvalue %struct.GB2 %16, i32 %10, 2
  %18 = insertvalue %struct.GB2 %17, <4 x i8> %13, 3
  %19 = insertvalue %struct.GB2 %18, i8 %14, 4
  %20 = insertvalue <{ %struct.GB2 }> undef, %struct.GB2 %19, 0
  ret <{ %struct.GB2 }> %20
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
!9 = !{<{ %struct.GB2 }> (<4 x float>, %struct.GB2)* @fs_gb2_acc, !10, !13}
!10 = !{!11}
!11 = !{!"air.imageblock_data", !"air.imageblock_data_size", i32 32, !"air.struct_type_info", !12, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"GB2", !"air.arg_name", !"gb"}
!12 = !{i32 0, i32 16, i32 0, !"float4", !"f", !"air.raster_order_group", i32 0, i32 16, i32 4, i32 0, !"int", !"i", !"air.raster_order_group", i32 0, i32 20, i32 4, i32 0, !"uint", !"u", !"air.raster_order_group", i32 0, i32 24, i32 4, i32 0, !"uchar4", !"c", !"air.raster_order_group", i32 0, i32 28, i32 1, i32 0, !"uchar", !"b", !"air.raster_order_group", i32 0}
!13 = !{!14, !15}
!14 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos", !"air.arg_unused"}
!15 = !{i32 1, !"air.imageblock_data", !"air.imageblock_data_size", i32 32, !"air.struct_type_info", !12, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"GB2", !"air.arg_name", !"prev"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!20 = !{i32 2, i32 7, i32 0}
!21 = !{!"Metal", i32 3, i32 2, i32 0}
!22 = !{!"fs_gb2_acc.metal"}
