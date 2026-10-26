; Owned synthetic fixture for a pointer phi with a `null` arm over a RAW device buffer.
; Not derived from a third-party metallib; this is the Metal front end's own output for the
; sibling .metal, rewritten to opaque pointers.
source_filename = "kernel_nullable_branch_cursor.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.0.0"

; Function Attrs: mustprogress nofree nosync nounwind willreturn
define void @nullable_branch_cursor(ptr addrspace(1) noundef readonly "air-buffer-no-alias" %0, ptr addrspace(1) nocapture noundef readonly "air-buffer-no-alias" %1, ptr addrspace(1) nocapture noundef writeonly "air-buffer-no-alias" %2, i32 noundef %3) local_unnamed_addr #0 {
  %5 = bitcast ptr addrspace(1) %0 to ptr addrspace(1)
  %6 = getelementptr inbounds i8, ptr addrspace(1) %5, i64 3
  %7 = load i8, ptr addrspace(1) %6, align 1, !tbaa !23, !alias.scope !26, !noalias !29
  %8 = load i32, ptr addrspace(1) %1, align 4, !tbaa !32, !alias.scope !34, !noalias !35
  %9 = icmp eq i32 %8, 0
  br i1 %9, label %15, label %10

10:                                               ; preds = %4
  %11 = getelementptr inbounds i32, ptr addrspace(1) %1, i64 1
  %12 = load i32, ptr addrspace(1) %11, align 4, !tbaa !32, !alias.scope !34, !noalias !35
  %13 = zext i32 %12 to i64
  %14 = getelementptr inbounds <4 x float>, ptr addrspace(1) %0, i64 %13
  br label %15

15:                                               ; preds = %10, %4
  %16 = phi ptr addrspace(1) [ %14, %10 ], [ null, %4 ]
  %17 = icmp eq ptr addrspace(1) %16, null
  br i1 %17, label %21, label %18

18:                                               ; preds = %15
  %19 = load <4 x float>, ptr addrspace(1) %16, align 16, !alias.scope !26, !noalias !29
  %20 = extractelement <4 x float> %19, i64 0
  br label %21

21:                                               ; preds = %18, %15
  %22 = phi float [ %20, %18 ], [ 0.000000e+00, %15 ]
  %23 = tail call fast float @air.convert.f.f32.u.i8(i8 %7) #2
  %24 = fadd fast float %23, %22
  %25 = tail call fast float @air.convert.f.f32.u.i32(i32 %3) #2
  %26 = fadd fast float %24, %25
  %27 = zext i32 %3 to i64
  %28 = getelementptr inbounds float, ptr addrspace(1) %2, i64 %27
  store float %26, ptr addrspace(1) %28, align 4, !tbaa !36, !alias.scope !38, !noalias !39
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare float @air.convert.f.f32.u.i8(i8) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare float @air.convert.f.f32.u.i32(i32) local_unnamed_addr #1

attributes #0 = { mustprogress nofree nosync nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { nounwind readnone willreturn }

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
!9 = !{ptr @nullable_branch_cursor, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14, !15}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"src"}
!13 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"ctl"}
!14 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!15 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.883 (metalfe-32023.883)"}
!20 = !{i32 2, i32 8, i32 0}
!21 = !{!"Metal", i32 4, i32 0, i32 0}
!22 = !{!"kernel_nullable_branch_cursor.metal"}
!23 = !{!24, !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C++ TBAA"}
!26 = !{!27}
!27 = distinct !{!27, !28, !"air-alias-scope-arg(0)"}
!28 = distinct !{!28, !"air-alias-scopes(nullable_branch_cursor)"}
!29 = !{!30, !31}
!30 = distinct !{!30, !28, !"air-alias-scope-arg(1)"}
!31 = distinct !{!31, !28, !"air-alias-scope-arg(2)"}
!32 = !{!33, !33, i64 0}
!33 = !{!"int", !24, i64 0}
!34 = !{!30}
!35 = !{!27, !31}
!36 = !{!37, !37, i64 0}
!37 = !{!"float", !24, i64 0}
!38 = !{!31}
!39 = !{!27, !30}
