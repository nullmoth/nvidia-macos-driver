%State = type { ptr addrspace(1) }
define void @kernel_main(ptr addrspace(1) %input, ptr addrspace(1) %output, i32 %index) {
entry:
  %state = alloca %State, align 8
  %field = getelementptr %State, ptr %state, i64 0, i32 0
  store ptr addrspace(1) %input, ptr %field
  call void @read_field(ptr %state, ptr addrspace(1) %output, i32 %index)
  ret void
}
define internal void @read_field(ptr %state, ptr addrspace(1) %output, i32 %index) {
entry:
  %active = icmp ult i32 %index, 8
  br i1 %active, label %read, label %skip
read:
  %field = getelementptr %State, ptr %state, i64 0, i32 0
  %resource = load ptr addrspace(1), ptr %field
  br label %join
skip:
  br label %join
join:
  %selected = phi ptr addrspace(1) [ %resource, %read ], [ null, %skip ]
  br i1 %active, label %use, label %done
use:
  %value = load i32, ptr addrspace(1) %selected
  store i32 %value, ptr addrspace(1) %output
  br label %done
done:
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @kernel_main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"input"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
