#!/bin/sh
# fanpower.sh (Linux 4090 guest, root): which RM controls NVIDIA's nvidia-smi (NVML) uses for fan speed and power, with full
# parameter dumps: one isolated query per value (easy to map), the values themselves, and a full -q. Output in /root/fanpower.
T=/root/nvtrace.so; O=/root/fanpower; rm -rf $O; mkdir -p $O; cd $O
nvidia-smi --version > version.txt 2>&1
nvidia-smi --query-gpu=fan.speed,power.draw,power.draw.average,power.draw.instant,power.limit,power.default_limit,power.min_limit,power.max_limit,enforced.power.limit,temperature.gpu,temperature.memory,clocks.gr --format=csv > values-before.txt 2>&1
for q in fan.speed power.draw power.limit power.max_limit temperature.memory; do
  LD_PRELOAD=$T NVTRACE_OUT=$O/q-$q.log NVTRACE_CTRLMAX=8192 nvidia-smi --query-gpu=$q --format=csv,noheader > $O/q-$q.txt 2>&1
done
LD_PRELOAD=$T NVTRACE_OUT=$O/q-all.log NVTRACE_CTRLMAX=8192 nvidia-smi -q > $O/q-all.txt 2>&1
nvidia-smi --query-gpu=fan.speed,power.draw,power.draw.average,power.draw.instant,power.limit,power.default_limit,power.min_limit,power.max_limit,enforced.power.limit,temperature.gpu,temperature.memory,clocks.gr --format=csv > values-after.txt 2>&1
ls -la $O
