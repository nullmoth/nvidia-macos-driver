; ModuleID = 'shadow_pcf_gather_compare.metal'
source_filename = "shadow_pcf_gather_compare.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.0.0"

%struct._depth_2d_t = type opaque
%struct._sampler_t = type opaque

; Function Attrs: mustprogress nofree nounwind readonly willreturn
define <4 x float> @fs_main_gather_compare(<4 x float> %0, <4 x float> %1, %struct._depth_2d_t addrspace(1)* %2, %struct._sampler_t addrspace(2)* nocapture readonly %3) local_unnamed_addr #0 {
  %5 = shufflevector <4 x float> %1, <4 x float> poison, <2 x i32> <i32 0, i32 1>
  %6 = extractelement <4 x float> %1, i64 2
  %7 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(%struct._depth_2d_t addrspace(1)* nocapture readonly %2, %struct._sampler_t addrspace(2)* nocapture readonly %3, i32 1, <2 x float> %5, float %6, i1 true, <2 x i32> <i32 -1, i32 -1>, i32 0) #2, !alias.scope !24
  %8 = extractvalue { <4 x float>, i8 } %7, 0
  %9 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(%struct._depth_2d_t addrspace(1)* nocapture readonly %2, %struct._sampler_t addrspace(2)* nocapture readonly %3, i32 1, <2 x float> %5, float %6, i1 true, <2 x i32> <i32 1, i32 -1>, i32 0) #2, !alias.scope !24
  %10 = extractvalue { <4 x float>, i8 } %9, 0
  %11 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(%struct._depth_2d_t addrspace(1)* nocapture readonly %2, %struct._sampler_t addrspace(2)* nocapture readonly %3, i32 1, <2 x float> %5, float %6, i1 true, <2 x i32> <i32 -1, i32 1>, i32 0) #2, !alias.scope !24
  %12 = extractvalue { <4 x float>, i8 } %11, 0
  %13 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(%struct._depth_2d_t addrspace(1)* nocapture readonly %2, %struct._sampler_t addrspace(2)* nocapture readonly %3, i32 1, <2 x float> %5, float %6, i1 true, <2 x i32> <i32 1, i32 1>, i32 0) #2, !alias.scope !24
  %14 = extractvalue { <4 x float>, i8 } %13, 0
  %15 = fadd fast <4 x float> %10, %8
  %16 = fadd fast <4 x float> %15, %12
  %17 = fadd fast <4 x float> %16, %14
  %18 = fmul fast <4 x float> %17, <float 6.250000e-02, float 6.250000e-02, float 6.250000e-02, float 6.250000e-02>
  ret <4 x float> %18
}

; Function Attrs: argmemonly mustprogress nofree nounwind readonly willreturn
declare { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(%struct._depth_2d_t addrspace(1)* nocapture readonly, %struct._sampler_t addrspace(2)* nocapture readonly, i32, <2 x float>, float, i1, <2 x i32>, i32) local_unnamed_addr #1

attributes #0 = { mustprogress nofree nounwind readonly willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { argmemonly mustprogress nofree nounwind readonly willreturn }
attributes #2 = { argmemonly nounwind readonly willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.fragment = !{!9}
!air.compile_options = !{!17, !18, !19}
!llvm.ident = !{!20}
!air.version = !{!21}
!air.language_version = !{!22}
!air.source_file_name = !{!23}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 26, i32 5]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{<4 x float> (<4 x float>, <4 x float>, %struct._depth_2d_t addrspace(1)*, %struct._sampler_t addrspace(2)*)* @fs_main_gather_compare, !10, !12}
!10 = !{!11}
!11 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!12 = !{!13, !14, !15, !16}
!13 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"position", !"air.arg_unused"}
!14 = !{i32 1, !"air.fragment_input", !"generated(13shadow_uv_refDv4_f)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"shadow_uv_ref"}
!15 = !{i32 2, !"air.texture", !"air.location_index", i32 9, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"shadow_map"}
!16 = !{i32 3, !"air.sampler", !"air.location_index", i32 8, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"shadow_sampler"}
!17 = !{!"air.compile.denorms_disable"}
!18 = !{!"air.compile.fast_math_enable"}
!19 = !{!"air.compile.framebuffer_fetch_enable"}
!20 = !{!"Apple metal version 32023.883 (metalfe-32023.883)"}
!21 = !{i32 2, i32 8, i32 0}
!22 = !{!"Metal", i32 3, i32 1, i32 0}
!23 = !{!"shadow_pcf_gather_compare.metal"}
!24 = !{!25, !27}
!25 = distinct !{!25, !26, !"air-alias-scope-textures"}
!26 = distinct !{!26, !"air-alias-scopes(fs_main_gather_compare)"}
!27 = distinct !{!27, !26, !"air-alias-scope-samplers"}
