; ModuleID = 'c.metal'
source_filename = "c.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v29-apple-macosx27.0.0"

%struct._instance_acceleration_structure_t = type opaque
%struct._intersection_function_table_t = type opaque

; Function Attrs: mustprogress nounwind willreturn
define void @k(%struct._instance_acceleration_structure_t addrspace(1)* %0, <4 x float> addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %1, i32 noundef %2) local_unnamed_addr #0 {
  %4 = tail call %struct._intersection_function_table_t addrspace(1)* @air.get_null_intersection_function_table() #4
  %5 = tail call { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, float } @air.intersect.instancing.curve_data(<3 x float> <float 0.000000e+00, float 0.000000e+00, float -5.000000e+00>, <3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, float 0.000000e+00, float 1.000000e+02, %struct._instance_acceleration_structure_t addrspace(1)* readonly %0, i32 -1, %struct._intersection_function_table_t addrspace(1)* readonly %4, i8* null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 4, i32 3, i32 0, i32 4, i1 false, i1 false) #5
  %6 = extractvalue { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, float } %5, 1
  %7 = extractvalue { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, float } %5, 2
  %8 = extractvalue { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, float } %5, 7
  %9 = tail call fast float @air.convert.f.f32.u.i32(i32 %7) #6
  %10 = insertelement <4 x float> <float poison, float poison, float poison, float 0.000000e+00>, float %6, i64 0
  %11 = insertelement <4 x float> %10, float %8, i64 1
  %12 = insertelement <4 x float> %11, float %9, i64 2
  %13 = zext i32 %2 to i64
  %14 = getelementptr inbounds <4 x float>, <4 x float> addrspace(1)* %1, i64 %13
  store <4 x float> %12, <4 x float> addrspace(1)* %14, align 16, !tbaa !22, !alias.scope !25, !noalias !28
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare float @air.convert.f.f32.u.i32(i32) local_unnamed_addr #1

; Function Attrs: inaccessiblememonly mustprogress nofree nounwind readonly willreturn
declare %struct._intersection_function_table_t addrspace(1)* @air.get_null_intersection_function_table() local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, float } @air.intersect.instancing.curve_data(<3 x float>, <3 x float>, float, float, %struct._instance_acceleration_structure_t addrspace(1)* readonly, i32, %struct._intersection_function_table_t addrspace(1)* readonly, i8*, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1) local_unnamed_addr #3

attributes #0 = { mustprogress nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="96" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { inaccessiblememonly mustprogress nofree nounwind readonly willreturn }
attributes #3 = { mustprogress nounwind willreturn }
attributes #4 = { inaccessiblememonly nounwind readonly willreturn }
attributes #5 = { nounwind willreturn }
attributes #6 = { nounwind readnone willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
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
!9 = !{void (%struct._instance_acceleration_structure_t addrspace(1)*, <4 x float> addrspace(1)*, i32)* @k, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14}
!12 = !{i32 0, !"air.instance_acceleration_structure", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!13 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"o"}
!14 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"t"}
!15 = !{!"air.compile.denorms_disable"}
!16 = !{!"air.compile.fast_math_enable"}
!17 = !{!"air.compile.framebuffer_fetch_enable"}
!18 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!19 = !{i32 2, i32 9, i32 0}
!20 = !{!"Metal", i32 4, i32 1, i32 0}
!21 = !{!"curve.metal"}
!22 = !{!23, !23, i64 0}
!23 = !{!"omnipotent char", !24, i64 0}
!24 = !{!"Simple C++ TBAA"}
!25 = !{!26}
!26 = distinct !{!26, !27, !"air-alias-scope-arg(1)"}
!27 = distinct !{!27, !"air-alias-scopes(k)"}
!28 = !{!29}
!29 = distinct !{!29, !27, !"air-alias-scope-arg(0)"}
