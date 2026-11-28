%VertexState = type { ptr }

@_ZL4base = internal addrspace(2) global i32 0, align 4
@_Z4base.MTL_FC_INIT_0_i = internal addrspace(2) externally_initialized constant i32 undef, section "air.fc_initializer", align 4

define internal void @_GLOBAL__sub_I_pointer_table() section "air.static_init" {
entry:
  %value = load i32, ptr addrspace(2) @_Z4base.MTL_FC_INIT_0_i, align 4
  store i32 %value, ptr addrspace(2) @_ZL4base, align 4
  ret void
}

define <{ <4 x float> }> @vertex_main(ptr addrspace(2) %buffer0, ptr addrspace(2) %buffer1, i32 %vertex_id) {
entry:
  %state = alloca %VertexState, align 8
  %buffers = alloca [2 x ptr addrspace(2)], align 8
  %first = getelementptr inbounds [2 x ptr addrspace(2)], ptr %buffers, i64 0, i64 0
  store ptr addrspace(2) %buffer0, ptr %first, align 8
  %second = getelementptr inbounds [2 x ptr addrspace(2)], ptr %buffers, i64 0, i64 1
  store ptr addrspace(2) %buffer1, ptr %second, align 8
  %field = getelementptr inbounds %VertexState, ptr %state, i64 0, i32 0
  store ptr %first, ptr %field, align 8
  %index = and i32 %vertex_id, 1
  %value = call float @read_buffer(ptr %state, i32 %index)
  %position0 = insertelement <4 x float> zeroinitializer, float %value, i64 0
  %position1 = insertelement <4 x float> %position0, float 1.000000e+00, i64 3
  %result = insertvalue <{ <4 x float> }> undef, <4 x float> %position1, 0
  ret <{ <4 x float> }> %result
}

define internal float @read_buffer(ptr %state, i32 %index) {
entry:
  %field = getelementptr inbounds %VertexState, ptr %state, i64 0, i32 0
  %buffers = load ptr, ptr %field, align 8
  %wide = zext i32 %index to i64
  %slot = getelementptr inbounds ptr addrspace(2), ptr %buffers, i64 %wide
  %buffer = load ptr addrspace(2), ptr %slot, align 8
  %value = load float, ptr addrspace(2) %buffer, align 4
  ret float %value
}

!air.vertex = !{!0}
!air.function_constants = !{!9}
!0 = !{ptr @vertex_main, !1, !3}
!1 = !{!2}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!3 = !{!4, !6, !8}
!4 = !{i32 0, !"air.buffer", !"air.location_index", ptr addrspace(2) @_ZL4base, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"buffer0"}
!6 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"buffer1"}
!8 = !{i32 2, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vertex_id"}
!9 = !{ptr addrspace(2) @_Z4base.MTL_FC_INIT_0_i, !"int", !"base", i32 0, i1 true}
