; ModuleID = 'tile_tg_half.metal'
source_filename = "tile_tg_half.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

; Function Attrs: convergent mustprogress nounwind willreturn
define void @tile_tg_half(half addrspace(3)* nocapture noundef "air-buffer-no-alias" %0, half addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %1, i16 noundef %2) local_unnamed_addr #0 {
  %4 = tail call fast half @air.convert.f.f16.u.i16(i16 %2) #3
  %5 = zext i16 %2 to i64
  %6 = getelementptr inbounds half, half addrspace(3)* %0, i64 %5
  store half %4, half addrspace(3)* %6, align 2, !tbaa !22, !alias.scope !26, !noalias !29
  tail call void @air.wg.barrier(i32 2, i32 1) #4
  %7 = xor i16 %2, 1
  %8 = zext i16 %7 to i64
  %9 = getelementptr inbounds half, half addrspace(3)* %0, i64 %8
  %10 = load half, half addrspace(3)* %9, align 2, !tbaa !22, !alias.scope !26, !noalias !29
  %11 = getelementptr inbounds half, half addrspace(1)* %1, i64 %5
  store half %10, half addrspace(1)* %11, align 2, !tbaa !22, !alias.scope !29, !noalias !26
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare half @air.convert.f.f16.u.i16(i16) local_unnamed_addr #1

; Function Attrs: convergent mustprogress nounwind willreturn
declare void @air.wg.barrier(i32, i32) local_unnamed_addr #2

attributes #0 = { convergent mustprogress nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { convergent mustprogress nounwind willreturn }
attributes #3 = { nounwind readnone willreturn }
attributes #4 = { convergent nounwind willreturn }

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
!9 = !{void (half addrspace(3)*, half addrspace(1)*, i16)* @tile_tg_half, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"mem"}
!13 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"out"}
!14 = !{i32 2, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"tidx"}
!15 = !{!"air.compile.denorms_disable"}
!16 = !{!"air.compile.fast_math_enable"}
!17 = !{!"air.compile.framebuffer_fetch_enable"}
!18 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!19 = !{i32 2, i32 7, i32 0}
!20 = !{!"Metal", i32 3, i32 2, i32 0}
!21 = !{!"tile_tg_half.metal"}
!22 = !{!23, !23, i64 0}
!23 = !{!"half", !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C++ TBAA"}
!26 = !{!27}
!27 = distinct !{!27, !28, !"air-alias-scope-arg(0)"}
!28 = distinct !{!28, !"air-alias-scopes(tile_tg_half)"}
!29 = !{!30}
!30 = distinct !{!30, !28, !"air-alias-scope-arg(1)"}
