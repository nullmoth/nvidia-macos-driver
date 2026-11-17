%State = type { ptr addrspace(2) }
%Matrix = type { [4 x <4 x float>] }
%ObjectMatrices = type { [2 x <4 x float>], <4 x float>, <4 x float> }

define <{ <4 x float> }> @vertex_main(ptr addrspace(2) %objects, i32 %instance) {
entry:
  %state = alloca %State, align 8
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0
  store ptr addrspace(2) %objects, ptr %field, align 8
  %position = call <4 x float> @read_translation(ptr %state, i32 %instance)
  %out = insertvalue <{ <4 x float> }> undef, <4 x float> %position, 0
  ret <{ <4 x float> }> %out
}

define internal <4 x float> @read_translation(ptr %state, i32 %instance) {
entry:
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0
  %objects = load ptr addrspace(2), ptr %field, align 8
  %index = zext i32 %instance to i64
  %translation = getelementptr inbounds %ObjectMatrices, ptr addrspace(2) %objects, i64 %index, i32 2
  %position = load <4 x float>, ptr addrspace(2) %translation, align 16
  ret <4 x float> %position
}

!air.vertex = !{!0}
!0 = !{ptr @vertex_main, !1, !3}
!1 = !{!2}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!3 = !{!4, !6}
!4 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 64, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"ObjectMatrices", !"air.arg_name", !"objects"}
!5 = !{i32 0, i32 16, i32 2, !"float4", !"orco_mul_bias", i32 32, i32 16, i32 0, !"float4", !"ob_color", i32 48, i32 16, i32 0, !"float4", !"infos"}
!6 = !{i32 1, !"air.instance_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"instance"}
