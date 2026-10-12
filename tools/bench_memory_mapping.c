#define VK_NO_PROTOTYPES
#include <vulkan/vulkan_core.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#define CHECK(x) do{VkResult r=(x);if(r!=VK_SUCCESS){fprintf(stderr,"%s failed: %d\n",#x,r);return 3;}}while(0)
#define INSTANCE(n) PFN_##n n=(PFN_##n)gipa(instance,#n);if(!n)return 4
#define DEVICE(n) PFN_##n n=(PFN_##n)gdpa(device,#n);if(!n)return 4
int main(int argc,char **argv){
 if(argc!=2)return 2;void *lib=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL);if(!lib){fprintf(stderr,"%s\n",dlerror());return 2;}
 PFN_vkGetInstanceProcAddr gipa=dlsym(lib,"vkGetInstanceProcAddr");if(!gipa)gipa=dlsym(lib,"vk_icdGetInstanceProcAddr");if(!gipa)return 2;
 PFN_vkCreateInstance create=(PFN_vkCreateInstance)gipa(NULL,"vkCreateInstance");
 VkApplicationInfo ai={.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO,.pApplicationName="Mapping regression probe",.apiVersion=VK_API_VERSION_1_1};
 VkInstanceCreateInfo ici={.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,.pApplicationInfo=&ai};VkInstance instance;
 CHECK(create(&ici,NULL,&instance));
 INSTANCE(vkEnumeratePhysicalDevices);INSTANCE(vkGetPhysicalDeviceQueueFamilyProperties);INSTANCE(vkGetPhysicalDeviceMemoryProperties);INSTANCE(vkGetPhysicalDeviceProperties);INSTANCE(vkCreateDevice);INSTANCE(vkGetDeviceProcAddr);INSTANCE(vkDestroyInstance);
 uint32_t count=1;VkPhysicalDevice pd;CHECK(vkEnumeratePhysicalDevices(instance,&count,&pd));if(!count)return 5;
 VkPhysicalDeviceProperties props;vkGetPhysicalDeviceProperties(pd,&props);printf("device=%s\n",props.deviceName);
 VkPhysicalDeviceMemoryProperties mp;vkGetPhysicalDeviceMemoryProperties(pd,&mp);
 uint32_t nq=32;VkQueueFamilyProperties qp[32];vkGetPhysicalDeviceQueueFamilyProperties(pd,&nq,qp);uint32_t qi=0;while(qi<nq&&!qp[qi].queueCount)qi++;if(qi==nq)return 5;
 float priority=1;VkDeviceQueueCreateInfo qci={.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,.queueFamilyIndex=qi,.queueCount=1,.pQueuePriorities=&priority};
 VkDeviceCreateInfo dci={.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,.queueCreateInfoCount=1,.pQueueCreateInfos=&qci};VkDevice device;CHECK(vkCreateDevice(pd,&dci,NULL,&device));PFN_vkGetDeviceProcAddr gdpa=vkGetDeviceProcAddr;
 DEVICE(vkAllocateMemory);DEVICE(vkMapMemory);DEVICE(vkUnmapMemory);DEVICE(vkFreeMemory);DEVICE(vkDestroyDevice);
 unsigned total=0;
 for(uint32_t t=0;t<mp.memoryTypeCount;t++)if(mp.memoryTypes[t].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT){
  for(unsigned pass=0;pass<100;pass++){
   VkDeviceMemory mem[8];void *ptr[8];
   for(unsigned i=0;i<8;i++){VkMemoryAllocateInfo mai={.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,.allocationSize=65536,.memoryTypeIndex=t};CHECK(vkAllocateMemory(device,&mai,NULL,&mem[i]));CHECK(vkMapMemory(device,mem[i],0,65536,0,&ptr[i]));memset(ptr[i],(pass+i+1)&255,65536);}
   for(unsigned i=8;i-->0;){unsigned char want=(pass+i+1)&255,*p=ptr[i];for(unsigned j=0;j<65536;j++)if(p[j]!=want){fprintf(stderr,"mapping corruption type=%u pass=%u slot=%u byte=%u\n",t,pass,i,j);return 6;}vkUnmapMemory(device,mem[i]);vkFreeMemory(device,mem[i],NULL);total++;}
  }printf("type=%u flags=%#x mappings=800 PASS\n",t,mp.memoryTypes[t].propertyFlags);fflush(stdout);
 }
 vkDestroyDevice(device,NULL);vkDestroyInstance(instance,NULL);printf("RESULT mappings=%u PASS gpu_submissions=0\n",total);return 0;
}
