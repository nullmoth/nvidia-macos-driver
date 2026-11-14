; ModuleID = 'tile_gb2_dump.metal'
source_filename = "tile_gb2_dump.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%"struct.metal::_imageblock_base" = type { %struct._imageblock_t addrspace(4)* }
%struct._imageblock_t = type opaque

; Function Attrs: mustprogress nofree nounwind willreturn
define void @tile_gb2_dump(%"struct.metal::_imageblock_base" %0, i32 addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %1, i32 addrspace(2)* nocapture noundef readonly align 4 dereferenceable(4) "air-buffer-no-alias" %2, <2 x i16> noundef %3, <2 x i16> noundef %4, <2 x i16> noundef %5) local_unnamed_addr #0 {
  %7 = tail call i8 addrspace(4)* @air.imageblock_data(<2 x i16> %3, i32 0, i16 0) #3
  %8 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %5) #4
  %9 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %4) #4
  %10 = mul <2 x i32> %9, %8
  %11 = tail call <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16> %3) #4
  %12 = add <2 x i32> %10, %11
  %13 = extractelement <2 x i32> %12, i64 1
  %14 = load i32, i32 addrspace(2)* %2, align 4, !tbaa !26, !alias.scope !30, !noalias !33
  %15 = mul i32 %13, %14
  %16 = extractelement <2 x i32> %12, i64 0
  %17 = add i32 %15, %16
  %18 = shl i32 %17, 3
  %19 = zext i32 %18 to i64
  %20 = getelementptr inbounds i32, i32 addrspace(1)* %1, i64 %19
  %21 = bitcast i8 addrspace(4)* %7 to <4 x i32> addrspace(4)*
  %22 = load <4 x i32>, <4 x i32> addrspace(4)* %21, align 16
  %23 = extractelement <4 x i32> %22, i64 0
  store i32 %23, i32 addrspace(1)* %20, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %24 = extractelement <4 x i32> %22, i64 1
  %25 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 1
  store i32 %24, i32 addrspace(1)* %25, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %26 = extractelement <4 x i32> %22, i64 2
  %27 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 2
  store i32 %26, i32 addrspace(1)* %27, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %28 = extractelement <4 x i32> %22, i64 3
  %29 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 3
  store i32 %28, i32 addrspace(1)* %29, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %30 = getelementptr inbounds i8, i8 addrspace(4)* %7, i64 16
  %31 = bitcast i8 addrspace(4)* %30 to i32 addrspace(4)*
  %32 = load i32, i32 addrspace(4)* %31, align 16, !tbaa !35
  %33 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 4
  store i32 %32, i32 addrspace(1)* %33, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %34 = getelementptr inbounds i8, i8 addrspace(4)* %7, i64 20
  %35 = bitcast i8 addrspace(4)* %34 to i32 addrspace(4)*
  %36 = load i32, i32 addrspace(4)* %35, align 4, !tbaa !37
  %37 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 5
  store i32 %36, i32 addrspace(1)* %37, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %38 = getelementptr inbounds i8, i8 addrspace(4)* %7, i64 24
  %39 = bitcast i8 addrspace(4)* %38 to i32 addrspace(4)*
  %40 = load i32, i32 addrspace(4)* %39, align 8, !tbaa !38
  %41 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 6
  store i32 %40, i32 addrspace(1)* %41, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  %42 = getelementptr inbounds i8, i8 addrspace(4)* %7, i64 28
  %43 = load i8, i8 addrspace(4)* %42, align 4, !tbaa !39
  %44 = zext i8 %43 to i32
  %45 = getelementptr inbounds i32, i32 addrspace(1)* %20, i64 7
  store i32 %44, i32 addrspace(1)* %45, align 4, !tbaa !26, !alias.scope !33, !noalias !30
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare <2 x i32> @air.convert.u.v2i32.u.v2i16(<2 x i16>) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nounwind readonly willreturn
declare i8 addrspace(4)* @air.imageblock_data(<2 x i16>, i32, i16) local_unnamed_addr #2

attributes #0 = { mustprogress nofree nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="32" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { mustprogress nofree nounwind readonly willreturn }
attributes #3 = { nounwind readonly willreturn }
attributes #4 = { nounwind readnone willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!19, !20, !21}
!llvm.ident = !{!22}
!air.version = !{!23}
!air.language_version = !{!24}
!air.source_file_name = !{!25}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (%"struct.metal::_imageblock_base", i32 addrspace(1)*, i32 addrspace(2)*, <2 x i16>, <2 x i16>, <2 x i16>)* @tile_gb2_dump, !10, !11}
!10 = !{}
!11 = !{!12, !14, !15, !16, !17, !18}
!12 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 32, !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<GB2, layout_explicit>", !"air.arg_name", !"blk"}
!13 = !{i32 0, i32 16, i32 0, !"float4", !"f", i32 16, i32 4, i32 0, !"int", !"i", i32 20, i32 4, i32 0, !"uint", !"u", i32 24, i32 4, i32 0, !"uchar4", !"c", i32 28, i32 1, i32 0, !"uchar", !"b"}
!14 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!15 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"width"}
!16 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"lid"}
!17 = !{i32 4, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tgsz"}
!18 = !{i32 5, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tile"}
!19 = !{!"air.compile.denorms_disable"}
!20 = !{!"air.compile.fast_math_enable"}
!21 = !{!"air.compile.framebuffer_fetch_enable"}
!22 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!23 = !{i32 2, i32 7, i32 0}
!24 = !{!"Metal", i32 3, i32 2, i32 0}
!25 = !{!"tile_gb2_dump.metal"}
!26 = !{!27, !27, i64 0}
!27 = !{!"int", !28, i64 0}
!28 = !{!"omnipotent char", !29, i64 0}
!29 = !{!"Simple C++ TBAA"}
!30 = !{!31}
!31 = distinct !{!31, !32, !"air-alias-scope-arg(2)"}
!32 = distinct !{!32, !"air-alias-scopes(tile_gb2_dump)"}
!33 = !{!34}
!34 = distinct !{!34, !32, !"air-alias-scope-arg(1)"}
!35 = !{!36, !27, i64 16}
!36 = !{!"_ZTS3GB2", !28, i64 0, !27, i64 16, !27, i64 20, !28, i64 24, !28, i64 28}
!37 = !{!36, !27, i64 20}
!38 = !{!28, !28, i64 0}
!39 = !{!36, !28, i64 28}
