#define VK_NO_PROTOTYPES
#define VK_USE_PLATFORM_METAL_EXT
#include <vulkan/vulkan.h>
#import <Foundation/Foundation.h>
#import <QuartzCore/CAMetalLayer.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <IOKit/IOKitLib.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <sys/sysctl.h>
#include <dlfcn.h>
#include <cerrno>
#include <climits>
#include "../../plugin/nvmtl_fd_token.h"
#include "../../plugin/nvmtl_rebind_import.h"
#include <fcntl.h>
#include <unistd.h>
#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <type_traits>
#include <unordered_map>
#include <vector>
#include "present_worker.h"
#ifdef NVKBRIDGE_WINE
#include "present_hud.h"
#endif

extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance, const char *);
extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice, const char *);
extern "C" VKAPI_ATTR VkResult VKAPI_CALL vkCreateMetalSurfaceEXT(
   VkInstance, const VkMetalSurfaceCreateInfoEXT *, const VkAllocationCallbacks *, VkSurfaceKHR *);

namespace {
struct Share { uint32_t version, surface, plane, pad; uint64_t bytes; };
static_assert(sizeof(Share) == 24);
using StageImport = void (*)(const Share *);
using SurfaceDirty = uint32_t (*)(VkDevice, uint32_t, uint32_t);
PFN_vkGetInstanceProcAddr backend_gipa;
PFN_vkGetDeviceProcAddr backend_gdpa;
StageImport stage_import;
SurfaceDirty surface_dirty;
void *backend;
std::once_flag backend_once;
std::once_flag device_procs_once;
std::atomic<uint64_t> next_handle{0x4e564b0000000000ull};
std::atomic<uint64_t> copies{}, copy_bytes{}, drawable_ns{}, producer_ns{}, import_failures{};
// Optional local profiling. Timings include the rendering dependencies waited
// by the copy fence; they are not GPU timestamps for the copy alone.
std::array<std::atomic<uint64_t>,16> present_stats{};
struct PresentTimer {
   bool active;
   unsigned counter;
   std::chrono::steady_clock::time_point start;
   PresentTimer(bool enabled,unsigned index):active(enabled),counter(index) {
      if(active)start=std::chrono::steady_clock::now();
   }
   void finish() {
      if(!active)return;
      present_stats[counter].fetch_add(std::chrono::duration_cast<std::chrono::nanoseconds>(
         std::chrono::steady_clock::now()-start).count(),std::memory_order_relaxed);
      active=false;
   }
   ~PresentTimer() { finish(); }
};
#ifdef NVKBRIDGE_WINE
std::atomic<uint64_t> hud_upload_bytes{};
#endif
using PresentObserver=void (*)(PFN_vkGetDeviceProcAddr,VkDevice,VkQueue,VkBuffer,uint32_t,uint32_t,uint32_t,uint32_t);
std::atomic<PresentObserver> present_observer{};
std::mutex registry;

struct Instance { VkInstance raw{}; };
struct Device;
struct Surface {
   CAMetalLayer *__strong layer;
#ifdef NVKBRIDGE_WINE
   std::shared_ptr<nvkbridge_hud::State> hud;
#endif
};
struct Queue { VkQueue raw{}; uint32_t family{}; std::weak_ptr<Device> dev; };
struct Image { VkImage raw{}; VkDeviceMemory memory{}; bool acquired{},present_pending{}; };
struct Frame;
struct Chain {
   std::shared_ptr<Device> dev;
   std::shared_ptr<Surface> surface;
   VkSwapchainKHR handle{};
   VkFormat format{};
   VkExtent2D extent{};
   VkPresentModeKHR present_mode{};
   std::vector<Image> images;
   // Idle entries retain the exact IOSurface, not its drawable. This prevents
   // surface-ID reuse without holding CoreAnimation's drawable slots hostage.
   std::array<std::shared_ptr<Frame>,3> present_cache;
   uint64_t present_cache_bytes{};
   unsigned present_cache_cursor{};
#ifdef NVKBRIDGE_WINE
   VkBuffer hud_buffer{};
   VkDeviceMemory hud_memory{};
   void *hud_mapping{};
   uint64_t hud_version{};
   bool hud_disabled{};
#endif
   std::mutex mutex;
   std::condition_variable available;
   uint32_t cursor{};
   bool retired{};
   VkResult present_error{VK_SUCCESS};
   VkAllocationCallbacks allocator{};
   bool custom_allocator{};
   const VkAllocationCallbacks *alloc() const { return custom_allocator ? &allocator : nullptr; }
};
struct Device {
   VkDevice raw{};
   VkPhysicalDevice physical{};
   std::shared_ptr<Instance> instance;
   VkPhysicalDeviceProperties properties{};
   VkPhysicalDeviceMemoryProperties memory{};
   id<MTLDevice> __strong metal;
   uint32_t copy_family{}, copy_index{};
   VkQueue copy_queue{};
   bool swapchain{};
   bool private_copy_queue{};
   bool reuse_present_resources{}, profile_present{};
   VkDeviceSize host_present_alignment{};
   std::atomic<bool> uncertain{};
   std::atomic<bool> closing{};
   std::recursive_mutex submit;
   // Lifecycle and present acceptance are serialized separately from backend
   // queue operations. The worker never takes this gate.
   std::mutex present_call;
   std::unique_ptr<nvkbridge::PresentWorker> presenter;
};
std::unordered_map<VkInstance, std::shared_ptr<Instance>> instances;
std::unordered_map<VkPhysicalDevice, std::shared_ptr<Instance>> physicals;
std::unordered_map<VkDevice, std::shared_ptr<Device>> devices;
std::unordered_map<VkQueue, std::shared_ptr<Queue>> queues;
std::unordered_map<VkSurfaceKHR, std::shared_ptr<Surface>> surfaces;
std::unordered_map<VkSwapchainKHR, std::shared_ptr<Chain>> chains;

template<class T, class K> std::shared_ptr<T> find(const std::unordered_map<K, std::shared_ptr<T>> &map, K key) {
   std::lock_guard<std::mutex> lock(registry);
   auto it = map.find(key);
   return it == map.end() ? nullptr : it->second;
}
template<class T> T global(const char *name) { return reinterpret_cast<T>(backend_gipa(VK_NULL_HANDLE, name)); }
template<class T> T iproc(VkInstance i, const char *name) { return reinterpret_cast<T>(backend_gipa(i, name)); }
template<class T> T dproc(VkDevice d, const char *name) { return reinterpret_cast<T>(backend_gdpa(d, name)); }
#define DP(dev, name) dproc<PFN_vk##name>((dev)->raw, "vk" #name)
#define IP(inst, name) iproc<PFN_vk##name>((inst)->raw, "vk" #name)

void fail_presenter(Device *d) {
   d->uncertain=true;
   std::lock_guard<std::mutex> guard(registry);
   for(auto &entry:chains)if(entry.second->dev.get()==d) {
      std::lock_guard<std::mutex> lock(entry.second->mutex);
      entry.second->present_error=VK_ERROR_DEVICE_LOST;entry.second->available.notify_all();
   }
   fprintf(stderr,"NVK Bridge: asynchronous presentation failed; device/resources retained\n");
}
void drain_presenter(const std::shared_ptr<Device> &d) {
   if(d->presenter)d->presenter->drain();
}
void shutdown_presenters() {
   // Registered after loading the renderer, so workers stop before either its
   // or this frontend's process-exit destructors. Never wait with registry held.
   for(;;) {
      std::shared_ptr<Device> d;
      { std::lock_guard<std::mutex> lock(registry);
        for(auto &entry:devices)if(entry.second->presenter && !entry.second->closing.exchange(true)) {d=entry.second;break;}
      }
      if(!d)break;
      std::lock_guard<std::mutex> gate(d->present_call);d->presenter->stop();
   }
}
bool register_presenter_shutdown() {
   static std::once_flag once;
   static bool registered=false;
   std::call_once(once,[]{registered=std::atexit(shutdown_presenters)==0;});
   return registered;
}
bool async_present_requested() {
#ifdef NVKBRIDGE_WINE
   const char *value=getenv("WINE_NVK_ASYNC_PRESENT");
   return !value || strcmp(value,"0")!=0;
#else
   return false;
#endif
}

// Browser GPU-process sandboxes (Firefox/Zen) deny reading /dev/null and the
// hw.pagesize sysctl. The NVK RM bridge uses a read-only /dev/null descriptor
// only as an owned handle number, and Mesa sizes system memory through
// sysconf(_SC_PHYS_PAGES), which Darwin's libc derives from hw.pagesize.
// Inside those sandboxes NVK enumerated no device, Zink could not choose a
// physical device and CGL fell back, forcing Firefox to software WebRender.
// A read-only pipe end at EOF stands in for the denied null descriptor, as in
// the Metal plugin's ICD path; failed page queries fall back to vm_page_size
// and hw.memsize. Other paths, modes, errors and sysconf names are unchanged.
int sandbox_open(const char *path, int flags, ...) {
   mode_t mode = 0;
   if (flags & O_CREAT) { va_list args; va_start(args, flags); mode = (mode_t)va_arg(args, int); va_end(args); }
   return nvmtl_icd_open(path, flags, mode);
}

long sandbox_sysconf(int name) {
   const int saved = errno;
   const long value = sysconf(name);
   if (value != -1 || (name != _SC_PAGESIZE && name != _SC_PHYS_PAGES)) return value;
   const int failure = errno;
   if (!vm_page_size) { errno = failure; return value; }
   if (name == _SC_PAGESIZE) { errno = saved; return static_cast<long>(vm_page_size); }
   // Darwin's libc derives _SC_PHYS_PAGES from the hw.pagesize sysctl. Use the
   // byte total from hw.memsize, which those sandboxes allow, or the host info.
   uint64_t bytes = 0;
   size_t length = sizeof(bytes);
   if (sysctlbyname("hw.memsize", &bytes, &length, nullptr, 0) || length != sizeof(bytes) || !bytes) {
      host_basic_info_data_t info{};
      mach_msg_type_number_t count = HOST_BASIC_INFO_COUNT;
      mach_port_t host = mach_host_self();
      const kern_return_t result = host_info(host, HOST_BASIC_INFO, reinterpret_cast<host_info_t>(&info), &count);
      mach_port_deallocate(mach_task_self(), host);
      if (result != KERN_SUCCESS || !info.max_mem) { errno = failure; return value; }
      bytes = info.max_mem;
   }
   const uint64_t pages = bytes / vm_page_size;
   if (!pages || pages > static_cast<uint64_t>(LONG_MAX)) { errno = failure; return value; }
   errno = saved;
   return static_cast<long>(pages);
}

// Keep each target image loaded for the lifetime of the callbacks it imports.
// Use exported anchors instead of walking dyld's process-wide image list.
void redirect_renderer_sandbox_imports(void *renderer, const std::string &path) {
   const void *anchor = dlsym(renderer, "vk_icdGetInstanceProcAddr");
   if (anchor) {
      nvmtl_rebind_import(anchor, "_open", reinterpret_cast<void *>(&sandbox_open));
      nvmtl_rebind_import(anchor, "_sysconf", reinterpret_cast<void *>(&sandbox_sysconf));
   }
   const std::string sibling = path.substr(0, path.find_last_of('/') + 1) + "libnvk_zink.dylib";
   static void *zink; // Intentional process-lifetime owner, like backend.
   zink = dlopen(sibling.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_NOLOAD);
   if (zink) {
      anchor = dlsym(zink, "nvk_zink_get_proc_address");
      if (anchor) {
         nvmtl_rebind_import(anchor, "_open", reinterpret_cast<void *>(&sandbox_open));
         nvmtl_rebind_import(anchor, "_sysconf", reinterpret_cast<void *>(&sandbox_sysconf));
      }
   }
}

bool initialize() {
   std::call_once(backend_once, [] {
#ifdef NVKBRIDGE_WINE
      // Wine preserves WINE_* variables in the Unix environment of PE child
      // processes. Forward the explicit opt-in before Mesa reads its options.
      if(const char *value=getenv("WINE_NVK_EXPERIMENTAL"))setenv("NVK_EXPERIMENTAL",value,1);
#endif
      Dl_info self{};
      if (!dladdr(reinterpret_cast<void *>(&vkGetInstanceProcAddr), &self) || !self.dli_fname) return;
      std::string path(self.dli_fname);
      path.resize(path.find_last_of('/') + 1);
      path += "libnvk_renderer.dylib";
      backend = dlopen(path.c_str(), RTLD_NOW | RTLD_LOCAL | RTLD_FIRST);
      if (!backend) { fprintf(stderr, "NVK Bridge: renderer load failed: %s\n", dlerror()); return; }
      redirect_renderer_sandbox_imports(backend, path);
      backend_gipa = reinterpret_cast<PFN_vkGetInstanceProcAddr>(dlsym(backend, "vk_icdGetInstanceProcAddr"));
      stage_import = reinterpret_cast<StageImport>(dlsym(backend, "nvmtl_nvk_stage_import"));
      surface_dirty = reinterpret_cast<SurfaceDirty>(dlsym(backend, "nvmtl_nvk_surface_dirty"));
      if (!backend_gipa || !stage_import || !surface_dirty) backend_gipa = nullptr;
      fprintf(stderr, "NVK Bridge: renderer=%s; presentation=gpu-copy; game-frame CPU pixel traffic=0\n", path.c_str());
   });
   return backend_gipa != nullptr;
}

template<class T> VkResult enumerate(const std::vector<T> &items, uint32_t *count, T *out) {
   if (!count) return VK_ERROR_INITIALIZATION_FAILED;
   if (!out) { *count = static_cast<uint32_t>(items.size()); return VK_SUCCESS; }
   uint32_t n = std::min(*count, static_cast<uint32_t>(items.size()));
   std::copy_n(items.begin(), n, out);
   *count = n;
   return n < items.size() ? VK_INCOMPLETE : VK_SUCCESS;
}
VkExtensionProperties extension(const char *name, uint32_t version) {
   VkExtensionProperties e{};
   strlcpy(e.extensionName, name, sizeof e.extensionName); e.specVersion = version;
   return e;
}
bool instance_owned(const char *name) {
   return !strcmp(name, VK_KHR_SURFACE_EXTENSION_NAME) || !strcmp(name, VK_EXT_METAL_SURFACE_EXTENSION_NAME) ||
          !strcmp(name, VK_KHR_GET_SURFACE_CAPABILITIES_2_EXTENSION_NAME);
}
#ifdef NVKBRIDGE_WINE
// This frontend owns swapchains. Renderer WSI entry points cannot accept them.
// Its staged IOSurface fd import is private, not general Wine handle sharing.
bool unsupported_extension(const char *name) {
   for (const char *blocked : {"VK_EXT_hdr_metadata", "VK_GOOGLE_display_timing",
        "VK_KHR_present_id", "VK_KHR_present_wait", "VK_KHR_present_id2", "VK_KHR_present_wait2",
        "VK_EXT_swapchain_maintenance1", "VK_KHR_swapchain_maintenance1",
        "VK_KHR_swapchain_mutable_format", "VK_EXT_full_screen_exclusive",
        "VK_EXT_display_control", "VK_AMD_display_native_hdr", "VK_NV_low_latency2",
        "VK_KHR_external_memory_fd", "VK_EXT_external_memory_dma_buf",
        "VK_KHR_external_semaphore_fd", "VK_KHR_external_fence_fd"})
      if (!strcmp(name, blocked)) return true;
   return false;
}
bool unsupported_proc(const char *name) {
   for (const char *blocked : {"vkSetHdrMetadataEXT", "vkGetPastPresentationTimingGOOGLE",
        "vkGetRefreshCycleDurationGOOGLE", "vkWaitForPresentKHR", "vkWaitForPresent2KHR",
        "vkReleaseSwapchainImagesEXT", "vkReleaseSwapchainImagesKHR",
        "vkGetPhysicalDeviceSurfacePresentModes2EXT", "vkAcquireFullScreenExclusiveModeEXT",
        "vkReleaseFullScreenExclusiveModeEXT", "vkGetDeviceGroupSurfacePresentModes2EXT",
        "vkGetSwapchainCounterEXT", "vkSetLocalDimmingAMD", "vkSetLatencySleepModeNV",
        "vkLatencySleepNV", "vkSetLatencyMarkerNV", "vkGetLatencyTimingsNV",
        "vkQueueNotifyOutOfBandNV", "vkGetMemoryFdKHR", "vkGetMemoryFdPropertiesKHR",
        "vkImportSemaphoreFdKHR", "vkGetSemaphoreFdKHR", "vkImportFenceFdKHR", "vkGetFenceFdKHR"})
      if (!strcmp(name, blocked)) return true;
   return false;
}
void VKAPI_CALL external_buffer(VkPhysicalDevice,const VkPhysicalDeviceExternalBufferInfo *,VkExternalBufferProperties *out) {
   out->externalMemoryProperties={};
}
void VKAPI_CALL external_semaphore(VkPhysicalDevice,const VkPhysicalDeviceExternalSemaphoreInfo *,VkExternalSemaphoreProperties *out) {
   out->exportFromImportedHandleTypes=0;out->compatibleHandleTypes=0;out->externalSemaphoreFeatures=0;
}
void VKAPI_CALL external_fence(VkPhysicalDevice,const VkPhysicalDeviceExternalFenceInfo *,VkExternalFenceProperties *out) {
   out->exportFromImportedHandleTypes=0;out->compatibleHandleTypes=0;out->externalFenceFeatures=0;
}
VkResult VKAPI_CALL image_format2(VkPhysicalDevice physical,const VkPhysicalDeviceImageFormatInfo2 *info,VkImageFormatProperties2 *out) {
   auto instance=find(physicals,physical);if (!instance) return VK_ERROR_INITIALIZATION_FAILED;
   for (auto p=(const VkBaseInStructure *)info->pNext;p;p=p->pNext)
      if (p->sType==VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO &&
          ((const VkPhysicalDeviceExternalImageFormatInfo *)p)->handleType) return VK_ERROR_FORMAT_NOT_SUPPORTED;
   return IP(instance,GetPhysicalDeviceImageFormatProperties2)(physical,info,out);
}
#endif
bool valid_format(VkFormat f) { return f == VK_FORMAT_B8G8R8A8_UNORM || f == VK_FORMAT_B8G8R8A8_SRGB; }
uint64_t elapsed(std::chrono::steady_clock::time_point start) {
   return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now() - start).count();
}
int transfer_family(VkPhysicalDevice physical, const std::shared_ptr<Instance> &instance) {
   uint32_t count = 0; IP(instance, GetPhysicalDeviceQueueFamilyProperties)(physical, &count, nullptr);
   std::vector<VkQueueFamilyProperties> props(count);
   IP(instance, GetPhysicalDeviceQueueFamilyProperties)(physical, &count, props.data());
   for (uint32_t i = 0; i < count; i++)
      if (props[i].queueCount && (props[i].queueFlags & VK_QUEUE_TRANSFER_BIT) &&
          !(props[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT))) return static_cast<int>(i);
   for (uint32_t i = 0; i < count; i++)
      if (props[i].queueCount && (props[i].queueFlags & VK_QUEUE_TRANSFER_BIT)) return static_cast<int>(i);
   return -1;
}
id<MTLDevice> metal_device(const VkPhysicalDeviceProperties &props) {
   if (props.vendorID != 0x10de) return nil;
   io_iterator_t iterator{};
   if (IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("NVAccel"), &iterator)) return nil;
   uint64_t selected = 0; unsigned matches = 0;
   for (io_service_t service; (service = IOIteratorNext(iterator)); IOObjectRelease(service)) {
      io_registry_entry_t parent{};
      if (IORegistryEntryGetParentEntry(service, kIOServicePlane, &parent)) continue;
      CFTypeRef data = IORegistryEntryCreateCFProperty(parent, CFSTR("device-id"), kCFAllocatorDefault, 0);
      uint32_t device_id = 0;
      if (data && CFGetTypeID(data) == CFDataGetTypeID() && CFDataGetLength((CFDataRef)data) >= 4)
         memcpy(&device_id, CFDataGetBytePtr((CFDataRef)data), 4);
      if (data) CFRelease(data);
      IOObjectRelease(parent);
      if (device_id == props.deviceID) { IORegistryEntryGetRegistryEntryID(service, &selected); matches++; }
   }
   IOObjectRelease(iterator);
   if (matches != 1) return nil;
   for (id<MTLDevice> device in MTLCopyAllDevices())
      if (device.registryID == selected) return device;
   return nil;
}

