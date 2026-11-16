// Exercise production surface creation and binding with simulated Metal/IOKit
// devices. No GPU, display settings or installed libraries are changed.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <IOKit/IOKitLib.h>
#include <vector>
#include <cassert>

static bool enumeration_fails{},invalid_iterator{},identifier_fails{};
static std::vector<io_service_t> accelerators{17,19};
static size_t cursor;
static kern_return_t simulated_services(mach_port_t,CFDictionaryRef matching,io_iterator_t *out) {
   CFRelease(matching);cursor=0;*out=100;return enumeration_fails?kIOReturnError:KERN_SUCCESS;
}
static io_object_t simulated_next(io_iterator_t) {
   return cursor<accelerators.size()?accelerators[cursor++]:IO_OBJECT_NULL;
}
[[maybe_unused]] static boolean_t simulated_valid(io_iterator_t) { return !invalid_iterator; }
static kern_return_t simulated_release(io_object_t) { return KERN_SUCCESS; }
static kern_return_t simulated_identifier(io_registry_entry_t service,uint64_t *out) {
   *out=service;return identifier_fails?kIOReturnError:KERN_SUCCESS;
}
static kern_return_t simulated_parent(io_registry_entry_t service,const io_name_t,io_registry_entry_t *out) {
   *out=service+1000;return KERN_SUCCESS;
}
static CFTypeRef simulated_property(io_registry_entry_t service,CFStringRef,CFAllocatorRef,IOOptionBits) {
   uint32_t id=service==1017?0x1f06:0x1f07;
   return CFDataCreate(kCFAllocatorDefault,(const UInt8 *)&id,sizeof(id));
}
static NSArray<id<MTLDevice>> *simulated_metal_devices;
static NSArray<id<MTLDevice>> *simulated_devices() { return simulated_metal_devices; }
#define IOServiceGetMatchingServices simulated_services
#define IOIteratorNext simulated_next
#define IOIteratorIsValid simulated_valid
#define IOObjectRelease simulated_release
#define IORegistryEntryGetRegistryEntryID simulated_identifier
#define IORegistryEntryGetParentEntry simulated_parent
#define IORegistryEntryCreateCFProperty simulated_property
#define MTLCopyAllDevices simulated_devices
#include "bridge.mm"

@interface SurfaceTestDevice : NSObject
@property uint64_t registryID;
@end
@implementation SurfaceTestDevice
@end
@interface SurfaceTestLayer : NSObject
@property(strong) id device;
@property CGRect bounds;
@property CGFloat contentsScale;
@property MTLPixelFormat pixelFormat;
@property BOOL framebufferOnly,allowsNextDrawableTimeout,displaySyncEnabled,opaque;
@property NSUInteger maximumDrawableCount;
@property CGSize drawableSize;
@end
@implementation SurfaceTestLayer
- (BOOL)isKindOfClass:(Class)cls { return cls==CAMetalLayer.class || [super isKindOfClass:cls]; }
@end

