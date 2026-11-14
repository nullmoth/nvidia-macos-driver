; ModuleID = 'tile_avg.metal'
source_filename = "tile_avg.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

%"struct.metal::_imageblock_base" = type { %struct._imageblock_t addrspace(4)* }
%struct._imageblock_t = type opaque

; Function Attrs: convergent mustprogress nounwind
define void @tile_avg(%"struct.metal::_imageblock_base" %0, <4 x float> addrspace(3)* nocapture noundef "air-buffer-no-alias" %1, <4 x float> addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %2, i32 addrspace(2)* nocapture noundef readonly align 4 dereferenceable(4) "air-buffer-no-alias" %3, <2 x i16> noundef %4, <2 x i16> noundef %5, <2 x i16> noundef %6, i16 noundef %7) local_unnamed_addr #0 {
  %9 = tail call fast <4 x half> @air.load.implicit_imageblock.v4f16(i32 0, <2 x i16> %4, i32 0, i16 0) #5
  %10 = tail call fast <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half> %9) #6
  %11 = zext i16 %7 to i64
  %12 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %1, i64 %11
  store <4 x float> %10, <4 x float> addrspace(3)* %12, align 16, !tbaa !28, !alias.scope !31, !noalias !34
  tail call void @air.wg.barrier(i32 2, i32 1) #7
  %13 = extractelement <2 x i16> %5, i64 0
  %14 = extractelement <2 x i16> %5, i64 1
  %15 = mul i16 %13, %14
  %16 = icmp ult i16 %15, 2
  br i1 %16, label %19, label %17

17:                                               ; preds = %8
  %18 = zext i16 %7 to i32
  br label %26

19:                                               ; preds = %38, %8
  %20 = load <4 x float>, <4 x float> addrspace(3)* %1, align 16, !tbaa !28, !alias.scope !31, !noalias !34
  %21 = tail call fast float @air.convert.f.f32.u.i16(i16 %15) #6
  %22 = insertelement <4 x float> poison, float %21, i64 0
  %23 = shufflevector <4 x float> %22, <4 x float> poison, <4 x i32> zeroinitializer
  %24 = fdiv fast <4 x float> %20, %23
  %25 = icmp eq i16 %7, 0
  br i1 %25, label %40, label %50

26:                                               ; preds = %17, %38
  %27 = phi i16 [ %15, %17 ], [ %28, %38 ]
  %28 = lshr i16 %27, 1
  %29 = icmp ugt i16 %28, %7
  br i1 %29, label %30, label %38

30:                                               ; preds = %26
  %31 = zext i16 %28 to i32
  %32 = add nuw nsw i32 %31, %18
  %33 = zext i32 %32 to i64
  %34 = getelementptr inbounds <4 x float>, <4 x float> addrspace(3)* %1, i64 %33
  %35 = load <4 x float>, <4 x float> addrspace(3)* %34, align 16, !tbaa !28, !alias.scope !31, !noalias !34
  %36 = load <4 x float>, <4 x float> addrspace(3)* %12, align 16, !tbaa !28, !alias.scope !31, !noalias !34
  %37 = fadd fast <4 x float> %36, %35
  store <4 x float> %37, <4 x float> addrspace(3)* %12, align 16, !tbaa !28, !alias.scope !31, !noalias !34
  br label %38

38:                                               ; preds = %30, %26
  tail call void @air.wg.barrier(i32 2, i32 1) #7
  %39 = icmp ult i16 %27, 4
  br i1 %39, label %19, label %26, !llvm.loop !37

40:                                               ; preds = %19
  %41 = extractelement <2 x i16> %6, i64 1
  %42 = zext i16 %41 to i32
  %43 = load i32, i32 addrspace(2)* %3, align 4, !tbaa !39, !alias.scope !41, !noalias !42
  %44 = mul i32 %43, %42
  %45 = extractelement <2 x i16> %6, i64 0
  %46 = zext i16 %45 to i32
  %47 = add i32 %44, %46
  %48 = zext i32 %47 to i64
  %49 = getelementptr inbounds <4 x float>, <4 x float> addrspace(1)* %2, i64 %48
  store <4 x float> %24, <4 x float> addrspace(1)* %49, align 16, !tbaa !28, !alias.scope !43, !noalias !44
  br label %50

