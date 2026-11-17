%FragmentState = type { ptr addrspace(2) }
%UniformData = type { %ThemeColors, %ThemeSizes, <2 x float>, <2 x float>, float, float, i32, float }
%ThemeColors = type { <4 x float>, <4 x float>, <4 x float>, <4 x float> }
%ThemeSizes = type { float, float, float, float, float, float, float, float, float, float, float, float }

define <{ <4 x float> }> @fragment_main(ptr addrspace(2) %uniform_buf) {
entry:
  %state = alloca %FragmentState, align 8
  %field = getelementptr inbounds %FragmentState, ptr %state, i64 0, i32 0
  store ptr addrspace(2) %uniform_buf, ptr %field, align 8
  %color = call <4 x float> @read_object_select(ptr %state)
  %result = insertvalue <{ <4 x float> }> undef, <4 x float> %color, 0
  ret <{ <4 x float> }> %result
}

define internal <4 x float> @read_object_select(ptr %state) {
entry:
  %field = getelementptr inbounds %FragmentState, ptr %state, i64 0, i32 0
  %uniform_buf = load ptr addrspace(2), ptr %field, align 8
  %size_viewport_inv = getelementptr inbounds %UniformData, ptr addrspace(2) %uniform_buf, i64 0, i32 3
  %viewport = load <2 x float>, ptr addrspace(2) %size_viewport_inv, align 8
  %viewport_x = extractelement <2 x float> %viewport, i64 0
  %object_select = getelementptr inbounds %UniformData, ptr addrspace(2) %uniform_buf, i64 0, i32 0, i32 3
  %color = load <4 x float>, ptr addrspace(2) %object_select, align 16
  %result = insertelement <4 x float> %color, float %viewport_x, i64 0
  ret <4 x float> %result
}

!air.fragment = !{!0}
!0 = !{ptr @fragment_main, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"color"}
!3 = !{!4}
!4 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 144, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"UniformData", !"air.arg_name", !"uniform_buf"}
!5 = !{!"air.struct_type_info", !6, i32 0, i32 64, i32 0, !"ThemeColors", !"colors", !"air.struct_type_info", !7, i32 64, i32 48, i32 0, !"ThemeSizes", !"sizes", i32 112, i32 8, i32 0, !"float2", !"size_viewport", i32 120, i32 8, i32 0, !"float2", !"size_viewport_inv", i32 128, i32 4, i32 0, !"float", !"fresnel_mix_edit", i32 132, i32 4, i32 0, !"float", !"pixel_fac", i32 136, i32 4, i32 0, !"int", !"backface_culling", i32 140, i32 4, i32 0, !"float", !"_pad1"}
!6 = !{i32 0, i32 16, i32 0, !"float4", !"wire", i32 16, i32 16, i32 0, !"float4", !"wire_edit", i32 32, i32 16, i32 0, !"float4", !"active_object", i32 48, i32 16, i32 0, !"float4", !"object_select"}
!7 = !{i32 0, i32 4, i32 0, !"float", !"pixel", i32 4, i32 4, i32 0, !"float", !"object_center", i32 8, i32 4, i32 0, !"float", !"light_center", i32 12, i32 4, i32 0, !"float", !"light_circle", i32 16, i32 4, i32 0, !"float", !"light_circle_shadow", i32 20, i32 4, i32 0, !"float", !"vert", i32 24, i32 4, i32 0, !"float", !"edge", i32 28, i32 4, i32 0, !"float", !"face_dot", i32 32, i32 4, i32 0, !"float", !"checker", i32 36, i32 4, i32 0, !"float", !"vertex_gpencil", i32 40, i32 4, i32 0, !"float", !"_pad1", i32 44, i32 4, i32 0, !"float", !"_pad2"}