#ifdef NVKBRIDGE_WINE
// Wine creates the window's layer with the system-default Intel or AMD device,
// before Vulkan chooses NVIDIA. Only defer this initial foreign
// binding; an NVIDIA-bound layer must retain the existing device checks.
// Called under registry so a second surface cannot reset an owned layer.
void prepare_surface_device(CAMetalLayer *layer) {
   id<MTLDevice> initial=layer.device;
   if (!initial || !initial.registryID) return;
   for (const auto &entry:surfaces)
      if (entry.second->layer==layer) return;
   io_iterator_t iterator{};
   if (IOServiceGetMatchingServices(kIOMainPortDefault,IOServiceMatching("NVAccel"),&iterator)) return;
   bool found=false,complete=true,owned=false;
   for (io_service_t service;(service=IOIteratorNext(iterator));IOObjectRelease(service)) {
      uint64_t identifier=0;
      if (IORegistryEntryGetRegistryEntryID(service,&identifier) || !identifier) complete=false;
      else { found=true;owned|=identifier==initial.registryID; }
   }
   complete&=IOIteratorIsValid(iterator);
   IOObjectRelease(iterator);
   if (found && complete && !owned && layer.device==initial) {
      layer.device=nil;
      fprintf(stderr,"NVK Bridge: deferred initial non-NVIDIA layer device to Vulkan selection\n");
   }
}
#endif

