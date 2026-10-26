; Owned synthetic fixture for the AIR single-level instanced triangle-intersection ABI.
; Not derived from a third-party metallib.
source_filename = "kernel_instance_as_intersect.metal"

define void @instance_as_intersect(ptr addrspace(1) %acceleration_structure, ptr addrspace(1) %intersection_table, ptr addrspace(1) %output) {
  %hit = call { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float> <float 0.0, float 0.0, float 1.0>, <3 x float> <float 0.0, float 0.0, float -1.0>, float 0.0, float 10.0, ptr addrspace(1) %acceleration_structure, i32 255, ptr addrspace(1) %intersection_table, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 3, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  %type = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 0
  %distance = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 1
  %primitive_id = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 2
  %geometry_id = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 3
  %instance_id = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 5
  %user_instance_id = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 6
  %barycentrics = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 7
  %front = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 8
  store i32 %type, ptr addrspace(1) %output, align 4
  %distance_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 1
  store float %distance, ptr addrspace(1) %distance_slot, align 4
  %primitive_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 2
  store i32 %primitive_id, ptr addrspace(1) %primitive_slot, align 4
  %geometry_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 3
  store i32 %geometry_id, ptr addrspace(1) %geometry_slot, align 4
  %instance_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 4
  store i32 %instance_id, ptr addrspace(1) %instance_slot, align 4
  %user_instance_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 5
  store i32 %user_instance_id, ptr addrspace(1) %user_instance_slot, align 4
  %bary_x = extractelement <2 x float> %barycentrics, i64 0
  %bary_x_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 6
  store float %bary_x, ptr addrspace(1) %bary_x_slot, align 4
  %bary_y = extractelement <2 x float> %barycentrics, i64 1
  %bary_y_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 7
  store float %bary_y, ptr addrspace(1) %bary_y_slot, align 4
  %front32 = zext i1 %front to i32
  %front_slot = getelementptr inbounds i32, ptr addrspace(1) %output, i64 8
  store i32 %front32, ptr addrspace(1) %front_slot, align 4
  ret void
}

declare { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.kernel = !{!0}
!0 = !{ptr @instance_as_intersect, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.instance_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"acceleration_structure"}
!4 = !{i32 1, !"air.intersection_function_table", !"air.location_index", i32 6, i32 1, !"air.read_write", !"air.arg_type_name", !"intersection_function_table<instancing, triangle_data>", !"air.arg_name", !"intersection_table"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 36, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
