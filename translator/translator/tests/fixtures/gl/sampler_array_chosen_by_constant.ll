; Sampler descriptor array: Apple's GL passes the two samplers of a lightmap lookup as one
; `array<sampler, 2>` parameter. Here they arrive byval and the shader picks element 1 with a
; constant index, which is the shape the translator previously refused outright.
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64-apple-macosx"

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32) #2

define { <4 x float>, <4 x float> } @__main(<2 x float> %0, ptr addrspace(1) %1, ptr byval([2 x ptr addrspace(2)]) %2) #1 {
gldPluginBody:
  %slot = getelementptr inbounds [2 x ptr addrspace(2)], ptr %2, i32 0, i32 1
  %s = load ptr addrspace(2), ptr %slot, align 8
  %12 = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %1, ptr addrspace(2) %s, <2 x float> %0, i1 false, <2 x i32> zeroinitializer, i1 true, float 0.000000e+00, float 0.000000e+00, i32 0) #1
  %13 = extractvalue { <4 x float>, i8 } %12, 0
  %8 = insertvalue { <4 x float>, <4 x float> } undef, <4 x float> <float 0.0, float 0.0, float 0.0, float 1.0>, 0
  %9 = insertvalue { <4 x float>, <4 x float> } %8, <4 x float> %13, 1
  ret { <4 x float>, <4 x float> } %9
}

attributes #1 = { nounwind }
attributes #2 = { nounwind memory(read) }

!air.vertex = !{!0}
!air.version = !{!12}
!air.language_version = !{!13}
!llvm.module.flags = !{}

!0 = !{ptr @__main, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"#air_position"}
!3 = !{!"air.vertex_output", !"user(slot8)", !"air.arg_type_name", !"float4", !"air.arg_name", !"user(slot8)"}
!4 = !{!5, !6, !7}
!5 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, i32 1, !"attrib_0", !"air.arg_type_name", !"float2", !"air.arg_name", !"attrib_0"}
!6 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"vs_tex_0"}
!7 = !{i32 2, !"air.sampler", !"air.location_index", i32 0, i32 2, !"air.arg_type_name", !"array<sampler, 2>", !"air.arg_name", !"vs_smp"}
!12 = !{i32 2, i32 3, i32 0}
!13 = !{!"OpenGL GLSL", i32 1, i32 0, i32 0}
