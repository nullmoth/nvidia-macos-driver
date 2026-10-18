; ModuleID = 'nvmtl-air'
source_filename = "glsl shader"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64-apple-macosx"

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32) #2

define { <4 x float>, <4 x float> } @__main(<2 x float> %0, ptr addrspace(1) %1, ptr addrspace(1) %2, ptr addrspace(2) %3, ptr addrspace(2) %4) #1 {
gldPluginBody:
  %5 = call { <4 x float>, <4 x float> } @"#internal_main"(<2 x float> %0, ptr addrspace(1) %1, ptr addrspace(1) %2, ptr addrspace(2) %3, ptr addrspace(2) %4)
  ret { <4 x float>, <4 x float> } %5
}

define internal { <4 x float>, <4 x float> } @"#internal_main"(<2 x float> %0, ptr addrspace(1) %1, ptr addrspace(1) %2, ptr addrspace(2) %3, ptr addrspace(2) %4) #1 {
allocaBlock:
  %5 = alloca <4 x float>, align 16
  %6 = alloca i32, align 4
  br label %block

block:
  store i32 0, ptr %6, align 4
  call void @"#sample_lightmap@sampler2D"(ptr %5, ptr %6, <2 x float> %0, ptr addrspace(1) %1, i32 0, ptr addrspace(1) %2, i32 1, ptr addrspace(2) %3, ptr addrspace(2) %4)
  %7 = load <4 x float>, ptr %5, align 16
  %8 = insertvalue { <4 x float>, <4 x float> } undef, <4 x float> <float 0.0, float 0.0, float 0.0, float 1.0>, 0
  %9 = insertvalue { <4 x float>, <4 x float> } %8, <4 x float> %7, 1
  ret { <4 x float>, <4 x float> } %9
}

define internal void @"#sample_lightmap@sampler2D"(ptr %0, ptr %1, <2 x float> %2, ptr addrspace(1) %3, i32 %4, ptr addrspace(1) %5, i32 %6, ptr addrspace(2) %7, ptr addrspace(2) %8) #1 {
block:
  %9 = load i32, ptr %1, align 4
  switch i32 %9, label %afterSwitch [
    i32 1, label %case
  ]

case:
  br label %afterSwitch

afterSwitch:
  %10 = phi ptr addrspace(1) [ %3, %block ], [ %5, %case ]
  %11 = phi ptr addrspace(2) [ %7, %block ], [ %8, %case ]
  %12 = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %10, ptr addrspace(2) %11, <2 x float> %2, i1 false, <2 x i32> zeroinitializer, i1 true, float 0.000000e+00, float 0.000000e+00, i32 0) #1
  %13 = extractvalue { <4 x float>, i8 } %12, 0
  store <4 x float> %13, ptr %0, align 16
  ret void
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
!4 = !{!5, !6, !7, !8, !9}
!5 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, i32 1, !"attrib_0", !"air.arg_type_name", !"float2", !"air.arg_name", !"attrib_0"}
!6 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"vs_tex_0"}
!7 = !{i32 2, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"vs_tex_1"}
!8 = !{i32 3, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"vs_smp_0"}
!9 = !{i32 4, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"vs_smp_1"}
!12 = !{i32 2, i32 3, i32 0}
!13 = !{!"OpenGL GLSL", i32 1, i32 0, i32 0}
