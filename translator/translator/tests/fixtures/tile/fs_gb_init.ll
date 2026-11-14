; ModuleID = 'fs_gb_init.metal'
source_filename = "fs_gb_init.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%struct.GB = type { <4 x half>, half }

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
define <{ %struct.GB }> @fs_gb_init(<4 x float> noundef %0) local_unnamed_addr #0 {
  %2 = extractelement <4 x float> %0, i64 0
  %3 = tail call i32 @air.convert.u.i32.f.f32(float %2) #2
  %4 = extractelement <4 x float> %0, i64 1
  %5 = tail call i32 @air.convert.u.i32.f.f32(float %4) #2
  %6 = shl i32 %3, 2
  %7 = and i32 %6, 252
  %8 = tail call fast float @air.convert.f.f32.u.i32(i32 %7) #2
  %9 = mul i32 %5, 5
  %10 = and i32 %9, 255
  %11 = tail call fast float @air.convert.f.f32.u.i32(i32 %10) #2
  %12 = insertelement <4 x float> <float poison, float poison, float 7.000000e+00, float 2.550000e+02>, float %8, i64 0
  %13 = insertelement <4 x float> %12, float %11, i64 1
  %14 = fmul fast <4 x float> %13, <float 0x3F70101020000000, float 0x3F70101020000000, float 0x3F70101020000000, float 0x3F70101020000000>
  %15 = tail call fast <4 x half> @air.convert.f.v4f16.f.v4f32(<4 x float> %14) #2
  %16 = insertvalue %struct.GB poison, <4 x half> %15, 0
  %17 = insertvalue %struct.GB %16, half 0xH3C00, 1
  %18 = insertvalue <{ %struct.GB }> undef, %struct.GB %17, 0
  ret <{ %struct.GB }> %18
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare i32 @air.convert.u.i32.f.f32(float) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare float @air.convert.f.f32.u.i32(i32) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare <4 x half> @air.convert.f.v4f16.f.v4f32(<4 x float>) local_unnamed_addr #1

attributes #0 = { mustprogress nofree nosync nounwind readnone willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { nounwind readnone willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.fragment = !{!9}
!air.compile_options = !{!15, !16, !17}
!llvm.ident = !{!18}
!air.version = !{!19}
!air.language_version = !{!20}
!air.source_file_name = !{!21}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{<{ %struct.GB }> (<4 x float>)* @fs_gb_init, !10, !13}
!10 = !{!11}
!11 = !{!"air.imageblock_data", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !12, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"GB", !"air.arg_name", !"gb"}
!12 = !{i32 0, i32 8, i32 0, !"half4", !"a", !"air.raster_order_group", i32 0, i32 8, i32 2, i32 0, !"half", !"b", !"air.raster_order_group", i32 0}
!13 = !{!14}
!14 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos"}
!15 = !{!"air.compile.denorms_disable"}
!16 = !{!"air.compile.fast_math_enable"}
!17 = !{!"air.compile.framebuffer_fetch_enable"}
!18 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!19 = !{i32 2, i32 7, i32 0}
!20 = !{!"Metal", i32 3, i32 2, i32 0}
!21 = !{!"fs_gb_init.metal"}
