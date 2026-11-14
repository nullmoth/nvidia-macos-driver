; ModuleID = 'tile_tg_write.metal'
source_filename = "tile_tg_write.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

; Function Attrs: argmemonly mustprogress nofree norecurse nosync nounwind willreturn writeonly
define void @tile_tg_write(i32 addrspace(3)* nocapture noundef writeonly "air-buffer-no-alias" %0, <2 x i16> noundef %1, i16 noundef %2) local_unnamed_addr #0 {
  %4 = extractelement <2 x i16> %1, i64 1
  %5 = zext i16 %4 to i32
  %6 = shl i32 %5, 24
  %7 = extractelement <2 x i16> %1, i64 0
  %8 = zext i16 %7 to i32
  %9 = shl nuw i32 %8, 16
  %10 = zext i16 %2 to i32
  %11 = or i32 %9, %10
  %12 = or i32 %11, %6
  %13 = zext i16 %2 to i64
  %14 = getelementptr inbounds i32, i32 addrspace(3)* %0, i64 %13
  store i32 %12, i32 addrspace(3)* %14, align 4, !tbaa !22, !alias.scope !26
  ret void
}

attributes #0 = { argmemonly mustprogress nofree norecurse nosync nounwind willreturn writeonly "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="32" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }

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
!9 = !{void (i32 addrspace(3)*, <2 x i16>, i16)* @tile_tg_write, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"mem"}
!13 = !{i32 1, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tile"}
!14 = !{i32 2, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"tidx"}
!15 = !{!"air.compile.denorms_disable"}
!16 = !{!"air.compile.fast_math_enable"}
!17 = !{!"air.compile.framebuffer_fetch_enable"}
!18 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!19 = !{i32 2, i32 7, i32 0}
!20 = !{!"Metal", i32 3, i32 2, i32 0}
!21 = !{!"tile_tg_write.metal"}
!22 = !{!23, !23, i64 0}
!23 = !{!"int", !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C++ TBAA"}
!26 = !{!27}
!27 = distinct !{!27, !28, !"air-alias-scope-arg(0)"}
!28 = distinct !{!28, !"air-alias-scopes(tile_tg_write)"}
