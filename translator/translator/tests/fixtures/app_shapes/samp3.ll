; ModuleID = 'samp3.metal'
source_filename = "samp3.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.0.0"

%struct._texture_2d_t = type opaque
%"struct.metal::sampler" = type { %struct._sampler_t addrspace(2)* }
%struct._sampler_t = type opaque
%"struct.metal::texture2d" = type { %struct._texture_2d_t addrspace(1)* }

; Function Attrs: argmemonly convergent mustprogress nofree nounwind readonly willreturn
define <4 x float> @_Z3smpN5metal9texture2dIfLNS_6accessE0ELNS_16memory_coherenceE0EvEERNS_7samplerEDv2_f(%struct._texture_2d_t addrspace(1)* nocapture readonly %0, %"struct.metal::sampler"* nocapture noundef nonnull readonly align 8 dereferenceable(8) %1, <2 x float> noundef %2) local_unnamed_addr #0 {
  %4 = getelementptr inbounds %"struct.metal::sampler", %"struct.metal::sampler"* %1, i64 0, i32 0
  %5 = load %struct._sampler_t addrspace(2)*, %struct._sampler_t addrspace(2)** %4, align 8, !tbaa.struct !25
  %6 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(%struct._texture_2d_t addrspace(1)* nocapture readonly %0, %struct._sampler_t addrspace(2)* nocapture readonly %5, <2 x float> %2, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #3
  %7 = extractvalue { <4 x float>, i8 } %6, 0
  ret <4 x float> %7
}

; Function Attrs: convergent mustprogress nofree nounwind readonly willreturn
define <4 x float> @fs(<4 x float> %0, <2 x float> %1, [2 x %struct._texture_2d_t addrspace(1)*]* nocapture readonly byval([2 x %struct._texture_2d_t addrspace(1)*]) %2, [2 x %struct._sampler_t addrspace(2)*]* nocapture readonly byval([2 x %struct._sampler_t addrspace(2)*]) %3, i32 addrspace(2)* nocapture noundef readonly align 4 dereferenceable(4) "air-buffer-no-alias" %4) local_unnamed_addr #1 {
  %6 = load i32, i32 addrspace(2)* %4, align 4, !tbaa !30, !alias.scope !32, !noalias !35
  %7 = zext i32 %6 to i64
  %8 = bitcast [2 x %struct._texture_2d_t addrspace(1)*]* %2 to [2 x %"struct.metal::texture2d"]*
  %9 = getelementptr inbounds [2 x %"struct.metal::texture2d"], [2 x %"struct.metal::texture2d"]* %8, i64 0, i64 %7, i32 0
  %10 = load %struct._texture_2d_t addrspace(1)*, %struct._texture_2d_t addrspace(1)** %9, align 8, !alias.scope !38, !noalias !39
  %11 = bitcast [2 x %struct._sampler_t addrspace(2)*]* %3 to [2 x %"struct.metal::sampler"]*
  %12 = getelementptr inbounds [2 x %"struct.metal::sampler"], [2 x %"struct.metal::sampler"]* %11, i64 0, i64 %7, i32 0
  %13 = load %struct._sampler_t addrspace(2)*, %struct._sampler_t addrspace(2)** %12, align 8, !tbaa.struct !25, !alias.scope !40, !noalias !41
  %14 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(%struct._texture_2d_t addrspace(1)* nocapture readonly %10, %struct._sampler_t addrspace(2)* nocapture readonly %13, <2 x float> %1, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #3
  %15 = extractvalue { <4 x float>, i8 } %14, 0
  ret <4 x float> %15
}

; Function Attrs: argmemonly convergent mustprogress nofree nounwind readonly willreturn
declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(%struct._texture_2d_t addrspace(1)* nocapture readonly, %struct._sampler_t addrspace(2)* nocapture readonly, <2 x float>, i1, <2 x i32>, i1, float, float, i32) local_unnamed_addr #2

attributes #0 = { argmemonly convergent mustprogress nofree nounwind readonly willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { convergent mustprogress nofree nounwind readonly willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #2 = { argmemonly convergent mustprogress nofree nounwind readonly willreturn }
attributes #3 = { argmemonly convergent nounwind readonly willreturn }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.fragment = !{!9}
!air.compile_options = !{!18, !19, !20}
!llvm.ident = !{!21}
!air.version = !{!22}
!air.language_version = !{!23}
!air.source_file_name = !{!24}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 26, i32 5]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{<4 x float> (<4 x float>, <2 x float>, [2 x %struct._texture_2d_t addrspace(1)*]*, [2 x %struct._sampler_t addrspace(2)*]*, i32 addrspace(2)*)* @fs, !10, !12}
!10 = !{!11}
!11 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!12 = !{!13, !14, !15, !16, !17}
!13 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"p", !"air.arg_unused"}
!14 = !{i32 1, !"air.fragment_input", !"generated(2uvDv2_f)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
!15 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 2, !"air.sample", !"air.arg_type_name", !"array<texture2d<float, sample>, 2>", !"air.arg_name", !"ts"}
!16 = !{i32 3, !"air.sampler", !"air.location_index", i32 0, i32 2, !"air.arg_type_name", !"array<sampler, 2>", !"air.arg_name", !"ss"}
!17 = !{i32 4, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!18 = !{!"air.compile.denorms_disable"}
!19 = !{!"air.compile.fast_math_enable"}
!20 = !{!"air.compile.framebuffer_fetch_enable"}
!21 = !{!"Apple metal version 32023.883 (metalfe-32023.883)"}
!22 = !{i32 2, i32 8, i32 0}
!23 = !{!"Metal", i32 4, i32 0, i32 0}
!24 = !{!"/src/samp3.metal"}
!25 = !{i64 0, i64 8, !26}
!26 = !{!27, !27, i64 0}
!27 = !{!"__metal_sampler_t", !28, i64 0}
!28 = !{!"omnipotent char", !29, i64 0}
!29 = !{!"Simple C++ TBAA"}
!30 = !{!31, !31, i64 0}
!31 = !{!"int", !28, i64 0}
!32 = !{!33}
!33 = distinct !{!33, !34, !"air-alias-scope-arg(4)"}
!34 = distinct !{!34, !"air-alias-scopes(fs)"}
!35 = !{!36, !37}
!36 = distinct !{!36, !34, !"air-alias-scope-textures"}
!37 = distinct !{!37, !34, !"air-alias-scope-samplers"}
!38 = !{!36}
!39 = !{!37, !33}
!40 = !{!37}
!41 = !{!36, !33}
