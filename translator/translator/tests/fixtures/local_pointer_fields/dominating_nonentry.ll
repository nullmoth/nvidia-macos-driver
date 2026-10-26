%Capture = type { i32, ptr }
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg)
define void @main(ptr addrspace(1) %out, i32 %n) {
entry:
  %object = alloca %Capture, align 8
  %a = alloca i32, align 4
  %b = alloca i32, align 4
  store i32 7, ptr %a, align 4
  store i32 11, ptr %b, align 4
  %condition = icmp eq i32 %n, 0
  br i1 %condition, label %left, label %right
left:
  br label %setup
right:
  br label %setup
setup:
  %field = getelementptr inbounds %Capture, ptr %object, i64 0, i32 1
  store ptr %a, ptr %field, align 8
  %first = load ptr, ptr %field, align 8
  br label %loop
loop:
  %p = phi ptr [ %first, %setup ], [ %later, %latch ]
  %i = phi i32 [ 0, %setup ], [ %next, %latch ]
  %value = load i32, ptr %p, align 4
  store i32 %value, ptr addrspace(1) %out, align 4
  %next = add i32 %i, 1
  %again = icmp ult i32 %next, %n
  br i1 %again, label %latch, label %exit
latch:
  %later = load ptr, ptr %field, align 8
  br label %loop
exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"n"}