50:                                               ; preds = %40, %19
  %51 = tail call fast <4 x half> @air.convert.f.v4f16.f.v4f32(<4 x float> %24) #6
  tail call void @air.store.implicit_imageblock.v4f16(<4 x half> %51, i32 0, <2 x i16> %4, i32 0, i16 0) #8
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half>) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare float @air.convert.f.f32.u.i16(i16) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind readnone willreturn
declare <4 x half> @air.convert.f.v4f16.f.v4f32(<4 x float>) local_unnamed_addr #1

; Function Attrs: convergent mustprogress nounwind willreturn
declare void @air.wg.barrier(i32, i32) local_unnamed_addr #2

; Function Attrs: mustprogress nofree nounwind readonly willreturn
declare <4 x half> @air.load.implicit_imageblock.v4f16(i32, <2 x i16>, i32, i16) local_unnamed_addr #3

; Function Attrs: mustprogress nounwind willreturn writeonly
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16) local_unnamed_addr #4

attributes #0 = { convergent mustprogress nounwind "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="64" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind readnone willreturn }
attributes #2 = { convergent mustprogress nounwind willreturn }
attributes #3 = { mustprogress nofree nounwind readonly willreturn }
attributes #4 = { mustprogress nounwind willreturn writeonly }
attributes #5 = { nounwind readonly willreturn }
attributes #6 = { nounwind readnone willreturn }
attributes #7 = { convergent nounwind willreturn }
attributes #8 = { nounwind willreturn writeonly }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!21, !22, !23}
!llvm.ident = !{!24}
!air.version = !{!25}
!air.language_version = !{!26}
!air.source_file_name = !{!27}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (%"struct.metal::_imageblock_base", <4 x float> addrspace(3)*, <4 x float> addrspace(1)*, i32 addrspace(2)*, <2 x i16>, <2 x i16>, <2 x i16>, i16)* @tile_avg, !10, !11}
!10 = !{}
!11 = !{!12, !14, !15, !16, !17, !18, !19, !20}
!12 = !{i32 0, !"air.imageblock", !"implicit", !"air.struct_type_info", !13, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<Color0, layout_implicit>", !"air.arg_name", !"blk"}
!13 = !{i32 0, i32 8, i32 0, !"half4", !"c", !"air.render_target", i32 0}
!14 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"sum"}
!15 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"outAvg"}
!16 = !{i32 3, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"tilesX"}
!17 = !{i32 4, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"lid"}
!18 = !{i32 5, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tgsz"}
!19 = !{i32 6, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tile"}
!20 = !{i32 7, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"tidx"}
!21 = !{!"air.compile.denorms_disable"}
!22 = !{!"air.compile.fast_math_enable"}
!23 = !{!"air.compile.framebuffer_fetch_enable"}
!24 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!25 = !{i32 2, i32 7, i32 0}
!26 = !{!"Metal", i32 3, i32 2, i32 0}
!27 = !{!"tile_avg.metal"}
!28 = !{!29, !29, i64 0}
!29 = !{!"omnipotent char", !30, i64 0}
!30 = !{!"Simple C++ TBAA"}
!31 = !{!32}
!32 = distinct !{!32, !33, !"air-alias-scope-arg(1)"}
!33 = distinct !{!33, !"air-alias-scopes(tile_avg)"}
!34 = !{!35, !36}
!35 = distinct !{!35, !33, !"air-alias-scope-arg(2)"}
!36 = distinct !{!36, !33, !"air-alias-scope-arg(3)"}
!37 = distinct !{!37, !38}
!38 = !{!"llvm.loop.mustprogress"}
!39 = !{!40, !40, i64 0}
!40 = !{!"int", !29, i64 0}
!41 = !{!36}
!42 = !{!32, !35}
!43 = !{!35}
!44 = !{!32, !36}
