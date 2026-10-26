; Owned synthetic fixture for a `constant`-space induction pointer walked by a nested loop.
; Not derived from a third-party metallib.
;
; The outer loop's carried pointer %8 is advanced by the INNER loop: its backedge arm %14 is a
; getelementptr off the inner header's phi %12, whose own entry arm is %8. No arm of %8 is a
; getelementptr off %8 itself, so the pointer is a CYCLE and not a self-recursive induction.
; The Metal front end never emits this spelling -- it rewrites the outer backedge as a single
; `gep %8, 3` because the inner trip count is constant -- so the .ll is hand-written and the
; .metal is the same computation in the spelling the front end does emit.
source_filename = "kernel_nested_loop_constant_weight_walk.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.0.0"

; Function Attrs: mustprogress nofree norecurse nosync nounwind
define void @nested_loop_constant_weight_walk(ptr addrspace(2) readonly captures(none) "air-buffer-no-alias" %0, ptr addrspace(1) writeonly captures(none) "air-buffer-no-alias" %1, i32 %2) local_unnamed_addr #0 {
  %4 = uitofp i32 %2 to float
  br label %5

5:                                                ; preds = %3, %16
  %6 = phi i32 [ 0, %3 ], [ %17, %16 ]
  %7 = phi ptr addrspace(2) [ %0, %3 ], [ %13, %16 ]
  %8 = phi float [ %4, %3 ], [ %20, %16 ]
  br label %9

9:                                                ; preds = %5, %9
  %10 = phi i32 [ 0, %5 ], [ %14, %9 ]
  %11 = phi ptr addrspace(2) [ %7, %5 ], [ %13, %9 ]
  %12 = phi float [ %8, %5 ], [ %20, %9 ]
  %18 = load <4 x float>, ptr addrspace(2) %11, align 16, !tbaa !23
  %19 = extractelement <4 x float> %18, i64 0
  %20 = fadd fast float %12, %19
  %13 = getelementptr inbounds <4 x float>, ptr addrspace(2) %11, i64 1
  %14 = add nuw nsw i32 %10, 1
  %15 = icmp eq i32 %14, 3
  br i1 %15, label %16, label %9, !llvm.loop !27

16:                                               ; preds = %9
  %17 = add nuw nsw i32 %6, 1
  %21 = icmp eq i32 %17, 4
  br i1 %21, label %22, label %5, !llvm.loop !29

22:                                               ; preds = %16
  %23 = zext i32 %2 to i64
  %24 = getelementptr inbounds float, ptr addrspace(1) %1, i64 %23
  store float %20, ptr addrspace(1) %24, align 4, !tbaa !23
  ret void
}

attributes #0 = { mustprogress nofree norecurse nosync nounwind "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }

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
!9 = !{ptr @nested_loop_constant_weight_walk, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"weights"}
!13 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!14 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!16 = !{!"air.compile.denorms_disable"}
!17 = !{!"air.compile.fast_math_enable"}
!18 = !{!"air.compile.framebuffer_fetch_enable"}
!19 = !{!"Apple metal version 32023.883 (metalfe-32023.883)"}
!20 = !{i32 2, i32 8, i32 0}
!21 = !{!"Metal", i32 4, i32 0, i32 0}
!22 = !{!"kernel_nested_loop_constant_weight_walk.metal"}
!23 = !{!24, !24, i64 0}
!24 = !{!"float", !25, i64 0}
!25 = !{!"omnipotent char", !26, i64 0}
!26 = !{!"Simple C++ TBAA"}
!27 = distinct !{!27, !28}
!28 = !{!"llvm.loop.mustprogress"}
!29 = distinct !{!29, !28}
