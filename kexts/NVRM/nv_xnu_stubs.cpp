/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
extern "C" {
#include "nvlink_os.h"
}
#include <IOKit/IOLib.h>

extern "C" void nv_xnu_power_state(bool *acPlugged, bool *haveBattery);   // nv_xnu_power.cpp

extern "C" {

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_d3cold_dsm_for_upstream_port(nv_state_t *, NvU8 *, NvU32, NvU32, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_acpi_d3cold_dsm_for_upstream_port");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_ddc_method(nv_state_t *, void *, NvU32 *, NvBool)
{
    NV_XNU_STUB_HIT("nv_acpi_ddc_method");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_dod_method(nv_state_t *, NvU32 *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_acpi_dod_method");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_dsm_method(nv_state_t *, NvU8 *, NvU32, NvBool, NvU32, void *, NvU16, NvU32 *, void *, NvU16 *)
{
    NV_XNU_STUB_HIT("nv_acpi_dsm_method");
    return NV_ERR_NOT_SUPPORTED;
}

// Answered for real in nv_xnu_power.cpp: a mobile GPU picks its clock and power policy from this, and it used to be
// told "not supported" on every start and every wake.
NV_STATUS NV_API_CALL nv_acpi_get_powersource(NvU32 *ac_plugged)
{
    if (!ac_plugged) return NV_ERR_INVALID_ARGUMENT;
    // A read of what the last registry walk found. The firmware asks this with its own locks held, so nothing here
    // may take another lock, allocate, or touch the registry.
    bool ac = true, haveBattery = false;
    nv_xnu_power_state(&ac, &haveBattery);
    *ac_plugged = ac ? 1 : 0;
    return NV_OK;
}

__attribute__((weak)) NvBool NV_API_CALL nv_acpi_is_battery_present(void)
{
    NV_XNU_STUB_HIT("nv_acpi_is_battery_present");
    return NV_FALSE;
}

__attribute__((weak)) void NV_API_CALL nv_acpi_methods_init(NvU32 *)
{
    NV_XNU_STUB_HIT("nv_acpi_methods_init");

}

__attribute__((weak)) void NV_API_CALL nv_acpi_methods_uninit(void)
{
    NV_XNU_STUB_HIT("nv_acpi_methods_uninit");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_mux_method(nv_state_t *, NvU32 *, NvU32, const char *)
{
    NV_XNU_STUB_HIT("nv_acpi_mux_method");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acpi_rom_method(nv_state_t *, NvU32 *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_acpi_rom_method");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_acquire_fabric_mgmt_cap(int, int*)
{
    NV_XNU_STUB_HIT("nv_acquire_fabric_mgmt_cap");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_acquire_mmap_lock(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_acquire_mmap_lock");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_add_mapping_context_to_file(nv_state_t *, nv_usermap_access_params_t*, NvU32, void *, NvU64, NvU32)
{
    NV_XNU_STUB_HIT("nv_add_mapping_context_to_file");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_alias_pages(nv_state_t *, NvU32, NvU64, NvU32, NvU32, NvU64, NvU64 *, NvBool, void **)
{
    NV_XNU_STUB_HIT("nv_alias_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void* NV_API_CALL nv_alloc_kernel_mapping(nv_state_t *, void *, NvU64, NvU32, NvU64, void **)
{
    NV_XNU_STUB_HIT("nv_alloc_kernel_mapping");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_alloc_pages(nv_state_t *, NvU32, NvU64, NvBool, NvU32, NvBool, NvBool, NvS32, NvU64 *, void **)
{
    NV_XNU_STUB_HIT("nv_alloc_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_alloc_user_mapping(nv_state_t *, void *, NvU64, NvU32, NvU64, NvU32, NvU64 *, void **)
{
    NV_XNU_STUB_HIT("nv_alloc_user_mapping");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_allow_runtime_suspend(nv_state_t *nv)
{
    NV_XNU_STUB_HIT("nv_allow_runtime_suspend");

}

__attribute__((weak)) void NV_API_CALL nv_audio_dynamic_power(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_audio_dynamic_power");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_bpmp_send_mrq(nv_state_t *, NvU32, const void *, NvU32, void *, NvU32, NvS32 *, NvS32 *)
{
    NV_XNU_STUB_HIT("nv_bpmp_send_mrq");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_cancel_nano_timer(nv_state_t *, nv_nano_timer_t *)
{
    NV_XNU_STUB_HIT("nv_cancel_nano_timer");

}

__attribute__((weak)) void NV_API_CALL nv_control_soc_irqs(nv_state_t *, NvBool bEnable)
{
    NV_XNU_STUB_HIT("nv_control_soc_irqs");

}

__attribute__((weak)) void NV_API_CALL nv_create_nano_timer(nv_state_t *, void *pTmrEvent, nv_nano_timer_t **)
{
    NV_XNU_STUB_HIT("nv_create_nano_timer");

}

__attribute__((weak)) void NV_API_CALL nv_destroy_nano_timer(nv_state_t *nv, nv_nano_timer_t *)
{
    NV_XNU_STUB_HIT("nv_destroy_nano_timer");

}

__attribute__((weak)) void NV_API_CALL nv_disable_clk(nv_state_t *, TEGRASOC_WHICH_CLK)
{
    NV_XNU_STUB_HIT("nv_disable_clk");

}

__attribute__((weak)) void NV_API_CALL nv_disallow_runtime_suspend(nv_state_t *nv)
{
    NV_XNU_STUB_HIT("nv_disallow_runtime_suspend");

}

__attribute__((weak)) void NV_API_CALL nv_dma_cache_invalidate(nv_dma_device_t *, void *)
{
    NV_XNU_STUB_HIT("nv_dma_cache_invalidate");

}

__attribute__((weak)) void* NV_API_CALL nv_dma_get_dev_pagemap(NvU64)
{
    NV_XNU_STUB_HIT("nv_dma_get_dev_pagemap");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_import_dma_buf(nv_dma_device_t *, struct dma_buf *, NvBool, NvU32 *, struct sg_table **, nv_dma_buf_t **)
{
    NV_XNU_STUB_HIT("nv_dma_import_dma_buf");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_import_from_fd(nv_dma_device_t *, NvS32, NvBool, NvU32 *, struct sg_table **, nv_dma_buf_t **)
{
    NV_XNU_STUB_HIT("nv_dma_import_from_fd");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_import_sgt(nv_dma_device_t *, struct sg_table *, struct drm_gem_object *)
{
    NV_XNU_STUB_HIT("nv_dma_import_sgt");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_map_alloc(nv_dma_device_t *, NvU64, NvU64 *, NvBool, NvBool, void **)
{
    NV_XNU_STUB_HIT("nv_dma_map_alloc");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_map_mmio(nv_dma_device_t *, NvU64, NvU64 *)
{
    NV_XNU_STUB_HIT("nv_dma_map_mmio");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_map_peer(nv_dma_device_t *, nv_dma_device_t *, NvU8, NvU64, NvU64 *)
{
    NV_XNU_STUB_HIT("nv_dma_map_peer");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_dma_put_dev_pagemap(void *)
{
    NV_XNU_STUB_HIT("nv_dma_put_dev_pagemap");

}

__attribute__((weak)) void NV_API_CALL nv_dma_release_dma_buf(nv_dma_buf_t *)
{
    NV_XNU_STUB_HIT("nv_dma_release_dma_buf");

}

__attribute__((weak)) void NV_API_CALL nv_dma_release_sgt(struct sg_table *, struct drm_gem_object *)
{
    NV_XNU_STUB_HIT("nv_dma_release_sgt");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_dma_unmap_alloc(nv_dma_device_t *, NvU64, NvU64 *, void **)
{
    NV_XNU_STUB_HIT("nv_dma_unmap_alloc");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_dma_unmap_mmio(nv_dma_device_t *, NvU64, NvU64)
{
    NV_XNU_STUB_HIT("nv_dma_unmap_mmio");

}

__attribute__((weak)) void NV_API_CALL nv_dma_unmap_peer(nv_dma_device_t *, NvU64, NvU64)
{
    NV_XNU_STUB_HIT("nv_dma_unmap_peer");

}

__attribute__((weak)) NvBool NV_API_CALL nv_dynamic_power_available(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_dynamic_power_available");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_enable_clk(nv_state_t *, TEGRASOC_WHICH_CLK)
{
    NV_XNU_STUB_HIT("nv_enable_clk");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_flush_coherent_cpu_cache_range(nv_state_t *nv, NvU64 cpu_virtual, NvU64 size)
{
    NV_XNU_STUB_HIT("nv_flush_coherent_cpu_cache_range");

}

__attribute__((weak)) void NV_API_CALL nv_free_kernel_mapping(nv_state_t *, void *, void *, void *)
{
    NV_XNU_STUB_HIT("nv_free_kernel_mapping");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_free_pages(nv_state_t *, NvU32, NvBool, NvU32, void *)
{
    NV_XNU_STUB_HIT("nv_free_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_free_user_mapping(nv_state_t *, void *, NvU64, void *)
{
    NV_XNU_STUB_HIT("nv_free_user_mapping");

}

__attribute__((weak)) nv_state_t* NV_API_CALL nv_get_adapter_state(NvU32, NvU8, NvU8)
{
    NV_XNU_STUB_HIT("nv_get_adapter_state");
    return NULL;
}

__attribute__((weak)) NvBool NV_API_CALL nv_get_all_mappings_revoked_locked(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_get_all_mappings_revoked_locked");
    return NV_FALSE;
}

__attribute__((weak)) nv_state_t* NV_API_CALL nv_get_ctl_state(void)
{
    NV_XNU_STUB_HIT("nv_get_ctl_state");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_current_irq_priv_data(nv_state_t *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_current_irq_priv_data");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) nv_soc_irq_type_t NV_API_CALL nv_get_current_irq_type(nv_state_t*)
{
    NV_XNU_STUB_HIT("nv_get_current_irq_type");
    return (nv_soc_irq_type_t)0;
}

__attribute__((weak)) NvU32 NV_API_CALL nv_get_dev_minor(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_get_dev_minor");
    return (NvU32)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_device_memory_config(nv_state_t *, NvU64 *, NvU64 *, NvU64 *, NvU64 *, NvU32 *, NvS32 *)
{
    NV_XNU_STUB_HIT("nv_get_device_memory_config");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_get_disp_smmu_stream_ids(nv_state_t *, NvU32 *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_disp_smmu_stream_ids");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_egm_info(nv_state_t *, NvU64 *, NvU64 *, NvS32 *)
{
    NV_XNU_STUB_HIT("nv_get_egm_info");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvS32 NV_API_CALL nv_get_event(nv_file_private_t *, nv_event_t *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_event");
    return (NvS32)0;
}

__attribute__((weak)) nv_file_private_t* NV_API_CALL nv_get_file_private(NvS32, NvBool, void **)
{
    NV_XNU_STUB_HIT("nv_get_file_private");
    return NULL;
}

__attribute__((weak)) const void* NV_API_CALL nv_get_firmware(nv_state_t *, nv_firmware_type_t, nv_firmware_chip_family_t, const void **, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_firmware");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_max_freq(nv_state_t *, TEGRASOC_WHICH_CLK, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_max_freq");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_num_dpaux_instances(nv_state_t *nv, NvU32 *num_instances)
{
    NV_XNU_STUB_HIT("nv_get_num_dpaux_instances");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_num_phys_pages(void *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_num_phys_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_phys_pages(void *, void *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_phys_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_get_screen_info(nv_state_t *, NvU64 *, NvU32 *, NvU32 *, NvU32 *, NvU32 *, NvU64 *)
{
    NV_XNU_STUB_HIT("nv_get_screen_info");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_syncpoint_aperture(NvU32, NvU64 *, NvU64 *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_syncpoint_aperture");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_get_tegra_brightness_level(nv_state_t *, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_get_tegra_brightness_level");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_get_updated_emu_seg(NvU32 *start, NvU32 *end)
{
    NV_XNU_STUB_HIT("nv_get_updated_emu_seg");

}

__attribute__((weak)) NvBool NV_API_CALL nv_grdma_pci_topology_supported(nv_state_t *, nv_dma_device_t *)
{
    NV_XNU_STUB_HIT("nv_grdma_pci_topology_supported");
    return NV_FALSE;
}

__attribute__((weak)) void* NV_API_CALL nv_i2c_add_adapter(nv_state_t *, NvU32)
{
    NV_XNU_STUB_HIT("nv_i2c_add_adapter");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_i2c_bus_status(nv_state_t *, NvU32, NvS32 *, NvS32 *)
{
    NV_XNU_STUB_HIT("nv_i2c_bus_status");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_i2c_del_adapter(nv_state_t *, void *)
{
    NV_XNU_STUB_HIT("nv_i2c_del_adapter");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_i2c_transfer(nv_state_t *, NvU32, NvU8, nv_i2c_msg_t *, int)
{
    NV_XNU_STUB_HIT("nv_i2c_transfer");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_i2c_unregister_clients(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_i2c_unregister_clients");

}

__attribute__((weak)) void NV_API_CALL nv_idle_holdoff(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_idle_holdoff");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_imp_enable_disable_rfl(nv_state_t *nv, NvBool bEnable)
{
    NV_XNU_STUB_HIT("nv_imp_enable_disable_rfl");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_imp_get_import_data(TEGRA_IMP_IMPORT_DATA *)
{
    NV_XNU_STUB_HIT("nv_imp_get_import_data");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_imp_get_uefi_data(nv_state_t *nv, NvU32 *iso_bw_kbps, NvU32 *floor_bw_kbps)
{
    NV_XNU_STUB_HIT("nv_imp_get_uefi_data");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_imp_icc_set_bw(nv_state_t *nv, NvU32 avg_bw_kbps, NvU32 floor_bw_kbps)
{
    NV_XNU_STUB_HIT("nv_imp_icc_set_bw");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_indicate_idle(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_indicate_idle");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_indicate_not_idle(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_indicate_not_idle");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvBool NV_API_CALL nv_is_chassis_notebook(void)
{
    NV_XNU_STUB_HIT("nv_is_chassis_notebook");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL nv_is_gpu_accessible(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_is_gpu_accessible");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_log_error(nv_state_t *, NvU32, const char *, va_list)
{
    NV_XNU_STUB_HIT("nv_log_error");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvBool NV_API_CALL nv_match_gpu_os_info(nv_state_t *, void *)
{
    NV_XNU_STUB_HIT("nv_match_gpu_os_info");
    return NV_FALSE;
}

__attribute__((weak)) void NV_API_CALL nv_pci_cxl_set_caching(nv_state_t *, NvBool)
{
    NV_XNU_STUB_HIT("nv_pci_cxl_set_caching");

}

__attribute__((weak)) void NV_API_CALL nv_pci_tegra_pm_deinit(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_pci_tegra_pm_deinit");

}

__attribute__((weak)) NvBool NV_API_CALL nv_pci_tegra_pm_init(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_pci_tegra_pm_init");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL nv_platform_supports_s0ix(void)
{
    NV_XNU_STUB_HIT("nv_platform_supports_s0ix");
    return NV_FALSE;
}

__attribute__((weak)) void NV_API_CALL nv_post_event(nv_event_t *, NvHandle, NvU32, NvU32, NvU16, NvBool)
{
    NV_XNU_STUB_HIT("nv_post_event");

}

__attribute__((weak)) int NV_API_CALL nv_printf(NvU32 debuglevel, const char *printf_format, ...)
{
    NV_XNU_STUB_HIT("nv_printf");
    return (int)0;
}

__attribute__((weak)) void NV_API_CALL nv_put_file_private(void *)
{
    NV_XNU_STUB_HIT("nv_put_file_private");

}

__attribute__((weak)) void NV_API_CALL nv_put_firmware(const void *)
{
    NV_XNU_STUB_HIT("nv_put_firmware");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_register_peer_io_mem(nv_state_t *, NvU64 *, NvU64, void **)
{
    NV_XNU_STUB_HIT("nv_register_peer_io_mem");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_register_phys_pages(nv_state_t *, NvU64 *, NvU64, NvU32, void **)
{
    NV_XNU_STUB_HIT("nv_register_phys_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_register_sgt(nv_state_t *, NvU64 *, NvU64, NvU32, void **, struct sg_table *, void *, NvBool)
{
    NV_XNU_STUB_HIT("nv_register_sgt");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_register_user_pages(nv_state_t *, NvU64, NvU64 *, void *, void **, NvBool)
{
    NV_XNU_STUB_HIT("nv_register_user_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_release_mmap_lock(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_release_mmap_lock");

}

__attribute__((weak)) NvBool NV_API_CALL nv_requires_dma_remap(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_requires_dma_remap");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_revoke_gpu_mappings(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_revoke_gpu_mappings");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvBool NV_API_CALL nv_s2idle_pm_configured(void)
{
    NV_XNU_STUB_HIT("nv_s2idle_pm_configured");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_schedule_uvm_drain_p2p(NvU8 *)
{
    NV_XNU_STUB_HIT("nv_schedule_uvm_drain_p2p");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_schedule_uvm_isr(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_schedule_uvm_isr");

}

__attribute__((weak)) void NV_API_CALL nv_schedule_uvm_resume_p2p(NvU8 *)
{
    NV_XNU_STUB_HIT("nv_schedule_uvm_resume_p2p");

}

__attribute__((weak)) void NV_API_CALL nv_set_dma_address_size(nv_state_t *, NvU32)
{
    NV_XNU_STUB_HIT("nv_set_dma_address_size");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_set_freq(nv_state_t *, TEGRASOC_WHICH_CLK, NvU32)
{
    NV_XNU_STUB_HIT("nv_set_freq");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_set_safe_to_mmap_locked(nv_state_t *, NvBool)
{
    NV_XNU_STUB_HIT("nv_set_safe_to_mmap_locked");

}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_set_tegra_brightness_level(nv_state_t *, NvU32)
{
    NV_XNU_STUB_HIT("nv_set_tegra_brightness_level");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nv_start_nano_timer(nv_state_t *nv, nv_nano_timer_t *, NvU64 timens)
{
    NV_XNU_STUB_HIT("nv_start_nano_timer");

}

__attribute__((weak)) NvS32 NV_API_CALL nv_start_rc_timer(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_start_rc_timer");
    return (NvS32)0;
}

__attribute__((weak)) NvS32 NV_API_CALL nv_stop_rc_timer(nv_state_t *)
{
    NV_XNU_STUB_HIT("nv_stop_rc_timer");
    return (NvS32)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_tegra_dce_client_ipc_send_recv(NvU32, void *, NvU32)
{
    NV_XNU_STUB_HIT("nv_tegra_dce_client_ipc_send_recv");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_tegra_dce_register_ipc_client(NvU32, void *, nvTegraDceClientIpcCallback, NvU32 *)
{
    NV_XNU_STUB_HIT("nv_tegra_dce_register_ipc_client");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL nv_tegra_dce_unregister_ipc_client(NvU32)
{
    NV_XNU_STUB_HIT("nv_tegra_dce_unregister_ipc_client");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU32 NV_API_CALL nv_tegra_get_rm_interface_type(NvU32)
{
    NV_XNU_STUB_HIT("nv_tegra_get_rm_interface_type");
    return (NvU32)0;
}

__attribute__((weak)) void NV_API_CALL nv_unregister_peer_io_mem(nv_state_t *, void *)
{
    NV_XNU_STUB_HIT("nv_unregister_peer_io_mem");

}

__attribute__((weak)) void NV_API_CALL nv_unregister_phys_pages(nv_state_t *, void *)
{
    NV_XNU_STUB_HIT("nv_unregister_phys_pages");

}

__attribute__((weak)) void NV_API_CALL nv_unregister_sgt(nv_state_t *, struct sg_table **, void **, void *)
{
    NV_XNU_STUB_HIT("nv_unregister_sgt");

}

__attribute__((weak)) void NV_API_CALL nv_unregister_user_pages(nv_state_t *, NvU64, void **, void **)
{
    NV_XNU_STUB_HIT("nv_unregister_user_pages");

}

__attribute__((weak)) NvlStatus NV_API_CALL nvlink_acquire_fabric_mgmt_cap(void *osPrivate, NvU64 capDescriptor)
{
    NV_XNU_STUB_HIT("nvlink_acquire_fabric_mgmt_cap");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL nvlink_assert(int expression)
{
    NV_XNU_STUB_HIT("nvlink_assert");

}

__attribute__((weak)) void NV_API_CALL nvlink_free(void *)
{
    NV_XNU_STUB_HIT("nvlink_free");

}

__attribute__((weak)) NvU64 NV_API_CALL nvlink_get_platform_time(void)
{
    NV_XNU_STUB_HIT("nvlink_get_platform_time");
    return (NvU64)0;
}

__attribute__((weak)) int NV_API_CALL nvlink_is_admin(void)
{
    NV_XNU_STUB_HIT("nvlink_is_admin");
    return (int)0;
}

__attribute__((weak)) int NV_API_CALL nvlink_is_fabric_manager(void *osPrivate)
{
    NV_XNU_STUB_HIT("nvlink_is_fabric_manager");
    return (int)0;
}

__attribute__((weak)) void * NV_API_CALL nvlink_malloc(NvLength)
{
    NV_XNU_STUB_HIT("nvlink_malloc");
    return NULL;
}

__attribute__((weak)) void * NV_API_CALL nvlink_memcpy(void *, const void *, NvLength)
{
    NV_XNU_STUB_HIT("nvlink_memcpy");
    return NULL;
}

__attribute__((weak)) void * NV_API_CALL nvlink_memset(void *, int, NvLength)
{
    NV_XNU_STUB_HIT("nvlink_memset");
    return NULL;
}

__attribute__((weak)) void NV_API_CALL nvlink_sleep(unsigned int ms)
{
    NV_XNU_STUB_HIT("nvlink_sleep");

}

__attribute__((weak)) int NV_API_CALL nvlink_strcmp(const char *, const char *)
{
    NV_XNU_STUB_HIT("nvlink_strcmp");
    return (int)0;
}

__attribute__((weak)) char * NV_API_CALL nvlink_strcpy(char *, const char *)
{
    NV_XNU_STUB_HIT("nvlink_strcpy");
    return NULL;
}

__attribute__((weak)) NvLength NV_API_CALL nvlink_strlen(const char *)
{
    NV_XNU_STUB_HIT("nvlink_strlen");
    return (NvLength)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_acquire_mutex(void *)
{
    NV_XNU_STUB_HIT("os_acquire_mutex");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_acquire_rwlock_read(void *)
{
    NV_XNU_STUB_HIT("os_acquire_rwlock_read");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_acquire_rwlock_write(void *)
{
    NV_XNU_STUB_HIT("os_acquire_rwlock_write");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_acquire_semaphore(void *)
{
    NV_XNU_STUB_HIT("os_acquire_semaphore");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU64 NV_API_CALL os_acquire_spinlock(void *)
{
    NV_XNU_STUB_HIT("os_acquire_spinlock");
    return (NvU64)0;
}

__attribute__((weak)) void NV_API_CALL os_add_record_for_crashLog(void *, NvU32)
{
    NV_XNU_STUB_HIT("os_add_record_for_crashLog");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_alloc_mem(void **, NvU64)
{
    NV_XNU_STUB_HIT("os_alloc_mem");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_alloc_mutex(void **)
{
    NV_XNU_STUB_HIT("os_alloc_mutex");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_alloc_pages_node(NvS32, NvU32, NvU32, NvU64 *)
{
    NV_XNU_STUB_HIT("os_alloc_pages_node");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void* NV_API_CALL os_alloc_rwlock(void)
{
    NV_XNU_STUB_HIT("os_alloc_rwlock");
    return NULL;
}

__attribute__((weak)) void* NV_API_CALL os_alloc_semaphore(NvU32)
{
    NV_XNU_STUB_HIT("os_alloc_semaphore");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_alloc_spinlock(void **)
{
    NV_XNU_STUB_HIT("os_alloc_spinlock");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_alloc_wait_queue(os_wait_queue **)
{
    NV_XNU_STUB_HIT("os_alloc_wait_queue");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_bug_check(NvU32, const char *)
{
    NV_XNU_STUB_HIT("os_bug_check");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_call_vgpu_vfio(void *, NvU32)
{
    NV_XNU_STUB_HIT("os_call_vgpu_vfio");
    return NV_ERR_NOT_SUPPORTED;
}

// NVLink fabric memory windows (multi-GPU servers); no Mac has a fabric.
NV_STATUS NV_API_CALL nv_dma_init_sysmem_window_for_fabric_access(nv_state_t *, NvU64, NvU64 *, NvU64 *, NvBool *)
{
    return NV_ERR_NOT_SUPPORTED;
}

void NV_API_CALL nv_dma_destroy_sysmem_window_for_fabric_access(nv_state_t *, NvU64, NvU64)
{
}

// Linux reports reclaimable page-cache memory here for RM's sysmem accounting; macOS has no equivalent RM can use.
NvU64 NV_API_CALL os_get_reclaimable_memory_usage(void)
{
    return 0;
}

__attribute__((weak)) void* NV_API_CALL os_cgroup_for_pid(int pid, void *pidInfo, int impl)
{
    NV_XNU_STUB_HIT("os_cgroup_for_pid");
    return NULL;
}

__attribute__((weak)) NvBool NV_API_CALL os_check_access(RsAccessRight accessRight)
{
    NV_XNU_STUB_HIT("os_check_access");
    return NV_FALSE;
}

__attribute__((weak)) void NV_API_CALL os_close_file(void *)
{
    NV_XNU_STUB_HIT("os_close_file");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_cond_acquire_mutex(void *)
{
    NV_XNU_STUB_HIT("os_cond_acquire_mutex");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_cond_acquire_rwlock_read(void *)
{
    NV_XNU_STUB_HIT("os_cond_acquire_rwlock_read");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_cond_acquire_rwlock_write(void *)
{
    NV_XNU_STUB_HIT("os_cond_acquire_rwlock_write");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_cond_acquire_semaphore(void *)
{
    NV_XNU_STUB_HIT("os_cond_acquire_semaphore");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU32 NV_API_CALL os_count_tail_pages(NvU64 address)
{
    NV_XNU_STUB_HIT("os_count_tail_pages");
    return (NvU32)0;
}

__attribute__((weak)) void NV_API_CALL os_dbg_breakpoint(void)
{
    NV_XNU_STUB_HIT("os_dbg_breakpoint");

}

__attribute__((weak)) void NV_API_CALL os_dbg_init(void)
{
    NV_XNU_STUB_HIT("os_dbg_init");

}

__attribute__((weak)) void NV_API_CALL os_dbg_set_level(NvU32)
{
    NV_XNU_STUB_HIT("os_dbg_set_level");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_delay(NvU32)
{
    NV_XNU_STUB_HIT("os_delay");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_delay_us(NvU32)
{
    NV_XNU_STUB_HIT("os_delay_us");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_delete_record_for_crashLog(void *)
{
    NV_XNU_STUB_HIT("os_delete_record_for_crashLog");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_device_vm_present(void)
{
    NV_XNU_STUB_HIT("os_device_vm_present");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_disable_console_access(void)
{
    NV_XNU_STUB_HIT("os_disable_console_access");

}

__attribute__((weak)) void NV_API_CALL os_dump_stack(void)
{
    NV_XNU_STUB_HIT("os_dump_stack");

}

__attribute__((weak)) void NV_API_CALL os_enable_console_access(void)
{
    NV_XNU_STUB_HIT("os_enable_console_access");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_enable_pci_req_atomics(void *, enum os_pci_req_atomics_type)
{
    NV_XNU_STUB_HIT("os_enable_pci_req_atomics");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_find_ns_pid(void *pid_info, NvU32 *ns_pid)
{
    NV_XNU_STUB_HIT("os_find_ns_pid");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_flush_cpu_cache_all(void)
{
    NV_XNU_STUB_HIT("os_flush_cpu_cache_all");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_flush_cpu_write_combine_buffer(void)
{
    NV_XNU_STUB_HIT("os_flush_cpu_write_combine_buffer");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_flush_user_cache(void)
{
    NV_XNU_STUB_HIT("os_flush_user_cache");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_flush_work_queue(struct os_work_queue *, NvBool)
{
    NV_XNU_STUB_HIT("os_flush_work_queue");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_free_mem(void *)
{
    NV_XNU_STUB_HIT("os_free_mem");

}

__attribute__((weak)) void NV_API_CALL os_free_mutex(void *)
{
    NV_XNU_STUB_HIT("os_free_mutex");

}

__attribute__((weak)) void NV_API_CALL os_free_rwlock(void *)
{
    NV_XNU_STUB_HIT("os_free_rwlock");

}

__attribute__((weak)) void NV_API_CALL os_free_semaphore(void *)
{
    NV_XNU_STUB_HIT("os_free_semaphore");

}

__attribute__((weak)) void NV_API_CALL os_free_spinlock(void *)
{
    NV_XNU_STUB_HIT("os_free_spinlock");

}

__attribute__((weak)) void NV_API_CALL os_free_wait_queue(os_wait_queue *)
{
    NV_XNU_STUB_HIT("os_free_wait_queue");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_acpi_rsdp_from_uefi(NvU32 *)
{
    NV_XNU_STUB_HIT("os_get_acpi_rsdp_from_uefi");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_cpu_count(void)
{
    NV_XNU_STUB_HIT("os_get_cpu_count");
    return (NvU32)0;
}

__attribute__((weak)) NvU64 NV_API_CALL os_get_cpu_frequency(void)
{
    NV_XNU_STUB_HIT("os_get_cpu_frequency");
    return (NvU64)0;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_cpu_number(void)
{
    NV_XNU_STUB_HIT("os_get_cpu_number");
    return (NvU32)0;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_current_process(void)
{
    NV_XNU_STUB_HIT("os_get_current_process");
    return (NvU32)0;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_current_process_flags(void)
{
    NV_XNU_STUB_HIT("os_get_current_process_flags");
    return (NvU32)0;
}

__attribute__((weak)) void NV_API_CALL os_get_current_process_name(char *, NvU32)
{
    NV_XNU_STUB_HIT("os_get_current_process_name");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_current_thread(NvU64 *)
{
    NV_XNU_STUB_HIT("os_get_current_thread");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_euid(NvU32 *)
{
    NV_XNU_STUB_HIT("os_get_euid");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_grid_csp_support(void)
{
    NV_XNU_STUB_HIT("os_get_grid_csp_support");
    return (NvU32)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_is_openrm(NvBool *)
{
    NV_XNU_STUB_HIT("os_get_is_openrm");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU64 NV_API_CALL os_get_max_user_va(void)
{
    NV_XNU_STUB_HIT("os_get_max_user_va");
    return (NvU64)0;
}

__attribute__((weak)) NvU64 NV_API_CALL os_get_monotonic_tick_resolution_ns(void)
{
    NV_XNU_STUB_HIT("os_get_monotonic_tick_resolution_ns");
    return (NvU64)0;
}

__attribute__((weak)) NvU64 NV_API_CALL os_get_monotonic_time_ns(void)
{
    NV_XNU_STUB_HIT("os_get_monotonic_time_ns");
    return (NvU64)0;
}

__attribute__((weak)) NvU64 NV_API_CALL os_get_monotonic_time_ns_hr(void)
{
    NV_XNU_STUB_HIT("os_get_monotonic_time_ns_hr");
    return (NvU64)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_numa_node_memory_usage(NvS32, NvU64 *, NvU64 *)
{
    NV_XNU_STUB_HIT("os_get_numa_node_memory_usage");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_page(NvU64 address)
{
    NV_XNU_STUB_HIT("os_get_page");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU32 NV_API_CALL os_get_page_refcount(NvU64 address)
{
    NV_XNU_STUB_HIT("os_get_page_refcount");
    return (NvU32)0;
}

__attribute__((weak)) void* NV_API_CALL os_get_pid_info(void)
{
    NV_XNU_STUB_HIT("os_get_pid_info");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_random_bytes(NvU8 *, NvU16)
{
    NV_XNU_STUB_HIT("os_get_random_bytes");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_smbios_header(NvU64 *pSmbsAddr)
{
    NV_XNU_STUB_HIT("os_get_smbios_header");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_system_time(NvU32 *, NvU32 *)
{
    NV_XNU_STUB_HIT("os_get_system_time");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_tegra_platform(NvU32 *)
{
    NV_XNU_STUB_HIT("os_get_tegra_platform");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_get_version_info(os_version_info*)
{
    NV_XNU_STUB_HIT("os_get_version_info");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvS32 NV_API_CALL os_imex_channel_count(void)
{
    NV_XNU_STUB_HIT("os_imex_channel_count");
    return (NvS32)0;
}

__attribute__((weak)) NvS32 NV_API_CALL os_imex_channel_get(NvU64)
{
    NV_XNU_STUB_HIT("os_imex_channel_get");
    return (NvS32)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_inject_vgx_msi(NvU16, NvU64, NvU32)
{
    NV_XNU_STUB_HIT("os_inject_vgx_msi");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvU8 NV_API_CALL os_io_read_byte(NvU32)
{
    NV_XNU_STUB_HIT("os_io_read_byte");
    return (NvU8)0;
}

__attribute__((weak)) NvU32 NV_API_CALL os_io_read_dword(NvU32)
{
    NV_XNU_STUB_HIT("os_io_read_dword");
    return (NvU32)0;
}

__attribute__((weak)) NvU16 NV_API_CALL os_io_read_word(NvU32)
{
    NV_XNU_STUB_HIT("os_io_read_word");
    return (NvU16)0;
}

__attribute__((weak)) void NV_API_CALL os_io_write_byte(NvU32, NvU8)
{
    NV_XNU_STUB_HIT("os_io_write_byte");

}

__attribute__((weak)) void NV_API_CALL os_io_write_dword(NvU32, NvU32)
{
    NV_XNU_STUB_HIT("os_io_write_dword");

}

__attribute__((weak)) void NV_API_CALL os_io_write_word(NvU32, NvU16)
{
    NV_XNU_STUB_HIT("os_io_write_word");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_iommu_sva_bind(void *arg, void **handle, NvU32 *pasid)
{
    NV_XNU_STUB_HIT("os_iommu_sva_bind");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_iommu_sva_unbind(void *handle)
{
    NV_XNU_STUB_HIT("os_iommu_sva_unbind");

}

__attribute__((weak)) NvBool NV_API_CALL os_is_administrator(void)
{
    NV_XNU_STUB_HIT("os_is_administrator");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_bif_reset_supported(void *)
{
    NV_XNU_STUB_HIT("os_is_bif_reset_supported");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_efi_enabled(void)
{
    NV_XNU_STUB_HIT("os_is_efi_enabled");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_grid_supported(void)
{
    NV_XNU_STUB_HIT("os_is_grid_supported");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_init_ns(void)
{
    NV_XNU_STUB_HIT("os_is_init_ns");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_isr(void)
{
    NV_XNU_STUB_HIT("os_is_isr");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_nvswitch_present(void)
{
    NV_XNU_STUB_HIT("os_is_nvswitch_present");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_queue_flush_ongoing(struct os_work_queue *)
{
    NV_XNU_STUB_HIT("os_is_queue_flush_ongoing");
    return NV_FALSE;
}

__attribute__((weak)) NvBool NV_API_CALL os_is_vgx_hyper(void)
{
    NV_XNU_STUB_HIT("os_is_vgx_hyper");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_lock_user_pages(void *, NvU64, void **, NvU32)
{
    NV_XNU_STUB_HIT("os_lock_user_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_lookup_user_io_memory(void *, NvU64, NvU64 **)
{
    NV_XNU_STUB_HIT("os_lookup_user_io_memory");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void* NV_API_CALL os_map_kernel_space(NvU64, NvU64, NvU32)
{
    NV_XNU_STUB_HIT("os_map_kernel_space");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_match_mmap_offset(void *, NvU64, NvU64 *)
{
    NV_XNU_STUB_HIT("os_match_mmap_offset");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvS32 NV_API_CALL os_mem_cmp(const NvU8 *, const NvU8 *, NvU32)
{
    NV_XNU_STUB_HIT("os_mem_cmp");
    return (NvS32)0;
}

__attribute__((weak)) void* NV_API_CALL os_mem_copy(void *, const void *, NvU32)
{
    NV_XNU_STUB_HIT("os_mem_copy");
    return NULL;
}

__attribute__((weak)) void* NV_API_CALL os_mem_set(void *, NvU8, NvU32)
{
    NV_XNU_STUB_HIT("os_mem_set");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_memcpy_from_user(void *, const void *, NvU32)
{
    NV_XNU_STUB_HIT("os_memcpy_from_user");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_memcpy_to_user(void *, const void *, NvU32)
{
    NV_XNU_STUB_HIT("os_memcpy_to_user");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_numa_add_gpu_memory(void *, NvU64, NvU64, NvU32 *)
{
    NV_XNU_STUB_HIT("os_numa_add_gpu_memory");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_numa_memblock_size(NvU64 *)
{
    NV_XNU_STUB_HIT("os_numa_memblock_size");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_numa_remove_gpu_memory(void *, NvU64, NvU64, NvU32)
{
    NV_XNU_STUB_HIT("os_numa_remove_gpu_memory");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_nv_cap_close_fd(int)
{
    NV_XNU_STUB_HIT("os_nv_cap_close_fd");

}

__attribute__((weak)) nv_cap_t* NV_API_CALL os_nv_cap_create_dir_entry(nv_cap_t *, const char *, int)
{
    NV_XNU_STUB_HIT("os_nv_cap_create_dir_entry");
    return NULL;
}

__attribute__((weak)) nv_cap_t* NV_API_CALL os_nv_cap_create_file_entry(nv_cap_t *, const char *, int)
{
    NV_XNU_STUB_HIT("os_nv_cap_create_file_entry");
    return NULL;
}

__attribute__((weak)) void NV_API_CALL os_nv_cap_destroy_entry(nv_cap_t *)
{
    NV_XNU_STUB_HIT("os_nv_cap_destroy_entry");

}

__attribute__((weak)) int NV_API_CALL os_nv_cap_validate_and_dup_fd(const nv_cap_t *, int)
{
    NV_XNU_STUB_HIT("os_nv_cap_validate_and_dup_fd");
    return (int)0;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_offline_page_at_address(NvU64 address)
{
    NV_XNU_STUB_HIT("os_offline_page_at_address");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void* NV_API_CALL os_pci_init_handle(NvU32, NvU8, NvU8, NvU8, NvU16 *, NvU16 *)
{
    NV_XNU_STUB_HIT("os_pci_init_handle");
    return NULL;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_read_byte(void *, NvU32, NvU8 *)
{
    NV_XNU_STUB_HIT("os_pci_read_byte");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_read_dword(void *, NvU32, NvU32 *)
{
    NV_XNU_STUB_HIT("os_pci_read_dword");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_read_word(void *, NvU32, NvU16 *)
{
    NV_XNU_STUB_HIT("os_pci_read_word");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_pci_remove(void *)
{
    NV_XNU_STUB_HIT("os_pci_remove");

}

__attribute__((weak)) NvBool NV_API_CALL os_pci_remove_supported(void)
{
    NV_XNU_STUB_HIT("os_pci_remove_supported");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_write_byte(void *, NvU32, NvU8)
{
    NV_XNU_STUB_HIT("os_pci_write_byte");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_write_dword(void *, NvU32, NvU32)
{
    NV_XNU_STUB_HIT("os_pci_write_dword");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_pci_write_word(void *, NvU32, NvU16)
{
    NV_XNU_STUB_HIT("os_pci_write_word");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_put_page(NvU64 address)
{
    NV_XNU_STUB_HIT("os_put_page");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_put_pid_info(void *pid_info)
{
    NV_XNU_STUB_HIT("os_put_pid_info");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_queue_work_item(struct os_work_queue *, void *)
{
    NV_XNU_STUB_HIT("os_queue_work_item");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_read_file(void *, NvU8 *, NvU64, NvU64)
{
    NV_XNU_STUB_HIT("os_read_file");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_registry_init(void)
{
    NV_XNU_STUB_HIT("os_registry_init");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_release_mutex(void *)
{
    NV_XNU_STUB_HIT("os_release_mutex");

}

__attribute__((weak)) void NV_API_CALL os_release_rwlock_read(void *)
{
    NV_XNU_STUB_HIT("os_release_rwlock_read");

}

__attribute__((weak)) void NV_API_CALL os_release_rwlock_write(void *)
{
    NV_XNU_STUB_HIT("os_release_rwlock_write");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_release_semaphore(void *)
{
    NV_XNU_STUB_HIT("os_release_semaphore");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_release_spinlock(void *, NvU64)
{
    NV_XNU_STUB_HIT("os_release_spinlock");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_schedule(void)
{
    NV_XNU_STUB_HIT("os_schedule");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NvBool NV_API_CALL os_semaphore_may_sleep(void)
{
    NV_XNU_STUB_HIT("os_semaphore_may_sleep");
    return NV_FALSE;
}

__attribute__((weak)) NvS32 NV_API_CALL os_snprintf(char *, NvU32, const char *, ...)
{
    NV_XNU_STUB_HIT("os_snprintf");
    return (NvS32)0;
}

__attribute__((weak)) NvS32 NV_API_CALL os_string_compare(const char *, const char *)
{
    NV_XNU_STUB_HIT("os_string_compare");
    return (NvS32)0;
}

__attribute__((weak)) char* NV_API_CALL os_string_copy(char *, const char *)
{
    NV_XNU_STUB_HIT("os_string_copy");
    return NULL;
}

__attribute__((weak)) NvU32 NV_API_CALL os_string_length(const char *)
{
    NV_XNU_STUB_HIT("os_string_length");
    return (NvU32)0;
}

__attribute__((weak)) NvU32 NV_API_CALL os_strtoul(const char *, char **, NvU32)
{
    NV_XNU_STUB_HIT("os_strtoul");
    return (NvU32)0;
}

__attribute__((weak)) NvBool NV_API_CALL os_supports_kernel_suspend_notifiers(void)
{
    NV_XNU_STUB_HIT("os_supports_kernel_suspend_notifiers");
    return NV_FALSE;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_tegra_igpu_perf_boost(void *, NvBool, NvU32, int)
{
    NV_XNU_STUB_HIT("os_tegra_igpu_perf_boost");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) NV_STATUS NV_API_CALL os_unlock_user_pages(NvU64, void *, NvU32)
{
    NV_XNU_STUB_HIT("os_unlock_user_pages");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL os_unmap_kernel_space(void *, NvU64)
{
    NV_XNU_STUB_HIT("os_unmap_kernel_space");

}

__attribute__((weak)) void NV_API_CALL os_wait_interruptible(os_wait_queue *)
{
    NV_XNU_STUB_HIT("os_wait_interruptible");

}

__attribute__((weak)) void NV_API_CALL os_wait_uninterruptible(os_wait_queue *)
{
    NV_XNU_STUB_HIT("os_wait_uninterruptible");

}

__attribute__((weak)) void NV_API_CALL os_wake_up(os_wait_queue *)
{
    NV_XNU_STUB_HIT("os_wake_up");

}

__attribute__((weak)) NV_STATUS NV_API_CALL os_write_file(void *, NvU8 *, NvU64, NvU64)
{
    NV_XNU_STUB_HIT("os_write_file");
    return NV_ERR_NOT_SUPPORTED;
}

__attribute__((weak)) void NV_API_CALL out_string(const char *str)
{
    NV_XNU_STUB_HIT("out_string");

}

__attribute__((weak)) nv_cap_t * nvidia_caps_root = 0;
__attribute__((weak)) NvBool os_cc_enabled = 0;
__attribute__((weak)) NvBool os_cc_sev_snp_enabled = 0;
__attribute__((weak)) NvBool os_cc_sme_enabled = 0;
__attribute__((weak)) NvBool os_cc_snp_vtom_enabled = 0;
__attribute__((weak)) NvBool os_cc_tdx_enabled = 0;
__attribute__((weak)) NvBool os_dma_buf_enabled = 0;
__attribute__((weak)) NvBool os_imex_channel_is_supported = 0;
__attribute__((weak)) NvU64 os_max_page_size = 0;
__attribute__((weak)) NvU64 os_page_mask = 0;
__attribute__((weak)) NvU8 os_page_shift = 0;
__attribute__((weak)) NvU64 os_page_size = 0;

__attribute__((weak)) long libspdm_aead_aes_gcm_decrypt(void) { NV_XNU_STUB_HIT("libspdm_aead_aes_gcm_decrypt"); return 0; }
__attribute__((weak)) long libspdm_aead_aes_gcm_decrypt_prealloc(void) { NV_XNU_STUB_HIT("libspdm_aead_aes_gcm_decrypt_prealloc"); return 0; }
__attribute__((weak)) long libspdm_aead_aes_gcm_encrypt(void) { NV_XNU_STUB_HIT("libspdm_aead_aes_gcm_encrypt"); return 0; }
__attribute__((weak)) long libspdm_aead_aes_gcm_encrypt_prealloc(void) { NV_XNU_STUB_HIT("libspdm_aead_aes_gcm_encrypt_prealloc"); return 0; }
__attribute__((weak)) long libspdm_aead_free(void) { NV_XNU_STUB_HIT("libspdm_aead_free"); return 0; }
__attribute__((weak)) long libspdm_aead_gcm_prealloc(void) { NV_XNU_STUB_HIT("libspdm_aead_gcm_prealloc"); return 0; }
__attribute__((weak)) long libspdm_asn1_get_tag(void) { NV_XNU_STUB_HIT("libspdm_asn1_get_tag"); return 0; }
__attribute__((weak)) long libspdm_check_crypto_backend(void) { NV_XNU_STUB_HIT("libspdm_check_crypto_backend"); return 0; }
__attribute__((weak)) long libspdm_decode_base64(void) { NV_XNU_STUB_HIT("libspdm_decode_base64"); return 0; }
__attribute__((weak)) long libspdm_ec_compute_key(void) { NV_XNU_STUB_HIT("libspdm_ec_compute_key"); return 0; }
__attribute__((weak)) long libspdm_ec_free(void) { NV_XNU_STUB_HIT("libspdm_ec_free"); return 0; }
__attribute__((weak)) long libspdm_ec_generate_key(void) { NV_XNU_STUB_HIT("libspdm_ec_generate_key"); return 0; }
__attribute__((weak)) long libspdm_ec_get_public_key_from_x509(void) { NV_XNU_STUB_HIT("libspdm_ec_get_public_key_from_x509"); return 0; }
__attribute__((weak)) long libspdm_ec_new_by_nid(void) { NV_XNU_STUB_HIT("libspdm_ec_new_by_nid"); return 0; }
__attribute__((weak)) long libspdm_ecdsa_sign(void) { NV_XNU_STUB_HIT("libspdm_ecdsa_sign"); return 0; }
__attribute__((weak)) long libspdm_ecdsa_verify(void) { NV_XNU_STUB_HIT("libspdm_ecdsa_verify"); return 0; }
__attribute__((weak)) long libspdm_encode_base64(void) { NV_XNU_STUB_HIT("libspdm_encode_base64"); return 0; }
__attribute__((weak)) long libspdm_hkdf_sha256_expand(void) { NV_XNU_STUB_HIT("libspdm_hkdf_sha256_expand"); return 0; }
__attribute__((weak)) long libspdm_hkdf_sha256_extract(void) { NV_XNU_STUB_HIT("libspdm_hkdf_sha256_extract"); return 0; }
__attribute__((weak)) long libspdm_hkdf_sha384_expand(void) { NV_XNU_STUB_HIT("libspdm_hkdf_sha384_expand"); return 0; }
__attribute__((weak)) long libspdm_hkdf_sha384_extract(void) { NV_XNU_STUB_HIT("libspdm_hkdf_sha384_extract"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_all(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_all"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_duplicate(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_duplicate"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_final(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_final"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_free(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_free"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_new(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_new"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_set_key(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_set_key"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha256_update(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha256_update"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_all(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_all"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_duplicate(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_duplicate"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_final(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_final"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_free(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_free"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_new(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_new"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_set_key(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_set_key"); return 0; }
__attribute__((weak)) long libspdm_hmac_sha384_update(void) { NV_XNU_STUB_HIT("libspdm_hmac_sha384_update"); return 0; }
__attribute__((weak)) long libspdm_random_bytes(void) { NV_XNU_STUB_HIT("libspdm_random_bytes"); return 0; }
__attribute__((weak)) long libspdm_rsa_free(void) { NV_XNU_STUB_HIT("libspdm_rsa_free"); return 0; }
__attribute__((weak)) long libspdm_rsa_get_public_key_from_x509(void) { NV_XNU_STUB_HIT("libspdm_rsa_get_public_key_from_x509"); return 0; }
__attribute__((weak)) long libspdm_rsa_new(void) { NV_XNU_STUB_HIT("libspdm_rsa_new"); return 0; }
__attribute__((weak)) long libspdm_rsa_pss_sign(void) { NV_XNU_STUB_HIT("libspdm_rsa_pss_sign"); return 0; }
__attribute__((weak)) long libspdm_rsa_pss_verify(void) { NV_XNU_STUB_HIT("libspdm_rsa_pss_verify"); return 0; }
__attribute__((weak)) long libspdm_rsa_set_key(void) { NV_XNU_STUB_HIT("libspdm_rsa_set_key"); return 0; }
__attribute__((weak)) long libspdm_sha256_duplicate(void) { NV_XNU_STUB_HIT("libspdm_sha256_duplicate"); return 0; }
__attribute__((weak)) long libspdm_sha256_final(void) { NV_XNU_STUB_HIT("libspdm_sha256_final"); return 0; }
__attribute__((weak)) long libspdm_sha256_free(void) { NV_XNU_STUB_HIT("libspdm_sha256_free"); return 0; }
__attribute__((weak)) long libspdm_sha256_hash_all(void) { NV_XNU_STUB_HIT("libspdm_sha256_hash_all"); return 0; }
__attribute__((weak)) long libspdm_sha256_init(void) { NV_XNU_STUB_HIT("libspdm_sha256_init"); return 0; }
__attribute__((weak)) long libspdm_sha256_new(void) { NV_XNU_STUB_HIT("libspdm_sha256_new"); return 0; }
__attribute__((weak)) long libspdm_sha256_update(void) { NV_XNU_STUB_HIT("libspdm_sha256_update"); return 0; }
__attribute__((weak)) long libspdm_sha384_duplicate(void) { NV_XNU_STUB_HIT("libspdm_sha384_duplicate"); return 0; }
__attribute__((weak)) long libspdm_sha384_final(void) { NV_XNU_STUB_HIT("libspdm_sha384_final"); return 0; }
__attribute__((weak)) long libspdm_sha384_free(void) { NV_XNU_STUB_HIT("libspdm_sha384_free"); return 0; }
__attribute__((weak)) long libspdm_sha384_hash_all(void) { NV_XNU_STUB_HIT("libspdm_sha384_hash_all"); return 0; }
__attribute__((weak)) long libspdm_sha384_init(void) { NV_XNU_STUB_HIT("libspdm_sha384_init"); return 0; }
__attribute__((weak)) long libspdm_sha384_new(void) { NV_XNU_STUB_HIT("libspdm_sha384_new"); return 0; }
__attribute__((weak)) long libspdm_sha384_update(void) { NV_XNU_STUB_HIT("libspdm_sha384_update"); return 0; }
__attribute__((weak)) long libspdm_x509_compare_date_time(void) { NV_XNU_STUB_HIT("libspdm_x509_compare_date_time"); return 0; }
__attribute__((weak)) long libspdm_x509_get_cert_from_cert_chain(void) { NV_XNU_STUB_HIT("libspdm_x509_get_cert_from_cert_chain"); return 0; }
__attribute__((weak)) long libspdm_x509_get_extended_basic_constraints(void) { NV_XNU_STUB_HIT("libspdm_x509_get_extended_basic_constraints"); return 0; }
__attribute__((weak)) long libspdm_x509_get_extended_key_usage(void) { NV_XNU_STUB_HIT("libspdm_x509_get_extended_key_usage"); return 0; }
__attribute__((weak)) long libspdm_x509_get_extension_data(void) { NV_XNU_STUB_HIT("libspdm_x509_get_extension_data"); return 0; }
__attribute__((weak)) long libspdm_x509_get_issuer_name(void) { NV_XNU_STUB_HIT("libspdm_x509_get_issuer_name"); return 0; }
__attribute__((weak)) long libspdm_x509_get_key_usage(void) { NV_XNU_STUB_HIT("libspdm_x509_get_key_usage"); return 0; }
__attribute__((weak)) long libspdm_x509_get_serial_number(void) { NV_XNU_STUB_HIT("libspdm_x509_get_serial_number"); return 0; }
__attribute__((weak)) long libspdm_x509_get_signature_algorithm(void) { NV_XNU_STUB_HIT("libspdm_x509_get_signature_algorithm"); return 0; }
__attribute__((weak)) long libspdm_x509_get_subject_name(void) { NV_XNU_STUB_HIT("libspdm_x509_get_subject_name"); return 0; }
__attribute__((weak)) long libspdm_x509_get_validity(void) { NV_XNU_STUB_HIT("libspdm_x509_get_validity"); return 0; }
__attribute__((weak)) long libspdm_x509_get_version(void) { NV_XNU_STUB_HIT("libspdm_x509_get_version"); return 0; }
__attribute__((weak)) long libspdm_x509_set_date_time(void) { NV_XNU_STUB_HIT("libspdm_x509_set_date_time"); return 0; }
__attribute__((weak)) long libspdm_x509_verify_cert(void) { NV_XNU_STUB_HIT("libspdm_x509_verify_cert"); return 0; }
__attribute__((weak)) long libspdm_x509_verify_cert_chain(void) { NV_XNU_STUB_HIT("libspdm_x509_verify_cert_chain"); return 0; }
// NVIDIA's module-parameter table (nv-reg.h), not a function: rm_parse_option_string() walks it comparing each entry's
// name until a NULL entry, and an option it does not find becomes an RM registry dword. It was a stub function of the
// same name, so that walk read the stub's machine code as string pointers and any nvregdwords= boot-arg stopped the
// boot in strcmp. Empty here, so every nvregdwords option reaches RM's registry.
struct nv_xnu_parm { const char *name; NvU32 *data; };
__attribute__((used)) struct nv_xnu_parm nv_parms[] = { { NULL, NULL } };
__attribute__((weak)) long nvswitch_os_acquire_fabric_mgmt_cap(void) { NV_XNU_STUB_HIT("nvswitch_os_acquire_fabric_mgmt_cap"); return 0; }
__attribute__((weak)) long nvswitch_os_add_client_event(void) { NV_XNU_STUB_HIT("nvswitch_os_add_client_event"); return 0; }
__attribute__((weak)) long nvswitch_os_alloc_contig_memory(void) { NV_XNU_STUB_HIT("nvswitch_os_alloc_contig_memory"); return 0; }
__attribute__((weak)) long nvswitch_os_assert_log(void) { NV_XNU_STUB_HIT("nvswitch_os_assert_log"); return 0; }
__attribute__((weak)) long nvswitch_os_free(void) { NV_XNU_STUB_HIT("nvswitch_os_free"); return 0; }
__attribute__((weak)) long nvswitch_os_free_contig_memory(void) { NV_XNU_STUB_HIT("nvswitch_os_free_contig_memory"); return 0; }
__attribute__((weak)) long nvswitch_os_get_os_version(void) { NV_XNU_STUB_HIT("nvswitch_os_get_os_version"); return 0; }
__attribute__((weak)) long nvswitch_os_get_pid(void) { NV_XNU_STUB_HIT("nvswitch_os_get_pid"); return 0; }
__attribute__((weak)) long nvswitch_os_get_platform_time(void) { NV_XNU_STUB_HIT("nvswitch_os_get_platform_time"); return 0; }
__attribute__((weak)) long nvswitch_os_get_platform_time_epoch(void) { NV_XNU_STUB_HIT("nvswitch_os_get_platform_time_epoch"); return 0; }
__attribute__((weak)) long nvswitch_os_get_supported_register_events_params(void) { NV_XNU_STUB_HIT("nvswitch_os_get_supported_register_events_params"); return 0; }
__attribute__((weak)) long nvswitch_os_is_admin(void) { NV_XNU_STUB_HIT("nvswitch_os_is_admin"); return 0; }
__attribute__((weak)) long nvswitch_os_is_fabric_manager(void) { NV_XNU_STUB_HIT("nvswitch_os_is_fabric_manager"); return 0; }
__attribute__((weak)) long nvswitch_os_is_uuid_in_blacklist(void) { NV_XNU_STUB_HIT("nvswitch_os_is_uuid_in_blacklist"); return 0; }
__attribute__((weak)) long nvswitch_os_malloc_trace(void) { NV_XNU_STUB_HIT("nvswitch_os_malloc_trace"); return 0; }
__attribute__((weak)) long nvswitch_os_map_dma_region(void) { NV_XNU_STUB_HIT("nvswitch_os_map_dma_region"); return 0; }
__attribute__((weak)) long nvswitch_os_mem_read32(void) { NV_XNU_STUB_HIT("nvswitch_os_mem_read32"); return 0; }
__attribute__((weak)) long nvswitch_os_mem_write32(void) { NV_XNU_STUB_HIT("nvswitch_os_mem_write32"); return 0; }
__attribute__((weak)) long nvswitch_os_memcpy(void) { NV_XNU_STUB_HIT("nvswitch_os_memcpy"); return 0; }
__attribute__((weak)) long nvswitch_os_memset(void) { NV_XNU_STUB_HIT("nvswitch_os_memset"); return 0; }
__attribute__((weak)) long nvswitch_os_notify_client_event(void) { NV_XNU_STUB_HIT("nvswitch_os_notify_client_event"); return 0; }
__attribute__((weak)) long nvswitch_os_override_platform(void) { NV_XNU_STUB_HIT("nvswitch_os_override_platform"); return 0; }
__attribute__((weak)) long nvswitch_os_print(void) { NV_XNU_STUB_HIT("nvswitch_os_print"); return 0; }
__attribute__((weak)) long nvswitch_os_read_registry_dword(void) { NV_XNU_STUB_HIT("nvswitch_os_read_registry_dword"); return 0; }
__attribute__((weak)) long nvswitch_os_remove_client_event(void) { NV_XNU_STUB_HIT("nvswitch_os_remove_client_event"); return 0; }
__attribute__((weak)) long nvswitch_os_report_error(void) { NV_XNU_STUB_HIT("nvswitch_os_report_error"); return 0; }
__attribute__((weak)) long nvswitch_os_set_dma_mask(void) { NV_XNU_STUB_HIT("nvswitch_os_set_dma_mask"); return 0; }
__attribute__((weak)) long nvswitch_os_sleep(void) { NV_XNU_STUB_HIT("nvswitch_os_sleep"); return 0; }
__attribute__((weak)) long nvswitch_os_snprintf(void) { NV_XNU_STUB_HIT("nvswitch_os_snprintf"); return 0; }
__attribute__((weak)) long nvswitch_os_strlen(void) { NV_XNU_STUB_HIT("nvswitch_os_strlen"); return 0; }
__attribute__((weak)) long nvswitch_os_strncmp(void) { NV_XNU_STUB_HIT("nvswitch_os_strncmp"); return 0; }
__attribute__((weak)) long nvswitch_os_sync_dma_region_for_cpu(void) { NV_XNU_STUB_HIT("nvswitch_os_sync_dma_region_for_cpu"); return 0; }
__attribute__((weak)) long nvswitch_os_sync_dma_region_for_device(void) { NV_XNU_STUB_HIT("nvswitch_os_sync_dma_region_for_device"); return 0; }
__attribute__((weak)) long nvswitch_os_unmap_dma_region(void) { NV_XNU_STUB_HIT("nvswitch_os_unmap_dma_region"); return 0; }
__attribute__((weak)) long nvswitch_os_vsnprintf(void) { NV_XNU_STUB_HIT("nvswitch_os_vsnprintf"); return 0; }

}