VkResult VKAPI_CALL create_instance(const VkInstanceCreateInfo *info, const VkAllocationCallbacks *alloc, VkInstance *result) {
   if (!initialize()) return VK_ERROR_INCOMPATIBLE_DRIVER;
   std::vector<const char *> extensions;
   for (uint32_t i = 0; i < info->enabledExtensionCount; i++)
      if (!instance_owned(info->ppEnabledExtensionNames[i])) extensions.push_back(info->ppEnabledExtensionNames[i]);
   VkInstanceCreateInfo copy = *info;
   copy.enabledExtensionCount = static_cast<uint32_t>(extensions.size()); copy.ppEnabledExtensionNames = extensions.data();
   VkResult status = global<PFN_vkCreateInstance>("vkCreateInstance")(&copy, alloc, result);
   if (status != VK_SUCCESS) return status;
   auto state = std::make_shared<Instance>(); state->raw = *result;
   std::lock_guard<std::mutex> lock(registry); instances[*result] = state;
   return VK_SUCCESS;
}
void VKAPI_CALL destroy_instance(VkInstance instance, const VkAllocationCallbacks *alloc) {
   if (!instance) return;
   auto state = find(instances, instance);
   if (!state) return;
   { std::lock_guard<std::mutex> lock(registry);
     for (auto it = physicals.begin(); it != physicals.end();) {
        if (it->second == state) it = physicals.erase(it); else ++it;
     }
     instances.erase(instance);
   }
   IP(state, DestroyInstance)(instance, alloc);
}
VkResult VKAPI_CALL instance_extensions(const char *layer, uint32_t *count, VkExtensionProperties *out) {
   if (!initialize()) return VK_ERROR_INCOMPATIBLE_DRIVER;
   auto fn = global<PFN_vkEnumerateInstanceExtensionProperties>("vkEnumerateInstanceExtensionProperties");
   if (layer) return fn(layer, count, out);
   uint32_t n = 0; VkResult r = fn(nullptr, &n, nullptr); if (r != VK_SUCCESS) return r;
   std::vector<VkExtensionProperties> all(n); r = fn(nullptr, &n, all.data()); if (r != VK_SUCCESS) return r;
   all.resize(n);
   all.erase(std::remove_if(all.begin(), all.end(), [](auto &e) { return instance_owned(e.extensionName) ||
      !strcmp(e.extensionName, VK_EXT_HEADLESS_SURFACE_EXTENSION_NAME); }), all.end());
   all.push_back(extension(VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_SURFACE_SPEC_VERSION));
   all.push_back(extension(VK_EXT_METAL_SURFACE_EXTENSION_NAME, VK_EXT_METAL_SURFACE_SPEC_VERSION));
   all.push_back(extension(VK_KHR_GET_SURFACE_CAPABILITIES_2_EXTENSION_NAME, VK_KHR_GET_SURFACE_CAPABILITIES_2_SPEC_VERSION));
   return enumerate(all, count, out);
}
VkResult VKAPI_CALL instance_layers(uint32_t *count, VkLayerProperties *out) {
   return enumerate(std::vector<VkLayerProperties>{}, count, out);
}
VkResult VKAPI_CALL instance_version(uint32_t *version) {
   if (!initialize()) return VK_ERROR_INCOMPATIBLE_DRIVER;
   auto fn = global<PFN_vkEnumerateInstanceVersion>("vkEnumerateInstanceVersion");
   return fn ? fn(version) : (*version = VK_API_VERSION_1_0, VK_SUCCESS);
}
VkResult VKAPI_CALL enumerate_physical(VkInstance instance, uint32_t *count, VkPhysicalDevice *out) {
   auto state = find(instances, instance); if (!state) return VK_ERROR_INITIALIZATION_FAILED;
   VkResult result = IP(state, EnumeratePhysicalDevices)(instance, count, out);
   if (out && result >= 0) { std::lock_guard<std::mutex> lock(registry); for (uint32_t i=0;i<*count;i++) physicals[out[i]] = state; }
   return result;
}
VkResult VKAPI_CALL device_extensions(VkPhysicalDevice physical, const char *layer, uint32_t *count, VkExtensionProperties *out) {
   auto state = find(physicals, physical); if (!state) return VK_ERROR_INITIALIZATION_FAILED;
   auto fn = IP(state, EnumerateDeviceExtensionProperties);
   if (layer) return fn(physical, layer, count, out);
   uint32_t n=0; VkResult r=fn(physical, nullptr, &n, nullptr); if (r != VK_SUCCESS) return r;
   std::vector<VkExtensionProperties> all(n); r=fn(physical, nullptr, &n, all.data()); if (r != VK_SUCCESS) return r;
   all.resize(n);
   all.erase(std::remove_if(all.begin(),all.end(),[](auto &e) {
#ifdef NVKBRIDGE_WINE
      if (unsupported_extension(e.extensionName)) return true;
#endif
      return !strcmp(e.extensionName, VK_KHR_SWAPCHAIN_EXTENSION_NAME);
   }),all.end());
   if (transfer_family(physical, state) >= 0) all.push_back(extension(VK_KHR_SWAPCHAIN_EXTENSION_NAME, VK_KHR_SWAPCHAIN_SPEC_VERSION));
   return enumerate(all,count,out);
}

bool limited_bar_memory(const VkPhysicalDeviceMemoryProperties &memory) {
   VkDeviceSize vram=0;
   for(uint32_t h=0;h<memory.memoryHeapCount;h++)
      if(memory.memoryHeaps[h].flags&VK_MEMORY_HEAP_DEVICE_LOCAL_BIT)
         vram=std::max(vram,memory.memoryHeaps[h].size);
   if(!vram)return false;
   bool system_uploads=false;
   for(uint32_t t=0;t<memory.memoryTypeCount;t++) {
      const auto &type=memory.memoryTypes[t];
      if(type.heapIndex>=memory.memoryHeapCount)continue;
      const auto flags=type.propertyFlags;
      if((flags&(VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT|VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT))==
         (VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT|VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) &&
         memory.memoryHeaps[type.heapIndex].size>=vram)return false;
      if(!(flags&VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) &&
         (flags&(VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))==
         (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))system_uploads=true;
   }
   return system_uploads;
}

void enable_host_present(const std::shared_ptr<Device> &d,std::vector<const char *> &extensions) {
   // Full BAR keeps its original device extensions and VRAM import path. Only
   // limited-BAR devices may recover a refused drawable import using its host
   // pages, as Zink does. Pixels still travel directly from the GPU.
   if(!limited_bar_memory(d->memory))return;
   uint32_t count=0;
   auto enumerate_extensions=IP(d->instance,EnumerateDeviceExtensionProperties);
   if(enumerate_extensions(d->physical,nullptr,&count,nullptr)!=VK_SUCCESS)return;
   std::vector<VkExtensionProperties> available(count);
   if(enumerate_extensions(d->physical,nullptr,&count,available.data())!=VK_SUCCESS)return;
   available.resize(count);
   if(std::none_of(available.begin(),available.end(),[](const auto &e) {
      return !strcmp(e.extensionName,VK_EXT_EXTERNAL_MEMORY_HOST_EXTENSION_NAME);
   }))return;
   auto properties=IP(d->instance,GetPhysicalDeviceProperties2);
   if(!properties)properties=iproc<PFN_vkGetPhysicalDeviceProperties2>(d->instance->raw,"vkGetPhysicalDeviceProperties2KHR");
   if(!properties)return;
   VkPhysicalDeviceExternalMemoryHostPropertiesEXT host{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_MEMORY_HOST_PROPERTIES_EXT};
   VkPhysicalDeviceProperties2 all{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2};all.pNext=&host;
   properties(d->physical,&all);
   const auto alignment=host.minImportedHostPointerAlignment;
   if(!alignment || alignment>SIZE_MAX || (alignment&(alignment-1)))return;
   if(std::none_of(extensions.begin(),extensions.end(),[](auto name) {
      return !strcmp(name,VK_EXT_EXTERNAL_MEMORY_HOST_EXTENSION_NAME);
   }))extensions.push_back(VK_EXT_EXTERNAL_MEMORY_HOST_EXTENSION_NAME);
   d->host_present_alignment=alignment;
}

