; ModuleID = 'atomics.metal'
source_filename = "atomics.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v29-apple-macosx27.0.0"

%"struct.metal::_atomic" = type { i32 }
%"struct.metal::_atomic.0" = type { i32 }
%"struct.metal::_atomic.1" = type { float }

; Function Attrs: mustprogress nounwind willreturn
define void @k(%"struct.metal::_atomic" addrspace(1)* nocapture noundef "air-buffer-no-alias" %0, %"struct.metal::_atomic.0" addrspace(1)* nocapture noundef "air-buffer-no-alias" %1, %"struct.metal::_atomic.1" addrspace(1)* nocapture noundef "air-buffer-no-alias" %2, %"struct.metal::_atomic" addrspace(3)* nocapture noundef "air-buffer-no-alias" %3, i32 noundef %4) local_unnamed_addr #0 {
  %6 = alloca i32, align 4
  %7 = getelementptr inbounds %"struct.metal::_atomic", %"struct.metal::_atomic" addrspace(1)* %0, i64 0, i32 0
  %8 = tail call i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %9 = tail call i32 @air.atomic.global.sub.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %10 = getelementptr inbounds %"struct.metal::_atomic.0", %"struct.metal::_atomic.0" addrspace(1)* %1, i64 0, i32 0
  %11 = tail call i32 @air.atomic.global.max.s.i32(i32 addrspace(1)* nocapture %10, i32 3, i32 0, i32 2, i32 0, i1 false) #3
  %12 = tail call i32 @air.atomic.global.min.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %13 = tail call i32 @air.atomic.global.and.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %14 = tail call i32 @air.atomic.global.or.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %15 = tail call i32 @air.atomic.global.xor.u.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %16 = tail call i32 @air.atomic.global.xchg.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %17 = bitcast i32* %6 to i8*
  call void @llvm.lifetime.start.p0i8(i64 4, i8* nonnull %17) #4
  store i32 1, i32* %6, align 4, !tbaa !27
  %18 = call i32 @air.atomic.global.cmpxchg.weak.i32(i32 addrspace(1)* nocapture %7, i32* nocapture nonnull %6, i32 %4, i32 0, i32 0, i32 2, i32 0, i1 false) #3
  call void @llvm.lifetime.end.p0i8(i64 4, i8* nonnull %17) #4
  tail call void @air.atomic.global.store.i32(i32 addrspace(1)* nocapture %7, i32 %4, i32 0, i32 2, i32 0, i1 false) #3
  %19 = tail call i32 @air.atomic.global.load.i32(i32 addrspace(1)* nocapture %7, i32 0, i32 2, i32 0, i1 false) #3
  tail call void @air.atomic.global.store.i32(i32 addrspace(1)* nocapture %10, i32 %19, i32 0, i32 2, i32 0, i1 false) #3
  %20 = getelementptr inbounds %"struct.metal::_atomic.1", %"struct.metal::_atomic.1" addrspace(1)* %2, i64 0, i32 0
  %21 = tail call fast float @air.atomic.global.add.f32(float addrspace(1)* nocapture %20, float 1.000000e+00, i32 0, i32 2, i32 0, i1 false) #3
  %22 = getelementptr inbounds %"struct.metal::_atomic", %"struct.metal::_atomic" addrspace(3)* %3, i64 0, i32 0
  %23 = tail call i32 @air.atomic.local.add.u.i32(i32 addrspace(3)* nocapture %22, i32 %4, i32 0, i32 1, i32 0, i1 false) #3
  tail call void @air.atomic.fence(i32 1, i32 5, i32 2) #3
  ret void
}

; Function Attrs: argmemonly mustprogress nocallback nofree nosync nounwind willreturn
declare void @llvm.lifetime.start.p0i8(i64 immarg, i8* nocapture) #1

; Function Attrs: argmemonly mustprogress nocallback nofree nosync nounwind willreturn
declare void @llvm.lifetime.end.p0i8(i64 immarg, i8* nocapture) #1

; Function Attrs: mustprogress nounwind willreturn
declare void @air.atomic.fence(i32, i32, i32) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.sub.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.max.s.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.min.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.and.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.or.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.xor.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.xchg.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.cmpxchg.weak.i32(i32 addrspace(1)* nocapture, i32* nocapture, i32, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare void @air.atomic.global.store.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.global.load.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare float @air.atomic.global.add.f32(float addrspace(1)* nocapture, float, i32, i32, i32, i1) local_unnamed_addr #2

; Function Attrs: mustprogress nounwind willreturn
declare i32 @air.atomic.local.add.u.i32(i32 addrspace(3)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2

attributes #0 = { mustprogress nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { argmemonly mustprogress nocallback nofree nosync nounwind willreturn }
attributes #2 = { mustprogress nounwind willreturn }
attributes #3 = { nounwind willreturn }
attributes #4 = { nounwind }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!20, !21, !22}
!llvm.ident = !{!23}
!air.version = !{!24}
!air.language_version = !{!25}
!air.source_file_name = !{!26}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (%"struct.metal::_atomic" addrspace(1)*, %"struct.metal::_atomic.0" addrspace(1)*, %"struct.metal::_atomic.1" addrspace(1)*, %"struct.metal::_atomic" addrspace(3)*, i32)* @k, !10, !11}
!10 = !{}
!11 = !{!12, !14, !16, !18, !19}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !13, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"c"}
!13 = !{i32 0, i32 4, i32 0, !"uint", !"__s"}
!14 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !15, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"s"}
!15 = !{i32 0, i32 4, i32 0, !"int", !"__s"}
!16 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !17, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"f"}
!17 = !{i32 0, i32 4, i32 0, !"float", !"__s"}
!18 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !13, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"l"}
!19 = !{i32 4, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!20 = !{!"air.compile.denorms_disable"}
!21 = !{!"air.compile.fast_math_enable"}
!22 = !{!"air.compile.framebuffer_fetch_enable"}
!23 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!24 = !{i32 2, i32 9, i32 0}
!25 = !{!"Metal", i32 4, i32 1, i32 0}
!26 = !{!"atomics.metal"}
!27 = !{!28, !28, i64 0}
!28 = !{!"int", !29, i64 0}
!29 = !{!"omnipotent char", !30, i64 0}
!30 = !{!"Simple C++ TBAA"}
