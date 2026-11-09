; ModuleID = 'wgptr1.metal'
source_filename = "wgptr1.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.0.0"

; Function Attrs: argmemonly mustprogress nofree norecurse nosync nounwind readonly willreturn
define <4 x float> @_Z4filtPU14MTLthreadgroupDv4_fji(<4 x float> addrspace(3)* nocapture noundef readonly %0, i32 noundef %1, i32 noundef %2) local_unnamed_addr #0 {
  %4 = icmp slt i32 %2, 0
  br i1 %4, label %7, label %5

5:                                                ; preds = %3
  %6 = sub nsw i32 0, %2
  br label %9

7:                                                ; preds = %9, %3
  %8 = phi <4 x float> [ zeroinitializer, %3 ], [ %16, %9 ]
  ret <4 x float> %8

9:                                                ; preds = %5, %9
  %10 = phi i32 [ %17, %9 ], [ %6, %5 ]
  %11 = phi <4 x float> [ %16, %9 ], [ zeroinitializer, %5 ]
  %12 = add i32 %10, %1
  %13 = zext i32 %12 to i64
  %14 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %0, i64 %13
  %15 = load <4 x float>, <4 x float> addrspace(3)* %14, align 16, !tbaa !23
  %16 = fadd fast <4 x float> %15, %11
  %17 = add i32 %10, 1
  %18 = icmp eq i32 %10, %2
  br i1 %18, label %7, label %9, !llvm.loop !26
}

; Function Attrs: convergent mustprogress nounwind
define void @k(<4 x float> addrspace(1)* nocapture noundef "air-buffer-no-alias" %0, <4 x float> addrspace(3)* nocapture noundef "air-buffer-no-alias" %1, i32 noundef %2, i32 noundef %3) local_unnamed_addr #1 {
  %5 = zext i32 %3 to i64
  %6 = getelementptr inbounds <4 x float>, <4 x float> addrspace(1)* %0, i64 %5
  %7 = load <4 x float>, <4 x float> addrspace(1)* %6, align 16, !tbaa !23, !alias.scope !28, !noalias !31
  %8 = add i32 %2, 8
  %9 = zext i32 %8 to i64
  %10 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %1, i64 %9
  store <4 x float> %7, <4 x float> addrspace(3)* %10, align 16, !tbaa !23, !alias.scope !31, !noalias !28
  tail call void @air.wg.barrier(i32 2, i32 1) #3
  %11 = zext i32 %2 to i64
  %12 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %1, i64 0, i64 %11
  %13 = load float, float addrspace(3)* %12, align 4, !tbaa !33, !alias.scope !31, !noalias !28
  %14 = fadd fast float %13, 1.000000e+00
  store float %14, float addrspace(3)* %12, align 4, !tbaa !33, !alias.scope !31, !noalias !28
  br label %15

15:                                               ; preds = %15, %4
  %16 = phi i32 [ %23, %15 ], [ -3, %4 ]
  %17 = phi <4 x float> [ %22, %15 ], [ zeroinitializer, %4 ]
  %18 = add i32 %16, %8
  %19 = zext i32 %18 to i64
  %20 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %1, i64 %19
  %21 = load <4 x float>, <4 x float> addrspace(3)* %20, align 16, !tbaa !23, !alias.scope !31, !noalias !28
  %22 = fadd fast <4 x float> %21, %17
  %23 = add nsw i32 %16, 1
  %24 = icmp eq i32 %16, 3
  br i1 %24, label %25, label %15, !llvm.loop !26

25:                                               ; preds = %15
  store <4 x float> %22, <4 x float> addrspace(1)* %6, align 16, !tbaa !23, !alias.scope !28, !noalias !31
  ret void
}

; Function Attrs: convergent mustprogress nounwind willreturn
declare void @air.wg.barrier(i32, i32) local_unnamed_addr #2

attributes #0 = { argmemonly mustprogress nofree norecurse nosync nounwind readonly willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { convergent mustprogress nounwind "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #2 = { convergent mustprogress nounwind willreturn }
attributes #3 = { convergent nounwind willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!16, !17, !18}
!llvm.ident = !{!19}
!air.version = !{!20}
!air.language_version = !{!21}
!air.source_file_name = !{!22}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 26, i32 5]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (<4 x float> addrspace(1)*, <4 x float> addrspace(3)*, i32, i32)* @k, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14, !15}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"o"}
!13 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"tile"}
!14 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!15 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"g"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.883 (metalfe-32023.883)"}
!20 = !{i32 2, i32 8, i32 0}
!21 = !{!"Metal", i32 4, i32 0, i32 0}
!22 = !{!"/src/wgptr1.metal"}
!23 = !{!24, !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C++ TBAA"}
!26 = distinct !{!26, !27}
!27 = !{!"llvm.loop.mustprogress"}
!28 = !{!29}
!29 = distinct !{!29, !30, !"air-alias-scope-arg(0)"}
!30 = distinct !{!30, !"air-alias-scopes(k)"}
!31 = !{!32}
!32 = distinct !{!32, !30, !"air-alias-scope-arg(1)"}
!33 = !{!34, !34, i64 0}
!34 = !{!"float", !24, i64 0}
