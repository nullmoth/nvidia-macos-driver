; Reduced from Civilization VI's fs_main_SP_Civ6FireFXShaders_blended_main (Metal shader converted from D3D):
; a 4-tap shadow PCF with depth2d::gather_compare at offsets (-1,-1), (1,-1), (-1,1), (1,1), averaged.
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v28-apple-macosx26.7.0"

define <{ <4 x float> }> @fs_main_gather_compare(<4 x float> %fsinput.0, <4 x float> %fsinput.1, ptr addrspace(1) %T4_t9.coerce, ptr addrspace(2) readonly captures(none) %S2_s8.coerce) local_unnamed_addr #0 {
entry:
  %uv = shufflevector <4 x float> %fsinput.1, <4 x float> poison, <2 x i32> <i32 0, i32 1>
  %ref = extractelement <4 x float> %fsinput.1, i64 2
  %g0 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(ptr addrspace(1) readonly captures(none) %T4_t9.coerce, ptr addrspace(2) readonly captures(none) %S2_s8.coerce, i32 1, <2 x float> %uv, float %ref, i1 true, <2 x i32> splat (i32 -1), i32 0)
  %v0 = extractvalue { <4 x float>, i8 } %g0, 0
  %g1 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(ptr addrspace(1) readonly captures(none) %T4_t9.coerce, ptr addrspace(2) readonly captures(none) %S2_s8.coerce, i32 1, <2 x float> %uv, float %ref, i1 true, <2 x i32> <i32 1, i32 -1>, i32 0)
  %v1 = extractvalue { <4 x float>, i8 } %g1, 0
  %g2 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(ptr addrspace(1) readonly captures(none) %T4_t9.coerce, ptr addrspace(2) readonly captures(none) %S2_s8.coerce, i32 1, <2 x float> %uv, float %ref, i1 true, <2 x i32> <i32 -1, i32 1>, i32 0)
  %v2 = extractvalue { <4 x float>, i8 } %g2, 0
  %g3 = tail call { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(ptr addrspace(1) readonly captures(none) %T4_t9.coerce, ptr addrspace(2) readonly captures(none) %S2_s8.coerce, i32 1, <2 x float> %uv, float %ref, i1 true, <2 x i32> splat (i32 1), i32 0)
  %v3 = extractvalue { <4 x float>, i8 } %g3, 0
  %s01 = fadd fast <4 x float> %v0, %v1
  %s23 = fadd fast <4 x float> %v2, %v3
  %s = fadd fast <4 x float> %s01, %s23
  %avg = fmul fast <4 x float> %s, splat (float 6.250000e-02)
  %r = insertvalue <{ <4 x float> }> undef, <4 x float> %avg, 0
  ret <{ <4 x float> }> %r
}

declare { <4 x float>, i8 } @air.gather_compare_depth_2d.f32(ptr addrspace(1) readonly captures(none), ptr addrspace(2) readonly captures(none), i32, <2 x float>, float, i1, <2 x i32>, i32) local_unnamed_addr #1

attributes #0 = { convergent mustprogress nofree nounwind willreturn memory(read) }
attributes #1 = { convergent mustprogress nofree nounwind willreturn memory(read) }

!air.version = !{!9}
!air.language_version = !{!10}
!air.fragment = !{!14}

!9 = !{i32 2, i32 8, i32 0}
!10 = !{!"Metal", i32 3, i32 1, i32 0}
!14 = !{ptr @fs_main_gather_compare, !15, !17}
!15 = !{!16}
!16 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"fs_o0"}
!17 = !{!18, !19, !32, !36}
!18 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"fs_v0"}
!19 = !{i32 1, !"air.fragment_input", !"user(TEXCOORD0)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"fs_v1"}
!32 = !{i32 2, !"air.texture", !"air.location_index", i32 9, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"T4_t9"}
!36 = !{i32 3, !"air.sampler", !"air.location_index", i32 8, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"S2_s8"}
