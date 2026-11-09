; A compute kernel with three [[stage_in]] attributes (float3, float4, float2): AIR passes each member as its own parameter.
source_filename = "k.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "spirv-unknown-vulkan1.2"

; Function Attrs: mustprogress nofree norecurse nosync nounwind willreturn memory(argmem: write)
define void @stage_copy(<3 x float> %0, <4 x float> %1, <2 x float> %2, ptr addrspace(1) noundef writeonly captures(none) "air-buffer-no-alias" %3, i32 noundef %4) local_unnamed_addr #0 {
  %6 = shufflevector <4 x float> %1, <4 x float> poison, <3 x i32> <i32 0, i32 1, i32 2>
  %7 = fadd fast <3 x float> %6, %0
  %8 = shufflevector <2 x float> %2, <2 x float> poison, <3 x i32> <i32 0, i32 1, i32 poison>
  %9 = insertelement <3 x float> %8, float 0.000000e+00, i64 2
  %10 = fadd fast <3 x float> %7, %9
  %11 = zext i32 %4 to i64
  %12 = getelementptr inbounds [3 x float], ptr addrspace(1) %3, i64 %11, i64 0
  %13 = extractelement <3 x float> %10, i64 0
  store float %13, ptr addrspace(1) %12, align 4, !tbaa !24, !alias.scope !28
  %14 = getelementptr inbounds [3 x float], ptr addrspace(1) %3, i64 %11, i64 1
  %15 = extractelement <3 x float> %10, i64 1
  store float %15, ptr addrspace(1) %14, align 4, !tbaa !24, !alias.scope !28
  %16 = getelementptr inbounds [3 x float], ptr addrspace(1) %3, i64 %11, i64 2
  %17 = extractelement <3 x float> %10, i64 2
  store float %17, ptr addrspace(1) %16, align 4, !tbaa !24, !alias.scope !28
  ret void
}

attributes #0 = { mustprogress nofree norecurse nosync nounwind willreturn memory(argmem: write) "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!17, !18, !19}
!llvm.ident = !{!20}
!air.version = !{!21}
!air.language_version = !{!22}
!air.source_file_name = !{!23}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{ptr @stage_copy, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14, !15, !16}
!12 = !{i32 0, !"air.stage_in", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"float3", !"air.arg_name", !"p"}
!13 = !{i32 1, !"air.stage_in", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"float4", !"air.arg_name", !"c"}
!14 = !{i32 2, !"air.stage_in", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
!15 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"packed_float3", !"air.arg_name", !"dst"}
!16 = !{i32 4, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!17 = !{!"air.compile.denorms_disable"}
!18 = !{!"air.compile.fast_math_enable"}
!19 = !{!"air.compile.framebuffer_fetch_enable"}
!20 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!21 = !{i32 2, i32 7, i32 0}
!22 = !{!"Metal", i32 3, i32 2, i32 0}
!23 = !{!"k.metal"}
!24 = !{!25, !25, i64 0}
!25 = !{!"float", !26, i64 0}
!26 = !{!"omnipotent char", !27, i64 0}
!27 = !{!"Simple C++ TBAA"}
!28 = !{!29}
!29 = distinct !{!29, !30, !"air-alias-scope-arg(3)"}
!30 = distinct !{!30, !"air-alias-scopes(stage_copy)"}

