; ModuleID = 'nvmtl-air'
source_filename = "glsl shader"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64-apple-macosx"

; Reduced from Dome Keeper (Godot's GL compatibility renderer): a GLSL helper `outline(vec2, sampler2D)` gets every
; texture/sampler pair plus a pointer to the texture unit, which the caller stores before each of its two calls, and
; picks the pair with a switch whose cases are not in unit order (the units Apple's GL assigned).

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32) #2

define { <4 x float> } @__main(<4 x float> %0, <2 x float> %1, ptr addrspace(1) %2, ptr addrspace(1) %3, ptr addrspace(1) %4, ptr addrspace(1) %5, ptr addrspace(2) %6, ptr addrspace(2) %7, ptr addrspace(2) %8, ptr addrspace(2) %9) #1 {
gldPluginBody:
  %10 = call { <4 x float> } @"#internal_main"(<2 x float> %1, ptr addrspace(1) %2, i32 0, ptr addrspace(1) %3, i32 1, ptr addrspace(1) %4, i32 2, ptr addrspace(1) %5, i32 3, ptr addrspace(2) %6, ptr addrspace(2) %7, ptr addrspace(2) %8, ptr addrspace(2) %9)
  ret { <4 x float> } %10
}

define { <4 x float> } @"#internal_main"(<2 x float> %0, ptr addrspace(1) %1, i32 %2, ptr addrspace(1) %3, i32 %4, ptr addrspace(1) %5, i32 %6, ptr addrspace(1) %7, i32 %8, ptr addrspace(2) %9, ptr addrspace(2) %10, ptr addrspace(2) %11, ptr addrspace(2) %12) #1 {
allocaBlock:
  %13 = alloca <2 x float>, align 8
  %14 = alloca i32, align 4
  %15 = alloca float, align 4
  %16 = alloca float, align 4
  br label %block

block:
  store <2 x float> %0, ptr %13, align 8
  store i32 3, ptr %14, align 4
  call void @"#outline@vec2@sampler2D"(ptr %13, ptr %14, ptr %15, ptr addrspace(1) %1, i32 0, ptr addrspace(1) %3, i32 1, ptr addrspace(1) %5, i32 2, ptr addrspace(1) %7, i32 3, ptr addrspace(2) %9, ptr addrspace(2) %10, ptr addrspace(2) %11, ptr addrspace(2) %12)
  %17 = fmul <2 x float> %0, <float 2.000000e+00, float 2.000000e+00>
  store <2 x float> %17, ptr %13, align 8
  store i32 3, ptr %14, align 4
  call void @"#outline@vec2@sampler2D"(ptr %13, ptr %14, ptr %16, ptr addrspace(1) %1, i32 0, ptr addrspace(1) %3, i32 1, ptr addrspace(1) %5, i32 2, ptr addrspace(1) %7, i32 3, ptr addrspace(2) %9, ptr addrspace(2) %10, ptr addrspace(2) %11, ptr addrspace(2) %12)
  %18 = load float, ptr %15, align 4
  %19 = load float, ptr %16, align 4
  %20 = insertelement <4 x float> <float 0.0, float 0.0, float 0.0, float 1.0>, float %18, i32 0
  %21 = insertelement <4 x float> %20, float %19, i32 1
  %22 = insertvalue { <4 x float> } undef, <4 x float> %21, 0
  ret { <4 x float> } %22
}

define internal void @"#outline@vec2@sampler2D"(ptr %0, ptr %1, ptr %2, ptr addrspace(1) %3, i32 %4, ptr addrspace(1) %5, i32 %6, ptr addrspace(1) %7, i32 %8, ptr addrspace(1) %9, i32 %10, ptr addrspace(2) %11, ptr addrspace(2) %12, ptr addrspace(2) %13, ptr addrspace(2) %14) #1 {
block:
  %15 = load <2 x float>, ptr %0, align 8
  %16 = load i32, ptr %1, align 4
  switch i32 %16, label %afterSwitch [
    i32 0, label %case
    i32 3, label %case1
    i32 1, label %case2
  ]

case:
  br label %afterSwitch

case1:
  br label %afterSwitch

case2:
  br label %afterSwitch

afterSwitch:
  %17 = phi ptr addrspace(1) [ %3, %block ], [ %5, %case ], [ %7, %case1 ], [ %9, %case2 ]
  %18 = phi ptr addrspace(2) [ %11, %block ], [ %12, %case ], [ %13, %case1 ], [ %14, %case2 ]
  %19 = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %17, ptr addrspace(2) %18, <2 x float> %15, i1 false, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #1
  %20 = extractvalue { <4 x float>, i8 } %19, 0
  %21 = extractelement <4 x float> %20, i32 0
  store float %21, ptr %2, align 4
  ret void
}

attributes #1 = { nounwind }
attributes #2 = { nounwind memory(read) }

!air.fragment = !{!0}
!air.version = !{!20}
!air.language_version = !{!21}
!llvm.module.flags = !{}

!0 = !{ptr @__main, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7, !8, !9, !10, !11, !12, !13}
!4 = !{i32 0, !"air.fragment_input", !"user(slot12)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"user(slot12)"}
!5 = !{i32 1, !"air.fragment_input", !"user(slot14)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"user(slot14)"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"fs_tex_0"}
!7 = !{i32 3, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"fs_tex_1"}
!8 = !{i32 4, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"fs_tex_2"}
!9 = !{i32 5, !"air.texture", !"air.location_index", i32 3, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"fs_tex_3"}
!10 = !{i32 6, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"fs_smp_0"}
!11 = !{i32 7, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"fs_smp_1"}
!12 = !{i32 8, !"air.sampler", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"fs_smp_2"}
!13 = !{i32 9, !"air.sampler", !"air.location_index", i32 3, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"fs_smp_3"}
!20 = !{i32 2, i32 3, i32 0}
!21 = !{!"OpenGL GLSL", i32 1, i32 0, i32 0}
