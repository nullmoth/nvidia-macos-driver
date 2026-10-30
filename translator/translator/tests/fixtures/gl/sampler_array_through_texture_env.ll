; ANGLE's texture environments, as Metal compiles them for a shadowed WebGL2 material (three.js, two lights, #449).
; Every sampler of the shader lives in one local `ANGLE_TextureEnvs` object; the shadow maps are an array<depth2d, 2>
; and an array<sampler, 2>, and each element's environment is handed by reference to a helper that is not inlined.
; The entry stores the element addresses through bitcasts of field GEPs, the helper reads them back through a GEP of
; the env it was handed, and it reaches the element itself through the one-member `metal::sampler`/`metal::depth2d`
; wrappers: the three spellings the inline retry has to see as one field.
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "spirv-unknown-vulkan1.2"

%"struct.metal::texture2d" = type { ptr addrspace(1) }
%"struct.metal::sampler" = type { ptr addrspace(2) }
%struct.ANGLE_TextureEnvs = type { %struct.ANGLE_TextureEnv, %"struct.metal::array.4" }
%struct.ANGLE_TextureEnv = type { ptr, ptr }
%"struct.metal::array.4" = type { [2 x %struct.ANGLE_TextureEnv.5] }
%struct.ANGLE_TextureEnv.5 = type { ptr, ptr }
%"struct.metal::depth2d" = type { ptr addrspace(1) }

define <4 x float> @main0(<4 x float> %0, <3 x float> %1, ptr byval([2 x ptr addrspace(1)]) %4, ptr byval([2 x ptr addrspace(2)]) %5, ptr addrspace(1) %6, ptr addrspace(2) %7) #0 {
  %9 = alloca %"struct.metal::texture2d", align 8
  %10 = alloca %"struct.metal::sampler", align 8
  %11 = alloca %struct.ANGLE_TextureEnvs, align 8
  %12 = getelementptr inbounds %"struct.metal::texture2d", ptr %9, i64 0, i32 0
  store ptr addrspace(1) %6, ptr %12, align 8
  %13 = getelementptr inbounds %"struct.metal::sampler", ptr %10, i64 0, i32 0
  store ptr addrspace(2) %7, ptr %13, align 8
  %14 = bitcast ptr %11 to ptr
  call void @llvm.lifetime.start.p0(ptr %11)
  %15 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 0, i32 0
  store ptr %9, ptr %15, align 8
  %16 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 0, i32 1
  store ptr %10, ptr %16, align 8
  %17 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 1
  %18 = bitcast ptr %17 to ptr
  store ptr %4, ptr %18, align 8
  %19 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 1, i32 0, i64 0, i32 1
  %20 = bitcast ptr %19 to ptr
  store ptr %5, ptr %20, align 8
  %21 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %4, i64 0, i64 1
  %22 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 1, i32 0, i64 1, i32 0
  %23 = bitcast ptr %22 to ptr
  store ptr %21, ptr %23, align 8
  %24 = getelementptr inbounds [2 x ptr addrspace(2)], ptr %5, i64 0, i64 1
  %25 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 1, i32 0, i64 1, i32 1
  %26 = bitcast ptr %25 to ptr
  store ptr %24, ptr %26, align 8
  %27 = shufflevector <3 x float> %1, <3 x float> poison, <2 x i32> <i32 0, i32 1>
  %28 = extractelement <3 x float> %1, i64 2
  %29 = getelementptr inbounds %"struct.metal::array.4", ptr %17, i64 0, i32 0, i64 0
  %30 = call fastcc float @shadow(ptr %29, <2 x float> %27, float %28)
  %31 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %11, i64 0, i32 1, i32 0, i64 1
  %32 = call fastcc float @shadow(ptr %31, <2 x float> %27, float %28)
  %33 = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %6, ptr addrspace(2) %7, <2 x float> %27, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %34 = extractvalue { <4 x float>, i8 } %33, 0
  %35 = insertelement <4 x float> %34, float %30, i64 0
  %36 = insertelement <4 x float> %35, float %32, i64 1
  call void @llvm.lifetime.end.p0(ptr %11)
  ret <4 x float> %36
}

define internal fastcc float @shadow(ptr %0, <2 x float> %1, float %2) #0 {
  %4 = getelementptr inbounds %struct.ANGLE_TextureEnv.5, ptr %0, i64 0, i32 0
  %5 = load ptr, ptr %4, align 8
  %6 = getelementptr inbounds %struct.ANGLE_TextureEnv.5, ptr %0, i64 0, i32 1
  %7 = load ptr, ptr %6, align 8
  %8 = getelementptr inbounds %"struct.metal::sampler", ptr %7, i64 0, i32 0
  %9 = load ptr addrspace(2), ptr %8, align 8
  %10 = getelementptr inbounds %"struct.metal::depth2d", ptr %5, i64 0, i32 0
  %11 = load ptr addrspace(1), ptr %10, align 8
  %12 = call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %11, ptr addrspace(2) %9, i32 1, <2 x float> %1, float %2, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %13 = extractvalue { float, i8 } %12, 0
  ret float %13
}

declare { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, float, i1, <2 x i32>, i1, float, float, i32) #1
declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32) #1
declare void @llvm.lifetime.start.p0(ptr) #2
declare void @llvm.lifetime.end.p0(ptr) #2

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind willreturn memory(argmem: read) }
attributes #2 = { nounwind }

!air.fragment = !{!9}
!air.version = !{!27}
!air.language_version = !{!28}

!9 = !{ptr @main0, !10, !12}
!10 = !{!11}
!11 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!12 = !{!13, !14, !19, !20, !21, !22}
!13 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos", !"air.arg_unused"}
!14 = !{i32 1, !"air.fragment_input", !"user(w)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float3", !"air.arg_name", !"world"}
!19 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 2, !"air.sample", !"air.arg_type_name", !"array<depth2d<float, sample>, 2>", !"air.arg_name", !"ANGLE_t"}
!20 = !{i32 3, !"air.sampler", !"air.location_index", i32 0, i32 2, !"air.arg_type_name", !"array<sampler, 2>", !"air.arg_name", !"ANGLE_s"}
!21 = !{i32 4, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"mapT"}
!22 = !{i32 5, !"air.sampler", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"mapS"}
!27 = !{i32 2, i32 7, i32 0}
!28 = !{!"Metal", i32 3, i32 2, i32 0}
