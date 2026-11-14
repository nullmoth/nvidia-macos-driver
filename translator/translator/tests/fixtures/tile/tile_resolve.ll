; ModuleID = 'tile_resolve.metal'
source_filename = "tile_resolve.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%"struct.metal::_imageblock_base" = type { %struct._imageblock_t addrspace(4)* }
%struct._imageblock_t = type opaque
%struct._texture_2d_t = type opaque

; Function Attrs: mustprogress nounwind willreturn
define void @tile_resolve(%"struct.metal::_imageblock_base" %0, %struct._texture_2d_t addrspace(1)* %1, <2 x i16> noundef %2, <2 x i16> noundef %3, <2 x i16> noundef %4) local_unnamed_addr #0 {
  %6 = tail call i8 addrspace(4)* @air.imageblock_data(<2 x i16> %2, i32 0, i16 0) #4
  %7 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %4) #5
  %8 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %3) #5
  %9 = mul <2 x i32> %8, %7
  %10 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %2) #5
  %11 = add <2 x i32> %9, %10
  %12 = bitcast i8 addrspace(4)* %6 to <4 x half> addrspace(4)*
  %13 = load <4 x half>, <4 x half> addrspace(4)* %12, align 8
  %14 = getelementptr inbounds i8, i8 addrspace(4)* %6, i64 8
  %15 = bitcast i8 addrspace(4)* %14 to half addrspace(4)*
  %16 = load half, half addrspace(4)* %15, align 8, !tbaa !25
  %17 = fmul fast half %16, 0xH3000
  %18 = insertelement <4 x half> %13, half 0xH3C00, i64 3
  %19 = insertelement <4 x half> %18, half %17, i64 2
  tail call void @air.write_texture_2d.v4f16(%struct._texture_2d_t addrspace(1)* nocapture %1, <2 x i32> %11, <4 x half> %19, i32 0, i32 2) #6, !alias.scope !30
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16>) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nounwind readonly willreturn
declare i8 addrspace(4)* @air.imageblock_data(<2 x i16>, i32, i16) local_unnamed_addr #2

; Function Attrs: argmemonly mustprogress nounwind willreturn
declare void @air.write_texture_2d.v4f16(%struct._texture_2d_t addrspace(1)* nocapture, <2 x i32>, <4 x half>, i32, i32) local_unnamed_addr #3

attributes #0 = { mustprogress nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="64" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { mustprogress nofree nounwind readonly willreturn }
attributes #3 = { argmemonly mustprogress nounwind willreturn }
attributes #4 = { nounwind readonly willreturn }
attributes #5 = { nounwind readnone willreturn }
attributes #6 = { argmemonly nounwind willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!18, !19, !20}
!llvm.ident = !{!21}
!air.version = !{!22}
!air.language_version = !{!23}
!air.source_file_name = !{!24}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (%"struct.metal::_imageblock_base", %struct._texture_2d_t addrspace(1)*, <2 x i16>, <2 x i16>, <2 x i16>)* @tile_resolve, !10, !11}
!10 = !{}
!11 = !{!12, !14, !15, !16, !17}
!12 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<GB, layout_explicit>", !"air.arg_name", !"blk"}
!13 = !{i32 0, i32 8, i32 0, !"half4", !"a", i32 8, i32 2, i32 0, !"half", !"b"}
!14 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"out"}
!15 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"lid"}
!16 = !{i32 3, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tgsz"}
!17 = !{i32 4, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tile"}
!18 = !{!"air.compile.denorms_disable"}
!19 = !{!"air.compile.fast_math_enable"}
!20 = !{!"air.compile.framebuffer_fetch_enable"}
!21 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!22 = !{i32 2, i32 7, i32 0}
!23 = !{!"Metal", i32 3, i32 2, i32 0}
!24 = !{!"tile_resolve.metal"}
!25 = !{!26, !29, i64 8}
!26 = !{!"_ZTS2GB", !27, i64 0, !29, i64 8}
!27 = !{!"omnipotent char", !28, i64 0}
!28 = !{!"Simple C++ TBAA"}
!29 = !{!"half", !27, i64 0}
!30 = !{!31}
!31 = distinct !{!31, !32, !"air-alias-scope-textures"}
!32 = distinct !{!32, !"air-alias-scopes(tile_resolve)"}
