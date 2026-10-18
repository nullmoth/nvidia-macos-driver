; ModuleID = 'nvmtl-air'
source_filename = "glsl shader"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64-apple-macosx"

declare <3 x float> @air.convert.f.v3f32.s.v3i32(<3 x i32>) #0

define { <4 x float> } @__main(<3 x float> %0, ptr addrspace(2) %1, ptr addrspace(2) %2) #1 {
gldPluginBody:
  %3 = call { <4 x float> } @"#internal_main"(<3 x float> %0, ptr addrspace(2) %1, ptr addrspace(2) %2)
  ret { <4 x float> } %3
}

define internal { <4 x float> } @"#internal_main"(<3 x float> %0, ptr addrspace(2) %1, ptr addrspace(2) %2) #1 {
allocaBlock:
  %3 = alloca <3 x i32>, align 16
  %4 = alloca <3 x i32>, align 16
  br label %globalBlock

globalBlock:
  %5 = bitcast ptr addrspace(2) %1 to ptr addrspace(2)
  %6 = getelementptr float, ptr addrspace(2) %5, i32 0
  %7 = bitcast ptr addrspace(2) %2 to ptr addrspace(2)
  %8 = getelementptr float, ptr addrspace(2) %7, i32 4
  br label %block

block:
  %9 = bitcast ptr addrspace(2) %6 to ptr addrspace(2)
  %10 = load <3 x i32>, ptr addrspace(2) %9, align 16
  store <3 x i32> %10, ptr %3, align 16
  %11 = load <3 x i32>, ptr %3, align 16
  %12 = bitcast ptr addrspace(2) %8 to ptr addrspace(2)
  %13 = load <3 x i32>, ptr addrspace(2) %12, align 16
  store <3 x i32> %13, ptr %4, align 16
  %14 = load <3 x i32>, ptr %4, align 16
  %15 = sub <3 x i32> %11, %14
  %16 = call reassoc nsz arcp contract afn <3 x float> @air.convert.f.v3f32.s.v3i32(<3 x i32> %15)
  %17 = fadd <3 x float> %0, %16
  %18 = shufflevector <3 x float> %17, <3 x float> undef, <4 x i32> <i32 0, i32 1, i32 2, i32 2>
  %19 = insertelement <4 x float> %18, float 1.000000e+00, i64 3
  %20 = insertvalue { <4 x float> } undef, <4 x float> %19, 0
  ret { <4 x float> } %20
}

attributes #0 = { nounwind memory(none) }
attributes #1 = { nounwind }

!air.vertex = !{!0}
!air.version = !{!7}
!air.language_version = !{!8}
!llvm.module.flags = !{}

!0 = !{ptr @__main, !1, !3}
!1 = !{!2}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"#air_position"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, i32 1, !"attrib_0", !"air.arg_type_name", !"float3", !"air.arg_name", !"attrib_0"}
!5 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 6, i32 1, !"air.read", !"air.arg_type_name", !"uchar", !"air.arg_name", !"vs_ubo_1"}
!6 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 64, !"air.location_index", i32 7, i32 1, !"air.read", !"air.arg_type_name", !"uchar", !"air.arg_name", !"vs_ubo_3"}
!7 = !{i32 2, i32 3, i32 0}
!8 = !{!"OpenGL GLSL", i32 1, i32 0, i32 0}
