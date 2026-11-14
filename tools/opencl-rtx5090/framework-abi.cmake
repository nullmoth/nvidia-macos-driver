# Keep the framework load-compatible with applications linked to Apple's OpenCL ABI 1.
# This changes Mach-O packaging metadata; clvk's OpenCL platform/version reporting is unchanged.
function(clvk_isolated_framework_abi)
  set_target_properties(OpenCL PROPERTIES VERSION 1.0.0)
endfunction()
cmake_language(DEFER DIRECTORY "${CMAKE_SOURCE_DIR}" CALL clvk_isolated_framework_abi)