VkResult VKAPI_CALL create_device(VkPhysicalDevice physical, const VkDeviceCreateInfo *info, const VkAllocationCallbacks *alloc, VkDevice *result) {
   auto instance = find(physicals,physical); if (!instance) return VK_ERROR_INITIALIZATION_FAILED;
   auto state = std::make_shared<Device>(); state->physical=physical; state->instance=instance;
#ifdef NVKBRIDGE_WINE
   // Wine options stay process-local and survive Wine children. Reuse completed
   // presentation resources by default; keep an explicit troubleshooting opt-out.
   const char *reuse=getenv("WINE_NVK_PRESENT_REUSE"),*profile=getenv("WINE_NVK_PRESENT_PROFILE");
   state->reuse_present_resources=!reuse || strcmp(reuse,"0")!=0;
   state->profile_present=profile && !strcmp(profile,"1");
#endif
   IP(instance, GetPhysicalDeviceProperties)(physical,&state->properties);
   IP(instance, GetPhysicalDeviceMemoryProperties)(physical,&state->memory);
   std::vector<const char *> extensions;
   for (uint32_t i=0;i<info->enabledExtensionCount;i++) {
      const char *name=info->ppEnabledExtensionNames[i];
#ifdef NVKBRIDGE_WINE
      if (unsupported_extension(name)) return VK_ERROR_EXTENSION_NOT_PRESENT;
#endif
      if (!strcmp(name,VK_KHR_SWAPCHAIN_EXTENSION_NAME)) state->swapchain=true;
      else extensions.push_back(name);
   }
   std::vector<VkDeviceQueueCreateInfo> queue_info(info->pQueueCreateInfos,info->pQueueCreateInfos+info->queueCreateInfoCount);
   float priority=1.0f;
   std::vector<float> expanded_priorities;
   if (state->swapchain) {
      int family=transfer_family(physical,instance);
      if (family<0 || !(state->metal=metal_device(state->properties))) {
         fprintf(stderr,"NVK Bridge: a matching NVIDIA Metal device and transfer-capable queue are required\n");
         return VK_ERROR_FEATURE_NOT_PRESENT;
      }
      state->copy_family=static_cast<uint32_t>(family);
      bool present=false;
      for (auto &q:queue_info) if (q.queueFamilyIndex==state->copy_family) {
         uint32_t n=0; IP(instance,GetPhysicalDeviceQueueFamilyProperties)(physical,&n,nullptr);
         std::vector<VkQueueFamilyProperties> props(n); IP(instance,GetPhysicalDeviceQueueFamilyProperties)(physical,&n,props.data());
         state->copy_index=q.queueCount<props[family].queueCount?q.queueCount:0;
         if (state->copy_index) {
            state->private_copy_queue=true;
            expanded_priorities.assign(q.pQueuePriorities,q.pQueuePriorities+q.queueCount);
            expanded_priorities.push_back(priority); q.queueCount++; q.pQueuePriorities=expanded_priorities.data();
         }
         present=true; break;
      }
      if (!present) {
         state->private_copy_queue=true;
         VkDeviceQueueCreateInfo q{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
         q.queueFamilyIndex=state->copy_family; q.queueCount=1; q.pQueuePriorities=&priority; queue_info.push_back(q);
      }
      if (std::none_of(extensions.begin(),extensions.end(),[](auto name) { return !strcmp(name,VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME); }))
         extensions.push_back(VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME);
      enable_host_present(state,extensions);
   }
   VkDeviceCreateInfo copy=*info;
   copy.enabledExtensionCount=static_cast<uint32_t>(extensions.size()); copy.ppEnabledExtensionNames=extensions.data();
   copy.queueCreateInfoCount=static_cast<uint32_t>(queue_info.size()); copy.pQueueCreateInfos=queue_info.data();
   VkResult status=IP(instance,CreateDevice)(physical,&copy,alloc,result); if (status!=VK_SUCCESS) return status;
   state->raw=*result;
   std::call_once(device_procs_once,[&] { backend_gdpa=IP(instance,GetDeviceProcAddr); });
   if (state->swapchain) DP(state,GetDeviceQueue)(*result,state->copy_family,state->copy_index,&state->copy_queue);
   // Wine defaults to bounded asynchronous presentation with an explicit
   // opt-out. A shared application/copy queue retains synchronous ordering.
   if(state->swapchain && state->private_copy_queue && async_present_requested() && register_presenter_shutdown()) {
      try { state->presenter=std::make_unique<nvkbridge::PresentWorker>(
         [device=state.get()](std::exception_ptr){fail_presenter(device);}); }
      catch(...) { fprintf(stderr,"NVK Bridge: presenter thread unavailable; using synchronous presentation\n"); }
   }
   { std::lock_guard<std::mutex> lock(registry); devices[*result]=state; }
   fprintf(stderr,"NVK Bridge: native device=%s; copy family=%u; Metal registry=%llu\n",
      state->properties.deviceName,state->copy_family,(unsigned long long)state->metal.registryID);
   fprintf(stderr,"NVK Bridge: presentation completion=%s; private copy queue=%s\n",
      state->presenter?"asynchronous (bounded to 2 jobs)":"synchronous",state->private_copy_queue?"yes":"no");
   return VK_SUCCESS;
}
void VKAPI_CALL get_queue(VkDevice device,uint32_t family,uint32_t index,VkQueue *out) {
   auto state=find(devices,device); if (!state) { *out=VK_NULL_HANDLE; return; }
   DP(state,GetDeviceQueue)(device,family,index,out);
   auto q=std::make_shared<Queue>(); q->raw=*out; q->family=family; q->dev=state;
   std::lock_guard<std::mutex> lock(registry); queues[*out]=q;
}
void VKAPI_CALL get_queue2(VkDevice device,const VkDeviceQueueInfo2 *info,VkQueue *out) {
   auto state=find(devices,device); if (!state) { *out=VK_NULL_HANDLE; return; }
   DP(state,GetDeviceQueue2)(device,info,out);
   auto q=std::make_shared<Queue>(); q->raw=*out; q->family=info->queueFamilyIndex; q->dev=state;
   std::lock_guard<std::mutex> lock(registry); queues[*out]=q;
}
VkResult VKAPI_CALL queue_submit(VkQueue queue,uint32_t count,const VkSubmitInfo *infos,VkFence fence) {
   auto q=find(queues,queue); auto d=q?q->dev.lock():nullptr; if (!d || d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::recursive_mutex> lock(d->submit); return d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,QueueSubmit)(queue,count,infos,fence);
}
VkResult VKAPI_CALL queue_submit2(VkQueue queue,uint32_t count,const VkSubmitInfo2 *infos,VkFence fence) {
   auto q=find(queues,queue); auto d=q?q->dev.lock():nullptr; if (!d || d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::recursive_mutex> lock(d->submit);
   if(d->uncertain)return VK_ERROR_DEVICE_LOST;
   auto fn=DP(d,QueueSubmit2); if (!fn) fn=dproc<PFN_vkQueueSubmit2>(d->raw,"vkQueueSubmit2KHR");
   return fn?fn(queue,count,infos,fence):VK_ERROR_EXTENSION_NOT_PRESENT;
}
VkResult VKAPI_CALL bind_sparse(VkQueue queue,uint32_t count,const VkBindSparseInfo *infos,VkFence fence) {
   auto q=find(queues,queue); auto d=q?q->dev.lock():nullptr; if (!d || d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::recursive_mutex> lock(d->submit); return d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,QueueBindSparse)(queue,count,infos,fence);
}
VkResult VKAPI_CALL queue_idle(VkQueue queue) {
   auto q=find(queues,queue); auto d=q?q->dev.lock():nullptr; if (!d || d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::mutex> gate(d->present_call);drain_presenter(d);
   std::lock_guard<std::recursive_mutex> lock(d->submit); return d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,QueueWaitIdle)(queue);
}
VkResult VKAPI_CALL device_idle(VkDevice device) {
   auto d=find(devices,device); if (!d || d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::mutex> gate(d->present_call);drain_presenter(d);
   std::lock_guard<std::recursive_mutex> lock(d->submit); return d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,DeviceWaitIdle)(device);
}
void VKAPI_CALL destroy_device(VkDevice device,const VkAllocationCallbacks *alloc) {
   auto d=find(devices,device); if (!d) return;
   std::lock_guard<std::mutex> gate(d->present_call);drain_presenter(d);
   if(d->presenter)d->presenter->stop();
   std::lock_guard<std::recursive_mutex> lock(d->submit);
   if (d->uncertain) { fprintf(stderr,"NVK Bridge: uncertain GPU completion; retaining native device until process exit\n"); return; }
   DP(d,DeviceWaitIdle)(device);
   { std::lock_guard<std::mutex> guard(registry);
     for (auto it=queues.begin();it!=queues.end();) { if (it->second->dev.lock()==d) it=queues.erase(it); else ++it; }
     devices.erase(device);
   }
   DP(d,DestroyDevice)(device,alloc);
}

VkResult VKAPI_CALL create_surface(VkInstance instance,const VkMetalSurfaceCreateInfoEXT *info,const VkAllocationCallbacks *,VkSurfaceKHR *out) {
   if (!find(instances,instance) || !info || !info->pLayer || info->flags) return VK_ERROR_INITIALIZATION_FAILED;
   auto surface=std::make_shared<Surface>(); surface->layer=(CAMetalLayer *)info->pLayer;
   if (![surface->layer isKindOfClass:[CAMetalLayer class]]) return VK_ERROR_INITIALIZATION_FAILED;
#ifdef NVKBRIDGE_WINE
   if (nvkbridge_hud::enabled()) surface->hud=std::make_shared<nvkbridge_hud::State>();
#endif
   *out=(VkSurfaceKHR)(uintptr_t)next_handle.fetch_add(1);
   std::lock_guard<std::mutex> lock(registry);
#ifdef NVKBRIDGE_WINE
   prepare_surface_device(surface->layer);
#endif
   surfaces[*out]=surface;
   return VK_SUCCESS;
}
void VKAPI_CALL destroy_surface(VkInstance,VkSurfaceKHR surface,const VkAllocationCallbacks *) {
   std::lock_guard<std::mutex> lock(registry);
   surfaces.erase(surface);
}
VkResult VKAPI_CALL surface_support(VkPhysicalDevice physical,uint32_t family,VkSurfaceKHR surface,VkBool32 *supported) {
   *supported=VK_FALSE;
   auto i=find(physicals,physical); auto s=find(surfaces,surface); if (!i || !s) return VK_ERROR_SURFACE_LOST_KHR;
   uint32_t count=0; IP(i,GetPhysicalDeviceQueueFamilyProperties)(physical,&count,nullptr);
   if (family>=count) return VK_SUCCESS;
   std::vector<VkQueueFamilyProperties> props(count); IP(i,GetPhysicalDeviceQueueFamilyProperties)(physical,&count,props.data());
   VkPhysicalDeviceProperties device{}; IP(i,GetPhysicalDeviceProperties)(physical,&device);
   id<MTLDevice> m=metal_device(device);
   *supported= m && transfer_family(physical,i)>=0 &&
      (props[family].queueFlags & (VK_QUEUE_GRAPHICS_BIT|VK_QUEUE_COMPUTE_BIT))==(VK_QUEUE_GRAPHICS_BIT|VK_QUEUE_COMPUTE_BIT) &&
      (!s->layer.device || s->layer.device.registryID==m.registryID);
   return VK_SUCCESS;
}
VkResult VKAPI_CALL surface_caps(VkPhysicalDevice physical,VkSurfaceKHR surface,VkSurfaceCapabilitiesKHR *caps) {
   auto i=find(physicals,physical); auto s=find(surfaces,surface); if (!i || !s) return VK_ERROR_SURFACE_LOST_KHR;
   VkPhysicalDeviceProperties props{}; IP(i,GetPhysicalDeviceProperties)(physical,&props);
   *caps={}; caps->minImageCount=2; caps->maxImageCount=8;
   CGSize size=s->layer.bounds.size; CGFloat scale=s->layer.contentsScale;
   uint32_t w=(uint32_t)std::max(0.0,size.width*scale),h=(uint32_t)std::max(0.0,size.height*scale);
   caps->currentExtent=w&&h?VkExtent2D{w,h}:VkExtent2D{UINT32_MAX,UINT32_MAX};
   caps->minImageExtent={1,1}; caps->maxImageExtent={props.limits.maxImageDimension2D,props.limits.maxImageDimension2D};
   caps->maxImageArrayLayers=1; caps->supportedTransforms=VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR; caps->currentTransform=VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR;
   caps->supportedCompositeAlpha=VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR|VK_COMPOSITE_ALPHA_PRE_MULTIPLIED_BIT_KHR;
   caps->supportedUsageFlags=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT;
   return VK_SUCCESS;
}
VkResult VKAPI_CALL surface_caps2(VkPhysicalDevice physical,const VkPhysicalDeviceSurfaceInfo2KHR *info,VkSurfaceCapabilities2KHR *out) {
   for (VkBaseOutStructure *p=(VkBaseOutStructure *)out->pNext;p;p=p->pNext)
      if (p->sType==VK_STRUCTURE_TYPE_SURFACE_PROTECTED_CAPABILITIES_KHR)
         ((VkSurfaceProtectedCapabilitiesKHR *)p)->supportsProtected=VK_FALSE;
   return surface_caps(physical,info->surface,&out->surfaceCapabilities);
}
VkResult VKAPI_CALL surface_formats(VkPhysicalDevice physical,VkSurfaceKHR surface,uint32_t *count,VkSurfaceFormatKHR *out) {
   if (!find(physicals,physical) || !find(surfaces,surface)) return VK_ERROR_SURFACE_LOST_KHR;
   return enumerate(std::vector<VkSurfaceFormatKHR>{{VK_FORMAT_B8G8R8A8_SRGB,VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
      {VK_FORMAT_B8G8R8A8_UNORM,VK_COLOR_SPACE_SRGB_NONLINEAR_KHR}},count,out);
}
VkResult VKAPI_CALL surface_formats2(VkPhysicalDevice physical,const VkPhysicalDeviceSurfaceInfo2KHR *info,uint32_t *count,VkSurfaceFormat2KHR *out) {
   if (!out) return surface_formats(physical,info->surface,count,nullptr);
   uint32_t n=0;VkResult r=surface_formats(physical,info->surface,&n,nullptr);if (r!=VK_SUCCESS) return r;
   std::vector<VkSurfaceFormatKHR> formats(n);
   r=surface_formats(physical,info->surface,&n,formats.data());if (r!=VK_SUCCESS) return r;
   uint32_t written=std::min(n,*count);
   for (uint32_t j=0;j<written;j++) out[j].surfaceFormat=formats[j]; *count=written;
   return written<n?VK_INCOMPLETE:VK_SUCCESS;
}
VkResult VKAPI_CALL present_modes(VkPhysicalDevice physical,VkSurfaceKHR surface,uint32_t *count,VkPresentModeKHR *out) {
   if (!find(physicals,physical) || !find(surfaces,surface)) return VK_ERROR_SURFACE_LOST_KHR;
   return enumerate(std::vector<VkPresentModeKHR>{VK_PRESENT_MODE_FIFO_KHR,VK_PRESENT_MODE_IMMEDIATE_KHR},count,out);
}

int memory_type(const Device &d,uint32_t bits,VkMemoryPropertyFlags flags) {
   for (uint32_t i=0;i<d.memory.memoryTypeCount;i++) if ((bits&(1u<<i)) && (d.memory.memoryTypes[i].propertyFlags&flags)==flags) return (int)i;
   return -1;
}
void release_images(const std::shared_ptr<Chain> &c) {
   if (c->dev->uncertain) return;
   for(auto &entry:c->present_cache)entry.reset();
   c->present_cache_bytes=0;
#ifdef NVKBRIDGE_WINE
   if(c->hud_mapping)DP(c->dev,UnmapMemory)(c->dev->raw,c->hud_memory);
   if(c->hud_buffer)DP(c->dev,DestroyBuffer)(c->dev->raw,c->hud_buffer,c->alloc());
   if(c->hud_memory)DP(c->dev,FreeMemory)(c->dev->raw,c->hud_memory,c->alloc());
   c->hud_mapping=nullptr;c->hud_buffer=VK_NULL_HANDLE;c->hud_memory=VK_NULL_HANDLE;c->hud_version=0;
#endif
   for (auto &im:c->images) {
      if (im.raw) DP(c->dev,DestroyImage)(c->dev->raw,im.raw,c->alloc());
      if (im.memory) DP(c->dev,FreeMemory)(c->dev->raw,im.memory,c->alloc());
      im={};
   }
}
VkResult VKAPI_CALL create_swapchain(VkDevice device,const VkSwapchainCreateInfoKHR *info,const VkAllocationCallbacks *alloc,VkSwapchainKHR *out) {
   auto d=find(devices,device); auto s=find(surfaces,info->surface);
   if (!d || !d->swapchain || !s) return VK_ERROR_SURFACE_LOST_KHR;
   if (d->uncertain) return VK_ERROR_DEVICE_LOST;
   if (!valid_format(info->imageFormat) || info->imageColorSpace!=VK_COLOR_SPACE_SRGB_NONLINEAR_KHR) return VK_ERROR_FORMAT_NOT_SUPPORTED;
   if (info->flags || info->imageArrayLayers!=1 || info->minImageCount<2 || info->minImageCount>8 ||
       !info->imageExtent.width || !info->imageExtent.height ||
       info->imageExtent.width>d->properties.limits.maxImageDimension2D || info->imageExtent.height>d->properties.limits.maxImageDimension2D ||
       info->preTransform!=VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR ||
       (info->presentMode!=VK_PRESENT_MODE_FIFO_KHR && info->presentMode!=VK_PRESENT_MODE_IMMEDIATE_KHR)) return VK_ERROR_INITIALIZATION_FAILED;
   VkSurfaceCapabilitiesKHR caps{}; surface_caps(d->physical,info->surface,&caps);
   if ((info->imageUsage&~caps.supportedUsageFlags) || !(info->compositeAlpha&caps.supportedCompositeAlpha)) return VK_ERROR_FEATURE_NOT_PRESENT;
   std::lock_guard<std::mutex> gate(d->present_call);drain_presenter(d);
   if(d->uncertain)return VK_ERROR_DEVICE_LOST;
   std::lock_guard<std::recursive_mutex> submit(d->submit);
   if (s->layer.device && s->layer.device.registryID!=d->metal.registryID) return VK_ERROR_SURFACE_LOST_KHR;
   s->layer.device=d->metal; s->layer.pixelFormat=info->imageFormat==VK_FORMAT_B8G8R8A8_SRGB?MTLPixelFormatBGRA8Unorm_sRGB:MTLPixelFormatBGRA8Unorm;
   s->layer.framebufferOnly=NO; s->layer.allowsNextDrawableTimeout=YES;
   s->layer.maximumDrawableCount=std::min(3u,info->minImageCount);
   s->layer.drawableSize=CGSizeMake(info->imageExtent.width,info->imageExtent.height);
   s->layer.displaySyncEnabled=info->presentMode!=VK_PRESENT_MODE_IMMEDIATE_KHR;
   s->layer.opaque=info->compositeAlpha==VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR;
   auto c=std::make_shared<Chain>(); c->dev=d;c->surface=s;c->format=info->imageFormat;c->extent=info->imageExtent;c->present_mode=info->presentMode;
   if (alloc) { c->allocator=*alloc; c->custom_allocator=true; }
   c->images.resize(info->minImageCount);
   std::vector<uint32_t> families;
   if (info->imageSharingMode==VK_SHARING_MODE_CONCURRENT && info->queueFamilyIndexCount)
      families.assign(info->pQueueFamilyIndices,info->pQueueFamilyIndices+info->queueFamilyIndexCount);
   if (families.empty()) {
      uint32_t n=0; IP(d->instance,GetPhysicalDeviceQueueFamilyProperties)(d->physical,&n,nullptr);
      for (uint32_t j=0;j<n;j++) families.push_back(j);
   }
   if (std::find(families.begin(),families.end(),d->copy_family)==families.end()) families.push_back(d->copy_family);
   std::sort(families.begin(),families.end()); families.erase(std::unique(families.begin(),families.end()),families.end());
   VkImageCreateInfo image{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
   image.imageType=VK_IMAGE_TYPE_2D;image.format=c->format;image.extent={c->extent.width,c->extent.height,1};
   image.mipLevels=1;image.arrayLayers=1;image.samples=VK_SAMPLE_COUNT_1_BIT;image.tiling=VK_IMAGE_TILING_OPTIMAL;
   image.usage=info->imageUsage|VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
#ifdef NVKBRIDGE_WINE
   if(s->hud)image.usage|=VK_IMAGE_USAGE_TRANSFER_DST_BIT;
#endif
   image.sharingMode=families.size()>1?VK_SHARING_MODE_CONCURRENT:VK_SHARING_MODE_EXCLUSIVE;
   image.queueFamilyIndexCount=families.size()>1?(uint32_t)families.size():0;image.pQueueFamilyIndices=families.data();
   for (auto &im:c->images) {
      VkResult r=DP(d,CreateImage)(device,&image,c->alloc(),&im.raw); if (r!=VK_SUCCESS) { release_images(c);return r; }
      VkMemoryRequirements req{};DP(d,GetImageMemoryRequirements)(device,im.raw,&req);
      int mt=memory_type(*d,req.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
      if (mt<0) { release_images(c);return VK_ERROR_OUT_OF_DEVICE_MEMORY; }
      VkMemoryAllocateInfo allocation{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};allocation.allocationSize=req.size;allocation.memoryTypeIndex=mt;
      r=DP(d,AllocateMemory)(device,&allocation,c->alloc(),&im.memory);
      if (r==VK_SUCCESS) r=DP(d,BindImageMemory)(device,im.raw,im.memory,0);
      if (r!=VK_SUCCESS) { release_images(c);return r; }
   }
   c->handle=(VkSwapchainKHR)(uintptr_t)next_handle.fetch_add(1);*out=c->handle;
   { std::lock_guard<std::mutex> lock(registry);chains[c->handle]=c; }
   if (auto old=find(chains,info->oldSwapchain)) { std::lock_guard<std::mutex> lock(old->mutex);old->retired=true;old->available.notify_all(); }
   fprintf(stderr,"NVK Bridge: swapchain %ux%u, %u Vulkan images, %lu Metal slots\n",c->extent.width,c->extent.height,info->minImageCount,(unsigned long)s->layer.maximumDrawableCount);
   return VK_SUCCESS;
}
void VKAPI_CALL destroy_swapchain(VkDevice device,VkSwapchainKHR swapchain,const VkAllocationCallbacks *) {
   auto c=find(chains,swapchain);if (!c || c->dev->raw!=device) return;
   std::lock_guard<std::mutex> gate(c->dev->present_call);
   { std::lock_guard<std::mutex> lock(c->mutex);c->retired=true;c->available.notify_all(); }
   drain_presenter(c->dev);
   std::lock_guard<std::recursive_mutex> submit(c->dev->submit);
   if (c->dev->uncertain) return;
   release_images(c);
   { std::lock_guard<std::mutex> lock(registry);chains.erase(swapchain); }
   fprintf(stderr,"NVK Bridge: gpu_copy_count=%llu gpu_copy_bytes=%llu cpu_pixel_readback_bytes=0 metal_cpu_upload_bytes=0\n",
      (unsigned long long)copies.load(),(unsigned long long)copy_bytes.load());
#ifdef NVKBRIDGE_WINE
   if(c->surface->hud)fprintf(stderr,"NVK HUD: cumulative bitmap CPU upload bytes=%llu; no game-frame readback\n",(unsigned long long)hud_upload_bytes.load());
#endif
}
VkResult VKAPI_CALL swapchain_images(VkDevice device,VkSwapchainKHR swapchain,uint32_t *count,VkImage *out) {
   auto c=find(chains,swapchain);if (!c || c->dev->raw!=device) return VK_ERROR_OUT_OF_DATE_KHR;
   std::vector<VkImage> images;for (auto &im:c->images) images.push_back(im.raw);return enumerate(images,count,out);
}
VkResult VKAPI_CALL acquire_image(VkDevice device,VkSwapchainKHR swapchain,uint64_t timeout,VkSemaphore semaphore,VkFence fence,uint32_t *index) {
   auto c=find(chains,swapchain);if (!c || c->dev->raw!=device) return VK_ERROR_OUT_OF_DATE_KHR;
   if (!semaphore && !fence) return VK_ERROR_INITIALIZATION_FAILED;
   auto d=c->dev;if (d->uncertain) return VK_ERROR_DEVICE_LOST;
   std::unique_lock<std::mutex> lock(c->mutex);
   auto ready=[&] { return c->retired || d->uncertain || c->present_error!=VK_SUCCESS || std::any_of(c->images.begin(),c->images.end(),[](auto &im){return !im.acquired;}); };
   if (!ready()) {
      if (!timeout) return VK_NOT_READY;
      if (timeout==UINT64_MAX) c->available.wait(lock,ready);
      else if (!c->available.wait_for(lock,std::chrono::nanoseconds(std::min(timeout,(uint64_t)INT64_MAX)),ready)) return VK_TIMEOUT;
   }
   if (c->retired) return VK_ERROR_OUT_OF_DATE_KHR;
   if (d->uncertain) return VK_ERROR_DEVICE_LOST;
   if (c->present_error!=VK_SUCCESS) return c->present_error;
   for (uint32_t j=0;j<c->images.size();j++) {
      uint32_t k=(c->cursor+j)%(uint32_t)c->images.size();
      if (!c->images[k].acquired) { c->images[k].acquired=true;*index=k;c->cursor=(k+1)%(uint32_t)c->images.size();break; }
   }
   lock.unlock();
   VkSubmitInfo signal{VK_STRUCTURE_TYPE_SUBMIT_INFO};signal.signalSemaphoreCount=semaphore?1:0;signal.pSignalSemaphores=&semaphore;
   VkResult r;
   { std::lock_guard<std::recursive_mutex> submit(d->submit);r=d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,QueueSubmit)(d->copy_queue,1,&signal,fence); }
   if (r!=VK_SUCCESS) { std::lock_guard<std::mutex> rollback(c->mutex);c->images[*index].acquired=false;c->available.notify_all(); }
   return r;
}
VkResult VKAPI_CALL acquire_image2(VkDevice device,const VkAcquireNextImageInfoKHR *info,uint32_t *index) {
   if (info->deviceMask!=1) return VK_ERROR_FEATURE_NOT_PRESENT;
   return acquire_image(device,info->swapchain,info->timeout,info->semaphore,info->fence,index);
}

struct Frame {
   std::shared_ptr<Device> dev;
   // Only uncertain GPU work retains the chain. Idle cached entries must not
   // form a Chain -> Frame -> Chain ownership cycle.
   std::shared_ptr<Chain> chain;
   id<CAMetalDrawable> __strong drawable;
   IOSurfaceRef surface{};
   VkBuffer buffer{};
   VkDeviceMemory memory{};
   VkCommandPool pool{};
   VkCommandBuffer commands{};
   VkFence fence{};
   uint64_t bytes{};
   size_t row{}, allocation_size{};
   void *host_pointer{};
   bool host_locked{};
   ~Frame() {
      auto d=dev;
      if (d->uncertain) return;
      PresentTimer cleanup(d->profile_present,8);
      if (pool) DP(d,DestroyCommandPool)(d->raw,pool,nullptr);
      if (fence) DP(d,DestroyFence)(d->raw,fence,nullptr);
      if (buffer) DP(d,DestroyBuffer)(d->raw,buffer,nullptr);
      if (memory) DP(d,FreeMemory)(d->raw,memory,nullptr);
      if (host_locked) IOSurfaceUnlock(surface,0,nullptr);
      if (surface) CFRelease(surface);
   }
};
std::vector<std::shared_ptr<Frame>> quarantine;

VkResult import_host_frame(const std::shared_ptr<Frame> &frame,VkBufferCreateInfo buffer) {
   auto d=frame->dev;
   const size_t alignment=d->host_present_alignment;
   auto host_properties=DP(d,GetMemoryHostPointerPropertiesEXT);
   if(!alignment || !host_properties)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
   if(IOSurfaceLock(frame->surface,0,nullptr)!=kIOReturnSuccess)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
   frame->host_locked=true;
   void *base=IOSurfaceGetBaseAddress(frame->surface);
   if(!base || reinterpret_cast<uintptr_t>(base)%alignment)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
   DP(d,DestroyBuffer)(d->raw,frame->buffer,nullptr);frame->buffer=VK_NULL_HANDLE;
   VkExternalMemoryBufferCreateInfo external{VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO};
   constexpr auto handle_type=VK_EXTERNAL_MEMORY_HANDLE_TYPE_HOST_ALLOCATION_BIT_EXT;
   external.handleTypes=handle_type;buffer.pNext=&external;
   VkResult r=DP(d,CreateBuffer)(d->raw,&buffer,nullptr,&frame->buffer);if(r!=VK_SUCCESS)return r;
   VkMemoryRequirements requirements{};DP(d,GetBufferMemoryRequirements)(d->raw,frame->buffer,&requirements);
   if(requirements.size>SIZE_MAX-(alignment-1))return VK_ERROR_OUT_OF_DEVICE_MEMORY;
   const size_t bytes=(requirements.size+alignment-1)&~(alignment-1);
   if(bytes>frame->allocation_size)return VK_ERROR_OUT_OF_DEVICE_MEMORY;
   VkMemoryHostPointerPropertiesEXT properties{VK_STRUCTURE_TYPE_MEMORY_HOST_POINTER_PROPERTIES_EXT};
   r=host_properties(d->raw,handle_type,base,&properties);if(r!=VK_SUCCESS)return r;
   int type=memory_type(*d,requirements.memoryTypeBits&properties.memoryTypeBits,
      VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
   if(type<0)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
   VkImportMemoryHostPointerInfoEXT host{VK_STRUCTURE_TYPE_IMPORT_MEMORY_HOST_POINTER_INFO_EXT};
   host.handleType=handle_type;host.pHostPointer=base;
   VkMemoryAllocateInfo allocation{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
   allocation.pNext=&host;allocation.allocationSize=bytes;allocation.memoryTypeIndex=type;
   r=DP(d,AllocateMemory)(d->raw,&allocation,nullptr,&frame->memory);
   if(r==VK_SUCCESS)frame->host_pointer=base;
   return r;
}

void publish_tag(IOSurfaceRef surface) {
   struct Tag { uint32_t magic,seed,generation; } tag{0x564d5634u,IOSurfaceGetSeed(surface),0};
   CFTypeRef old=IOSurfaceCopyValue(surface,CFSTR("NVMTLVramAt:p0"));
   if (old && CFGetTypeID(old)==CFDataGetTypeID() && CFDataGetLength((CFDataRef)old)==sizeof tag) {
      Tag prior{};memcpy(&prior,CFDataGetBytePtr((CFDataRef)old),sizeof prior);
      if (prior.magic==tag.magic) tag.generation=prior.generation+1;
   }
   if (old) CFRelease(old);
   CFDataRef data=CFDataCreate(nullptr,(const UInt8 *)&tag,sizeof tag);
   if (data) { IOSurfaceSetValue(surface,CFSTR("NVMTLVramAt:p0"),data);CFRelease(data); }
}
#ifdef NVKBRIDGE_WINE
std::shared_ptr<const nvkbridge_hud::Bitmap> prepare_hud(const std::shared_ptr<Chain> &c) {
   if(!c->surface->hud || c->hud_disabled)return {};
   auto bitmap=c->surface->hud->bitmap(c->extent.width,c->extent.height);if(!bitmap)return {};
   auto d=c->dev;
   if(!c->hud_buffer) {
      VkBufferCreateInfo info{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};info.size=bitmap->pixels.size()*4;
      info.usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT;info.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
      VkResult result=DP(d,CreateBuffer)(d->raw,&info,c->alloc(),&c->hud_buffer);
      if(result==VK_SUCCESS) {
         VkMemoryRequirements requirements{};DP(d,GetBufferMemoryRequirements)(d->raw,c->hud_buffer,&requirements);
         int type=memory_type(*d,requirements.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
         if(type<0)result=VK_ERROR_FEATURE_NOT_PRESENT;
         else {
            VkMemoryAllocateInfo allocation{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};allocation.allocationSize=requirements.size;allocation.memoryTypeIndex=type;
            result=DP(d,AllocateMemory)(d->raw,&allocation,c->alloc(),&c->hud_memory);
            if(result==VK_SUCCESS)result=DP(d,BindBufferMemory)(d->raw,c->hud_buffer,c->hud_memory,0);
            if(result==VK_SUCCESS)result=DP(d,MapMemory)(d->raw,c->hud_memory,0,VK_WHOLE_SIZE,0,&c->hud_mapping);
         }
      }
      if(result!=VK_SUCCESS) {
         // The optional overlay must not prevent normal presentation.
         c->hud_disabled=true;fprintf(stderr,"NVK HUD: allocation failed (%d); overlay disabled for this swapchain\n",result);return {};
      }
      fprintf(stderr,"NVK HUD: GPU overlay enabled, %ux%u bitmap; small CPU upload refreshed at 4 Hz\n",bitmap->width,bitmap->height);
   }
   // Both paths complete one copy_frame before starting the next on this
   // device. The worker keeps this mapping unchanged until its fence completes.
   if(c->hud_version!=bitmap->version) {
      memcpy(c->hud_mapping,bitmap->pixels.data(),bitmap->pixels.size()*4);c->hud_version=bitmap->version;
      hud_upload_bytes+=bitmap->pixels.size()*4;
   }
   return bitmap;
}
#endif
VkResult copy_frame(const std::shared_ptr<Chain> &c,uint32_t index,VkSemaphore handoff,bool &consumed) {
   auto d=c->dev;
   auto start=std::chrono::steady_clock::now();
   id<CAMetalDrawable> drawable=[c->surface->layer nextDrawable];
   auto drawable_elapsed=elapsed(start);drawable_ns+=drawable_elapsed;
   if(d->profile_present)present_stats[2]+=drawable_elapsed;
   if (!drawable) return VK_ERROR_OUT_OF_DATE_KHR;
   id<MTLTexture> texture=drawable.texture;
   if (texture.device.registryID!=d->metal.registryID || texture.iosurfacePlane!=0 || !texture.iosurface)
      return VK_ERROR_SURFACE_LOST_KHR;
   // A valid layer can resize before its old swapchain is replaced. Let the
   // application recreate that chain rather than reporting a lost window.
   if (texture.width!=c->extent.width || texture.height!=c->extent.height)
      return VK_ERROR_OUT_OF_DATE_KHR;
   MTLPixelFormat expected=c->format==VK_FORMAT_B8G8R8A8_SRGB?MTLPixelFormatBGRA8Unorm_sRGB:MTLPixelFormatBGRA8Unorm;
   if (texture.pixelFormat!=expected) return VK_ERROR_FORMAT_NOT_SUPPORTED;
   IOSurfaceRef surface=texture.iosurface;
   size_t row=IOSurfaceGetBytesPerRow(surface),size=IOSurfaceGetAllocSize(surface);
   if (IOSurfaceGetPlaneCount(surface)>1 || row%4 || row<(uint64_t)c->extent.width*4 ||
       c->extent.height>SIZE_MAX/row || (uint64_t)row*c->extent.height>size) return VK_ERROR_FORMAT_NOT_SUPPORTED;
   std::unique_lock<std::recursive_mutex> backend_lock(d->submit);
   if(d->uncertain)return VK_ERROR_DEVICE_LOST;
   PresentTimer prepare(d->profile_present,3);
   std::shared_ptr<Frame> frame;
   if(d->reuse_present_resources) {
      for(auto &entry:c->present_cache) {
         if(entry && entry->surface==surface && entry->row==row && entry->allocation_size==size) {
            frame=std::move(entry);c->present_cache_bytes-=frame->allocation_size;
            if(d->profile_present)present_stats[10]++;
            break;
         }
      }
   }
   VkResult r;
   if(frame) {
      // Cached entries are parked only after successful fence completion.
      r=DP(d,ResetCommandPool)(d->raw,frame->pool,0);if(r!=VK_SUCCESS)return r;
      r=DP(d,ResetFences)(d->raw,1,&frame->fence);if(r!=VK_SUCCESS)return r;
      if(frame->host_pointer) {
         if(IOSurfaceLock(surface,0,nullptr)!=kIOReturnSuccess)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
         frame->host_locked=true;
         if(IOSurfaceGetBaseAddress(surface)!=frame->host_pointer)return VK_ERROR_INVALID_EXTERNAL_HANDLE;
      }
   } else {
      frame=std::make_shared<Frame>();frame->dev=d;
      frame->surface=(IOSurfaceRef)CFRetain(surface);frame->row=row;frame->allocation_size=size;
      frame->bytes=(uint64_t)row*c->extent.height;
      VkBufferCreateInfo buffer{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};buffer.size=frame->bytes;buffer.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT|VK_BUFFER_USAGE_TRANSFER_SRC_BIT;buffer.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
      r=DP(d,CreateBuffer)(d->raw,&buffer,nullptr,&frame->buffer);if (r!=VK_SUCCESS) return r;
      VkMemoryRequirements req{};DP(d,GetBufferMemoryRequirements)(d->raw,frame->buffer,&req);
      if (req.size>size) return VK_ERROR_OUT_OF_DEVICE_MEMORY;
      int mt=memory_type(*d,req.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);if (mt<0) return VK_ERROR_OUT_OF_DEVICE_MEMORY;
      VkImportMemoryFdInfoKHR imported{VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR};
      imported.handleType=VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;imported.fd=sandbox_open("/dev/null",O_RDONLY|O_CLOEXEC);
      if (imported.fd<0) return VK_ERROR_OUT_OF_HOST_MEMORY;
      VkMemoryAllocateInfo allocation{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
      allocation.pNext=&imported;allocation.allocationSize=req.size;allocation.memoryTypeIndex=mt;
      Share share{3,IOSurfaceGetID(frame->surface),0,0,req.size};
      stage_import(&share);r=DP(d,AllocateMemory)(d->raw,&allocation,nullptr,&frame->memory);stage_import(nullptr);
      if (r!=VK_SUCCESS) {
         close(imported.fd);import_failures++;
         if(r!=VK_ERROR_INVALID_EXTERNAL_HANDLE || !d->host_present_alignment)return r;
         r=import_host_frame(frame,buffer);if(r!=VK_SUCCESS)return r;
      }
      r=DP(d,BindBufferMemory)(d->raw,frame->buffer,frame->memory,0);if (r!=VK_SUCCESS) return r;
      VkCommandPoolCreateInfo pool{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};pool.queueFamilyIndex=d->copy_family;
      r=DP(d,CreateCommandPool)(d->raw,&pool,nullptr,&frame->pool);if (r!=VK_SUCCESS) return r;
      VkCommandBufferAllocateInfo cb{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};cb.commandPool=frame->pool;cb.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY;cb.commandBufferCount=1;
      r=DP(d,AllocateCommandBuffers)(d->raw,&cb,&frame->commands);if (r!=VK_SUCCESS) return r;
      VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};r=DP(d,CreateFence)(d->raw,&fi,nullptr,&frame->fence);if (r!=VK_SUCCESS) return r;
      if(d->profile_present)present_stats[9]++;
   }
   frame->drawable=drawable;
   prepare.finish();
   PresentTimer record(d->profile_present,4);
   VkCommandBufferBeginInfo begin{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};begin.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
   r=DP(d,BeginCommandBuffer)(frame->commands,&begin);if (r!=VK_SUCCESS) return r;
   VkImageMemoryBarrier barrier{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
   barrier.srcAccessMask=VK_ACCESS_MEMORY_WRITE_BIT;barrier.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
   barrier.oldLayout=VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;barrier.newLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
   barrier.srcQueueFamilyIndex=barrier.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED;
   barrier.image=c->images[index].raw;barrier.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
#ifdef NVKBRIDGE_WINE
   auto hud=prepare_hud(c);
   if(hud) {
      barrier.dstAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;barrier.newLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
      DP(d,CmdPipelineBarrier)(frame->commands,VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,nullptr,0,nullptr,1,&barrier);
      VkBufferImageCopy overlay{};overlay.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1};
      overlay.imageOffset={8,8,0};overlay.imageExtent={hud->width,hud->height,1};
      DP(d,CmdCopyBufferToImage)(frame->commands,c->hud_buffer,barrier.image,VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,1,&overlay);
      barrier.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;barrier.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
      barrier.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;barrier.newLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
   }
#endif
   DP(d,CmdPipelineBarrier)(frame->commands,VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,nullptr,0,nullptr,1,&barrier);
   VkBufferImageCopy region{};region.bufferRowLength=(uint32_t)(row/4);region.bufferImageHeight=c->extent.height;
   region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1};region.imageExtent={c->extent.width,c->extent.height,1};
   DP(d,CmdCopyImageToBuffer)(frame->commands,c->images[index].raw,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,frame->buffer,1,&region);
   if(frame->host_locked) {
      VkMemoryBarrier host{VK_STRUCTURE_TYPE_MEMORY_BARRIER};
      host.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;host.dstAccessMask=VK_ACCESS_HOST_READ_BIT;
      DP(d,CmdPipelineBarrier)(frame->commands,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_HOST_BIT,0,1,&host,0,nullptr,0,nullptr);
   }
   barrier.srcAccessMask=VK_ACCESS_TRANSFER_READ_BIT;barrier.dstAccessMask=0;
   barrier.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;barrier.newLayout=VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
   DP(d,CmdPipelineBarrier)(frame->commands,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,0,0,nullptr,0,nullptr,1,&barrier);
   r=DP(d,EndCommandBuffer)(frame->commands);if (r!=VK_SUCCESS) return r;
   record.finish();
   VkPipelineStageFlags transfer=VK_PIPELINE_STAGE_TRANSFER_BIT;
   VkSubmitInfo submit{VK_STRUCTURE_TYPE_SUBMIT_INFO};submit.waitSemaphoreCount=handoff?1:0;submit.pWaitSemaphores=&handoff;submit.pWaitDstStageMask=&transfer;
   submit.commandBufferCount=1;submit.pCommandBuffers=&frame->commands;
   { PresentTimer submission(d->profile_present,5);
     r=DP(d,QueueSubmit)(d->copy_queue,1,&submit,frame->fence); }
   if (r!=VK_SUCCESS) return r;
   if (handoff) consumed=true;
   // The synchronous caller retains its outer recursive lock. The worker has
   // no outer lock, allowing application submissions while it waits on the GPU.
   backend_lock.unlock();
   start=std::chrono::steady_clock::now();r=DP(d,WaitForFences)(d->raw,1,&frame->fence,VK_TRUE,5000000000ull);
   backend_lock.lock();
   auto wait_elapsed=elapsed(start);producer_ns+=wait_elapsed;
   if(d->profile_present)present_stats[6]+=wait_elapsed;
   if (r!=VK_SUCCESS) {
      d->uncertain=true;
      frame->chain=c;
      { std::lock_guard<std::mutex> lock(registry);quarantine.push_back(frame); }
      fprintf(stderr,"NVK Bridge: presentation completion uncertain (%d); resources quarantined\n",r);
      return VK_ERROR_DEVICE_LOST;
   }
   if (auto observer=present_observer.load()) observer(backend_gdpa,d->raw,d->copy_queue,frame->buffer,(uint32_t)row,c->extent.width,c->extent.height,d->copy_family);
   PresentTimer publication(d->profile_present,7);
   if(frame->host_locked) {
      if(IOSurfaceUnlock(frame->surface,0,nullptr)!=kIOReturnSuccess)return VK_ERROR_SURFACE_LOST_KHR;
      frame->host_locked=false;
   } else {
      if (surface_dirty(d->raw,IOSurfaceGetID(frame->surface),0)) return VK_ERROR_SURFACE_LOST_KHR;
      publish_tag(frame->surface);
   }
   [frame->drawable present];
   publication.finish();
   frame->drawable=nil;
#ifdef NVKBRIDGE_WINE
   if(c->surface->hud)c->surface->hud->record(std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count());
#endif
   copies++;copy_bytes+=frame->bytes;
   if(d->profile_present) { present_stats[0]++;present_stats[13]+=frame->bytes; }
   // Never park the drawable, unfinished work or unbounded VRAM imports.
   // A retained IOSurface pins its identity, matching the driver's idle-import
   // contract. All cache entries are idle under the device submission lock.
   constexpr uint64_t cache_cap=64ull<<20;
   if(d->reuse_present_resources && frame->allocation_size<=cache_cap) {
      std::shared_ptr<Frame> *slot=nullptr;
      for(auto &entry:c->present_cache)if(!entry) { slot=&entry;break; }
      if(!slot)slot=&c->present_cache[c->present_cache_cursor++%c->present_cache.size()];
      if(*slot) {
         c->present_cache_bytes-=(*slot)->allocation_size;slot->reset();
         if(d->profile_present)present_stats[11]++;
      }
      for(auto &entry:c->present_cache) {
         if(c->present_cache_bytes<=cache_cap-frame->allocation_size)break;
         if(entry) { c->present_cache_bytes-=entry->allocation_size;entry.reset();if(d->profile_present)present_stats[11]++; }
      }
      *slot=std::move(frame);c->present_cache_bytes+=(*slot)->allocation_size;
   }
   return VK_SUCCESS;
}

struct PresentTarget {
   std::shared_ptr<Chain> chain;
   uint32_t index{};
   VkResult result{VK_ERROR_OUT_OF_DATE_KHR};
   bool accepted{};
};
struct PresentJob {
   std::shared_ptr<Device> dev;
   std::vector<PresentTarget> targets;
   VkSemaphore handoff{};
};
void run_present_job(const std::shared_ptr<PresentJob> &job) {
   @autoreleasepool {
   auto d=job->dev;
   bool consumed=false;
   for(auto &target:job->targets)if(target.accepted) {
      if(d->uncertain)break;
      target.result=copy_frame(target.chain,target.index,consumed?VK_NULL_HANDLE:job->handoff,consumed);
      if(d->profile_present && target.result!=VK_SUCCESS)present_stats[12]++;
   }
   // Even an out-of-date drawable must consume and complete the caller's
   // rendering dependencies before its source image or semaphore is released.
   if(!consumed && !d->uncertain) {
      VkFence fence{};VkFenceCreateInfo info{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};VkResult r;
      { std::lock_guard<std::recursive_mutex> lock(d->submit);
        r=DP(d,CreateFence)(d->raw,&info,nullptr,&fence);
        if(r==VK_SUCCESS) {
           VkPipelineStageFlags all=VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
           VkSubmitInfo drain{VK_STRUCTURE_TYPE_SUBMIT_INFO};drain.waitSemaphoreCount=1;
           drain.pWaitSemaphores=&job->handoff;drain.pWaitDstStageMask=&all;
           r=DP(d,QueueSubmit)(d->copy_queue,1,&drain,fence);
        }
      }
      if(r==VK_SUCCESS)r=DP(d,WaitForFences)(d->raw,1,&fence,VK_TRUE,5000000000ull);
      if(r!=VK_SUCCESS)d->uncertain=true;
      else { std::lock_guard<std::recursive_mutex> lock(d->submit);DP(d,DestroyFence)(d->raw,fence,nullptr); }
   }
   if(d->uncertain)fail_presenter(d.get());
   else { std::lock_guard<std::recursive_mutex> lock(d->submit);DP(d,DestroySemaphore)(d->raw,job->handoff,nullptr);job->handoff={}; }
   for(auto &target:job->targets)if(target.accepted) {
      auto c=target.chain;
      std::lock_guard<std::mutex> lock(c->mutex);
      if(!d->uncertain) {
         c->images[target.index].acquired=false;c->images[target.index].present_pending=false;
         if(target.result!=VK_SUCCESS)c->present_error=target.result;
      }
      c->available.notify_all();
   }
   }
}

// Called under present_call, without submit. All caller arrays are copied and
// pResults is written before returning; the worker never retains those pointers.
VkResult enqueue_present(const std::shared_ptr<Device> &d,VkQueue queue,const VkPresentInfoKHR *info) {
   std::shared_ptr<PresentJob> job;
   nvkbridge::PresentWorker::Task task;
   std::vector<VkPipelineStageFlags> stages;
   try {
      job=std::make_shared<PresentJob>();job->dev=d;job->targets.resize(info->swapchainCount);
      stages.assign(info->waitSemaphoreCount,VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
      task=[job]{run_present_job(job);};
   } catch(...) { return VK_ERROR_OUT_OF_HOST_MEMORY; }
   VkResult overall=VK_SUCCESS;
   for(uint32_t j=0;j<info->swapchainCount;++j) {
      auto &target=job->targets[j];target.chain=find(chains,info->pSwapchains[j]);target.index=info->pImageIndices[j];
      auto c=target.chain;
      if(c && c->dev==d) {
         std::lock_guard<std::mutex> lock(c->mutex);
         if(c->present_error!=VK_SUCCESS)target.result=c->present_error;
         else if(!c->retired && target.index<c->images.size() && c->images[target.index].acquired && !c->images[target.index].present_pending) {
            target.result=VK_SUCCESS;target.accepted=true;c->images[target.index].present_pending=true;
         }
      }
      if(info->pResults)info->pResults[j]=target.result;
      if(target.result!=VK_SUCCESS)overall=target.result;
   }
   VkResult r;
   { std::lock_guard<std::recursive_mutex> lock(d->submit);
     VkSemaphoreCreateInfo sci{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
     r=d->uncertain?VK_ERROR_DEVICE_LOST:DP(d,CreateSemaphore)(d->raw,&sci,nullptr,&job->handoff);
     if(r==VK_SUCCESS) {
        VkSubmitInfo submit{VK_STRUCTURE_TYPE_SUBMIT_INFO};submit.waitSemaphoreCount=info->waitSemaphoreCount;
        submit.pWaitSemaphores=info->pWaitSemaphores;submit.pWaitDstStageMask=stages.data();
        submit.signalSemaphoreCount=1;submit.pSignalSemaphores=&job->handoff;
        r=DP(d,QueueSubmit)(queue,1,&submit,VK_NULL_HANDLE);
        if(r!=VK_SUCCESS)DP(d,DestroySemaphore)(d->raw,job->handoff,nullptr);
     }
   }
   if(r!=VK_SUCCESS) {
      for(auto &target:job->targets)if(target.accepted) {
         std::lock_guard<std::mutex> lock(target.chain->mutex);target.chain->images[target.index].present_pending=false;
      }
      if(info->pResults)std::fill_n(info->pResults,info->swapchainCount,r);
      return r;
   }
   // Capacity waits never retain submit: the worker may need it to finish.
   // present_call prevents stop from racing this post, so it cannot be refused.
   if(!d->presenter->post(std::move(task))) { fail_presenter(d.get());return VK_ERROR_DEVICE_LOST; }
   return d->uncertain?VK_ERROR_DEVICE_LOST:overall;
}

VkResult VKAPI_CALL queue_present(VkQueue queue,const VkPresentInfoKHR *info) {
   auto q=find(queues,queue);auto d=q?q->dev.lock():nullptr;if (!d || !d->swapchain || d->uncertain) return VK_ERROR_DEVICE_LOST;
   PresentTimer total(d->profile_present,1),lock_wait(d->profile_present,14);
   std::lock_guard<std::mutex> gate(d->present_call);
   if(d->uncertain || d->closing)return VK_ERROR_DEVICE_LOST;
   if(d->presenter) { lock_wait.finish();return enqueue_present(d,queue,info); }
   std::lock_guard<std::recursive_mutex> lock(d->submit);
   lock_wait.finish();
   VkSemaphore handoff{};VkSemaphoreCreateInfo sci{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
   VkResult r=DP(d,CreateSemaphore)(d->raw,&sci,nullptr,&handoff);if (r!=VK_SUCCESS) return r;
   std::vector<VkPipelineStageFlags> stages(info->waitSemaphoreCount,VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
   VkSubmitInfo submit{VK_STRUCTURE_TYPE_SUBMIT_INFO};submit.waitSemaphoreCount=info->waitSemaphoreCount;submit.pWaitSemaphores=info->pWaitSemaphores;
   submit.pWaitDstStageMask=stages.data();submit.signalSemaphoreCount=1;submit.pSignalSemaphores=&handoff;
   r=DP(d,QueueSubmit)(queue,1,&submit,VK_NULL_HANDLE);if (r!=VK_SUCCESS) { DP(d,DestroySemaphore)(d->raw,handoff,nullptr);return r; }
   VkResult overall=VK_SUCCESS;bool consumed=false;
   for (uint32_t j=0;j<info->swapchainCount;j++) {
      auto c=find(chains,info->pSwapchains[j]);VkResult result=VK_ERROR_OUT_OF_DATE_KHR;uint32_t k=info->pImageIndices[j];
      if (c && c->dev==d && k<c->images.size()) {
         bool valid;
         { std::lock_guard<std::mutex> guard(c->mutex);valid=c->images[k].acquired && !c->retired; }
         if (valid) {
            result=copy_frame(c,k,consumed?VK_NULL_HANDLE:handoff,consumed);
         }
         if (!d->uncertain) { std::lock_guard<std::mutex> guard(c->mutex);c->images[k].acquired=false;c->available.notify_all(); }
      }
      if (info->pResults) info->pResults[j]=result;
      if(d->profile_present && result!=VK_SUCCESS)present_stats[12]++;
      if (result!=VK_SUCCESS) overall=result;
      if (d->uncertain) return VK_ERROR_DEVICE_LOST;
   }
   if (!consumed) {
      VkPipelineStageFlags all=VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;VkSubmitInfo drain{VK_STRUCTURE_TYPE_SUBMIT_INFO};
      drain.waitSemaphoreCount=1;drain.pWaitSemaphores=&handoff;drain.pWaitDstStageMask=&all;
      r=DP(d,QueueSubmit)(d->copy_queue,1,&drain,VK_NULL_HANDLE);
      if (r==VK_SUCCESS) r=DP(d,QueueWaitIdle)(d->copy_queue);
      if (r!=VK_SUCCESS) { d->uncertain=true;return VK_ERROR_DEVICE_LOST; }
   }
   DP(d,DestroySemaphore)(d->raw,handoff,nullptr);
   return overall;
}
VkResult VKAPI_CALL group_caps(VkDevice device,VkDeviceGroupPresentCapabilitiesKHR *out) {
   if (!find(devices,device)) return VK_ERROR_DEVICE_LOST;
   memset(out->presentMask,0,sizeof out->presentMask);out->presentMask[0]=1;out->modes=VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;return VK_SUCCESS;
}
VkResult VKAPI_CALL group_modes(VkDevice device,VkSurfaceKHR surface,VkDeviceGroupPresentModeFlagsKHR *out) {
   if (!find(devices,device) || !find(surfaces,surface)) return VK_ERROR_SURFACE_LOST_KHR;
   *out=VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;return VK_SUCCESS;
}
VkResult VKAPI_CALL present_rects(VkPhysicalDevice physical,VkSurfaceKHR surface,uint32_t *count,VkRect2D *out) {
   VkSurfaceCapabilitiesKHR caps{};VkResult r=surface_caps(physical,surface,&caps);if (r!=VK_SUCCESS) return r;
   return enumerate(std::vector<VkRect2D>{{{0,0},caps.currentExtent}},count,out);
}

template<auto Function,class Signature=decltype(Function)> struct ApiGuard;
template<auto Function,class R,class... Args> struct ApiGuard<Function,R(*)(Args...)> {
   static R call(Args... args) noexcept {
      try { return Function(args...); }
      catch (const std::bad_alloc &) {
         if constexpr (std::is_same_v<R,VkResult>) return VK_ERROR_OUT_OF_HOST_MEMORY;
         else if constexpr (!std::is_void_v<R>) return R{};
      } catch (...) {
         if constexpr (std::is_same_v<R,VkResult>) return VK_ERROR_UNKNOWN;
         else if constexpr (!std::is_void_v<R>) return R{};
      }
   }
};
PFN_vkVoidFunction lookup(const char *name) {
   if (!strcmp(name,"vkGetInstanceProcAddr")) return (PFN_vkVoidFunction)&vkGetInstanceProcAddr;
   if (!strcmp(name,"vkGetDeviceProcAddr")) return (PFN_vkVoidFunction)&vkGetDeviceProcAddr;
   if (!strcmp(name,"vkCreateMetalSurfaceEXT")) return (PFN_vkVoidFunction)&vkCreateMetalSurfaceEXT;
#define MAP(vkname, fn) if (!strcmp(name,"vk" #vkname)) return reinterpret_cast<PFN_vkVoidFunction>(&ApiGuard<fn>::call)
   MAP(CreateInstance,create_instance); MAP(DestroyInstance,destroy_instance); MAP(EnumerateInstanceExtensionProperties,instance_extensions);
   MAP(EnumerateInstanceLayerProperties,instance_layers); MAP(EnumerateInstanceVersion,instance_version);
   MAP(EnumeratePhysicalDevices,enumerate_physical); MAP(EnumerateDeviceExtensionProperties,device_extensions);
   MAP(CreateDevice,create_device); MAP(DestroyDevice,destroy_device); MAP(GetDeviceQueue,get_queue); MAP(GetDeviceQueue2,get_queue2);
   MAP(QueueSubmit,queue_submit); MAP(QueueSubmit2,queue_submit2); MAP(QueueSubmit2KHR,queue_submit2);
   MAP(QueueBindSparse,bind_sparse); MAP(QueueWaitIdle,queue_idle); MAP(DeviceWaitIdle,device_idle);
   MAP(DestroySurfaceKHR,destroy_surface);
#ifdef NVKBRIDGE_WINE
   MAP(GetPhysicalDeviceExternalBufferProperties,external_buffer);MAP(GetPhysicalDeviceExternalBufferPropertiesKHR,external_buffer);
   MAP(GetPhysicalDeviceExternalSemaphoreProperties,external_semaphore);MAP(GetPhysicalDeviceExternalSemaphorePropertiesKHR,external_semaphore);
   MAP(GetPhysicalDeviceExternalFenceProperties,external_fence);MAP(GetPhysicalDeviceExternalFencePropertiesKHR,external_fence);
   MAP(GetPhysicalDeviceImageFormatProperties2,image_format2);MAP(GetPhysicalDeviceImageFormatProperties2KHR,image_format2);
#endif
   MAP(GetPhysicalDeviceSurfaceSupportKHR,surface_support); MAP(GetPhysicalDeviceSurfaceCapabilitiesKHR,surface_caps);
   MAP(GetPhysicalDeviceSurfaceCapabilities2KHR,surface_caps2); MAP(GetPhysicalDeviceSurfaceFormatsKHR,surface_formats);
   MAP(GetPhysicalDeviceSurfaceFormats2KHR,surface_formats2); MAP(GetPhysicalDeviceSurfacePresentModesKHR,present_modes);
   MAP(CreateSwapchainKHR,create_swapchain); MAP(DestroySwapchainKHR,destroy_swapchain); MAP(GetSwapchainImagesKHR,swapchain_images);
   MAP(AcquireNextImageKHR,acquire_image); MAP(AcquireNextImage2KHR,acquire_image2); MAP(QueuePresentKHR,queue_present);
   MAP(GetDeviceGroupPresentCapabilitiesKHR,group_caps); MAP(GetDeviceGroupSurfacePresentModesKHR,group_modes);
   MAP(GetPhysicalDevicePresentRectanglesKHR,present_rects);
#undef MAP
   return nullptr;
}
}

extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance instance,const char *name) {
   if (!name || !initialize()) return nullptr;
#ifdef NVKBRIDGE_WINE
   if (unsupported_proc(name)) return nullptr;
#endif
   if (!instance && strcmp(name,"vkGetInstanceProcAddr") && strcmp(name,"vkCreateInstance") &&
       strcmp(name,"vkEnumerateInstanceExtensionProperties") && strcmp(name,"vkEnumerateInstanceLayerProperties") &&
       strcmp(name,"vkEnumerateInstanceVersion")) return nullptr;
   if (auto fn=lookup(name)) return fn;
   if (!strcmp(name,"vkCreateHeadlessSurfaceEXT")) return nullptr;
   return backend_gipa(instance,name);
}
extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice device,const char *name) {
   auto d=find(devices,device);
   if (!name || !d) return nullptr;
#ifdef NVKBRIDGE_WINE
   if (unsupported_proc(name)) return nullptr;
#endif
   if (strstr(name,"vkGetPhysicalDevice")==name || strstr(name,"vkEnumerate")==name ||
       !strcmp(name,"vkGetInstanceProcAddr") || !strcmp(name,"vkCreateInstance") || !strcmp(name,"vkDestroyInstance") ||
       !strcmp(name,"vkCreateDevice") || !strcmp(name,"vkCreateMetalSurfaceEXT") || !strcmp(name,"vkDestroySurfaceKHR")) return nullptr;
   if ((strstr(name,"Swapchain") || !strcmp(name,"vkQueuePresentKHR") || !strcmp(name,"vkAcquireNextImageKHR") ||
        !strcmp(name,"vkAcquireNextImage2KHR") || !strcmp(name,"vkGetDeviceGroupPresentCapabilitiesKHR") ||
        !strcmp(name,"vkGetDeviceGroupSurfacePresentModesKHR")) && !d->swapchain) return nullptr;
   if (auto fn=lookup(name)) return fn;
   return backend_gdpa(device,name);
}
// Wine probes this direct symbol before choosing its EXT Metal surface route.
// Share the exact guarded implementation returned through gipa.
extern "C" VKAPI_ATTR VkResult VKAPI_CALL vkCreateMetalSurfaceEXT(
   VkInstance instance,const VkMetalSurfaceCreateInfoEXT *info,const VkAllocationCallbacks *alloc,VkSurfaceKHR *out) {
   return ApiGuard<create_surface>::call(instance,info,alloc,out);
}
extern "C" void nvkbridgeGetStats(uint64_t out[7]) {
   out[0]=copies;out[1]=copy_bytes;out[2]=drawable_ns;out[3]=producer_ns;out[4]=import_failures;out[5]=0;out[6]=0;
}
// frames, total-present ns, drawable ns, resource-prepare ns, recording ns,
// copy-submit ns, render+copy fence ns, publication ns, resource-cleanup ns,
// resources created, cache hits, evictions, errors, copied bytes, lock-wait ns,
// reserved. Profiling is opt-in; the existing seven-word stats ABI is unchanged.
extern "C" void nvkbridgeGetPresentStats(uint64_t out[16]) {
   for(unsigned i=0;i<16;i++)out[i]=present_stats[i].load(std::memory_order_relaxed);
}
extern "C" void nvkbridgeSetPresentObserver(PresentObserver observer) { present_observer=observer; }
// Loader/ICD interface. The frontend already owns portable surfaces and
// swapchains, so it must negotiate at least version 3: older interface versions
// make the loader keep its own opaque surface wrapper and pass that to the ICD.
extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vk_icdGetInstanceProcAddr(
   VkInstance instance,const char *name) {
   return vkGetInstanceProcAddr(instance,name);
}
extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vk_icdGetPhysicalDeviceProcAddr(
   VkInstance instance,const char *name) {
   if (!instance || !name) return nullptr;
   if (strncmp(name,"vkGetPhysicalDevice",19) &&
       strcmp(name,"vkEnumerateDeviceExtensionProperties") &&
       strcmp(name,"vkEnumerateDeviceLayerProperties")) return nullptr;
   return vkGetInstanceProcAddr(instance,name);
}
extern "C" VKAPI_ATTR VkResult VKAPI_CALL vk_icdNegotiateLoaderICDInterfaceVersion(
   uint32_t *version) {
   if (!version) return VK_ERROR_INITIALIZATION_FAILED;
   if (*version > 5) *version=5;
   if (*version < 3) return VK_ERROR_INCOMPATIBLE_DRIVER;
   return VK_SUCCESS;
}
