%State = type { ptr addrspace(2) }
%Uniform = type { <4 x float>, %Nested }
%Nested = type <{ <2 x float>, i32, [4 x i8] }>

define <{ <4 x float> }> @fragment_main(ptr addrspace(2) %uniform) {
entry:
  %state = alloca %State, align 8
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0
  store ptr addrspace(2) %uniform, ptr %field, align 8
  %color = call <4 x float> @read_nested(ptr %state)
  %out = insertvalue <{ <4 x float> }> undef, <4 x float> %color, 0
  ret <{ <4 x float> }> %out
}
define internal <4 x float> @read_nested(ptr %state) {
entry:
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0
  %uniform = load ptr addrspace(2), ptr %field, align 8
  %vptr = getelementptr inbounds %Uniform, ptr addrspace(2) %uniform, i64 0, i32 1, i32 0
  %v = load <2 x float>, ptr addrspace(2) %vptr, align 8
  %bptr = getelementptr inbounds %Uniform, ptr addrspace(2) %uniform, i64 0, i32 1, i32 2, i64 0
  %b = load i8, ptr addrspace(2) %bptr, align 1
  %color = call <4 x float> @use_nested(<2 x float> %v, i8 %b)
  ret <4 x float> %color
}
define internal <4 x float> @use_nested(<2 x float> %v, i8 %unused_padding) {
entry:
  %color = shufflevector <2 x float> %v, <2 x float> %v, <4 x i32> <i32 0, i32 1, i32 0, i32 1>
  ret <4 x float> %color
}
!air.fragment = !{!0}
!0 = !{ptr @fragment_main, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"color"}
!3 = !{!4}
!4 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Uniform", !"air.arg_name", !"uniform"}
!5 = !{i32 0, i32 16, i32 0, !"float4", !"header", !"air.struct_type_info", !6, i32 16, i32 16, i32 0, !"Nested", !"nested"}
!6 = !{i32 0, i32 8, i32 0, !"float2", !"value", i32 8, i32 4, i32 0, !"int", !"flag"}