static unsigned image_attempts;
static void VKAPI_CALL properties(VkPhysicalDevice,VkPhysicalDeviceProperties *out) {
   *out={};out->vendorID=0x10de;out->deviceID=0x1f06;out->limits.maxImageDimension2D=4096;
}
static void VKAPI_CALL families(VkPhysicalDevice,uint32_t *count,VkQueueFamilyProperties *out) {
   if(out && *count) { *out={};out->queueCount=1;out->queueFlags=VK_QUEUE_GRAPHICS_BIT|VK_QUEUE_COMPUTE_BIT|VK_QUEUE_TRANSFER_BIT; }
   *count=1;
}
static VkResult VKAPI_CALL image(VkDevice,const VkImageCreateInfo *,const VkAllocationCallbacks *,VkImage *) {
   image_attempts++;return VK_ERROR_OUT_OF_HOST_MEMORY;
}
static PFN_vkVoidFunction VKAPI_CALL instance_proc(VkInstance,const char *name) {
   if(!strcmp(name,"vkGetPhysicalDeviceProperties"))return (PFN_vkVoidFunction)properties;
   if(!strcmp(name,"vkGetPhysicalDeviceQueueFamilyProperties"))return (PFN_vkVoidFunction)families;
   return nullptr;
}
static PFN_vkVoidFunction VKAPI_CALL device_proc(VkDevice,const char *name) {
   return !strcmp(name,"vkCreateImage")?(PFN_vkVoidFunction)image:nullptr;
}
int main() { @autoreleasepool {
   alarm(20);
   auto selected=[SurfaceTestDevice new];selected.registryID=17;
   auto other_nvidia=[SurfaceTestDevice new];other_nvidia.registryID=19;
   auto intel=[SurfaceTestDevice new];intel.registryID=23;
   auto unknown=[SurfaceTestDevice new];unknown.registryID=0;
   simulated_metal_devices=(NSArray<id<MTLDevice>> *)@[selected,other_nvidia,intel];
   backend_gipa=instance_proc;backend_gdpa=device_proc;
   auto instance=std::make_shared<Instance>();instance->raw=(VkInstance)1;instances[instance->raw]=instance;
   auto dev=std::make_shared<Device>();dev->raw=(VkDevice)2;dev->instance=instance;dev->physical=(VkPhysicalDevice)3;
   dev->swapchain=true;dev->properties.limits.maxImageDimension2D=4096;dev->metal=(id<MTLDevice>)selected;
   devices[dev->raw]=dev;physicals[dev->physical]=instance;
   auto layer=[SurfaceTestLayer new];layer.bounds=CGRectMake(0,0,64,64);layer.contentsScale=1;
   VkMetalSurfaceCreateInfoEXT si{VK_STRUCTURE_TYPE_METAL_SURFACE_CREATE_INFO_EXT};si.pLayer=(CAMetalLayer *)layer;
   auto create=[&] { VkSurfaceKHR s{};assert(create_surface(instance->raw,&si,nullptr,&s)==VK_SUCCESS);return s; };
   auto supported=[&](VkSurfaceKHR s) { VkBool32 yes=99;assert(surface_support(dev->physical,0,s,&yes)==VK_SUCCESS);return yes; };
   VkSwapchainCreateInfoKHR ci{VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR};
   ci.minImageCount=3;ci.imageFormat=VK_FORMAT_B8G8R8A8_UNORM;ci.imageColorSpace=VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
   ci.imageExtent={64,64};ci.imageArrayLayers=1;ci.imageUsage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
   ci.preTransform=VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR;ci.compositeAlpha=VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR;
   ci.presentMode=VK_PRESENT_MODE_FIFO_KHR;
   auto chain=[&](VkSurfaceKHR s) { ci.surface=s;VkSwapchainKHR out=(VkSwapchainKHR)7;
      auto result=create_swapchain(dev->raw,&ci,nullptr,&out);assert(out==(VkSwapchainKHR)7);return result; };
   layer.device=intel;auto s=create();
#ifdef NVKBRIDGE_WINE
   assert(!layer.device && supported(s));
   assert(chain(s)==VK_ERROR_OUT_OF_HOST_MEMORY && image_attempts==1 && layer.device==selected);
   puts("PASS Wine initial Intel binding defers to the exact NVIDIA adapter through surface support and swapchain creation");
#else
   assert(layer.device==intel && !supported(s));
   assert(chain(s)==VK_ERROR_SURFACE_LOST_KHR && image_attempts==0);
   puts("PASS native frontend retains its existing layer binding behavior");
#endif
   layer.device=intel;unsigned before=image_attempts;
   assert(!supported(s) && chain(s)==VK_ERROR_SURFACE_LOST_KHR && image_attempts==before);
   auto alias=create();assert(layer.device==intel && !supported(alias));
   destroy_surface(instance->raw,alias,nullptr);destroy_surface(instance->raw,s,nullptr);
   puts("PASS later device changes and aliased layers cannot be silently rebound");
   for(id initial in @[selected,other_nvidia,unknown]) {
      layer.device=initial;s=create();assert(layer.device==initial);
      if(initial==selected) { assert(supported(s));assert(chain(s)==VK_ERROR_OUT_OF_HOST_MEMORY); }
      else { assert(!supported(s));assert(chain(s)==VK_ERROR_SURFACE_LOST_KHR); }
      destroy_surface(instance->raw,s,nullptr);
   }
   puts("PASS matching NVIDIA, different NVIDIA and unknown registry IDs retain their checks");
   layer.device=nil;s=create();assert(!layer.device && supported(s));
   assert(chain(s)==VK_ERROR_OUT_OF_HOST_MEMORY && layer.device==selected);destroy_surface(instance->raw,s,nullptr);
   for(unsigned failure=0;failure<4;failure++) {
      enumeration_fails=failure==0;identifier_fails=failure==1;invalid_iterator=failure==2;
      if(failure==3)accelerators.clear();
      layer.device=intel;s=create();assert(layer.device==intel && !supported(s));
      assert(chain(s)==VK_ERROR_SURFACE_LOST_KHR);destroy_surface(instance->raw,s,nullptr);
   }
   puts("PASS deferred layers bind normally; enumeration failures leave preassigned devices unchanged");
   simulated_metal_devices=nil;devices.clear();physicals.clear();instances.clear();
} }
