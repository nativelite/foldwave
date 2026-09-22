//! The raw Vulkan surface this backend uses: C types, the few constants and
//! structures it needs, the entry-point tables, and the loader.
//!
//! Every layout, value and prototype here is transcribed from
//! `vulkan_core.h` / `vulkan_win32.h` (KhronosGroup/Vulkan-Headers, header
//! version 363), which are also what the registry man pages
//! (<https://registry.khronos.org/vulkan/specs/latest/man/html/>) show.
//! There are no validation layers on the development machine, so a wrong
//! field here shows up only as a wrong pixel or a crash: keep the
//! transcription literal and field-for-field.
//!
//! `vulkan-1.dll` (the Khronos loader, installed with every Vulkan driver) is
//! loaded at run time from System32 with `LoadLibraryExW`; only
//! `vkGetInstanceProcAddr` is looked up with `GetProcAddress`, and every other
//! entry point comes from it or from `vkGetDeviceProcAddr`, as the loader
//! documentation recommends. Nothing Vulkan is linked.
//!
//! `VKAPI_CALL` is `__stdcall` on Windows, which is `extern "system"` (and the
//! plain C ABI on x64).

#![allow(
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::upper_case_acronyms
)]

use std::ffi::c_void;

// A non-dispatchable handle is a pointer on 64-bit targets and a `uint64_t`
// elsewhere (`VK_DEFINE_NON_DISPATCHABLE_HANDLE`); both are 8 bytes here.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("the Vulkan backend supports 64-bit targets only");

// Dispatchable handles: pointers to opaque driver objects.
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;
pub type VkDevice = *mut c_void;
pub type VkQueue = *mut c_void;
pub type VkCommandBuffer = *mut c_void;

// Non-dispatchable handles.
pub type VkSurfaceKHR = u64;
pub type VkSwapchainKHR = u64;
pub type VkDeviceMemory = u64;
pub type VkBuffer = u64;
pub type VkImage = u64;
pub type VkImageView = u64;
pub type VkShaderModule = u64;
pub type VkPipelineLayout = u64;
pub type VkPipeline = u64;
pub type VkPipelineCache = u64;
pub type VkRenderPass = u64;
pub type VkFramebuffer = u64;
pub type VkDescriptorSetLayout = u64;
pub type VkDescriptorPool = u64;
pub type VkDescriptorSet = u64;
pub type VkSampler = u64;
pub type VkCommandPool = u64;
pub type VkFence = u64;
pub type VkSemaphore = u64;

pub type VkResult = i32;
pub type VkBool32 = u32;
pub type VkFlags = u32;
pub type VkDeviceSize = u64;
/// Every C enum used here is `int`-sized.
pub type VkEnum = u32;

pub const NULL_HANDLE: u64 = 0;

// VkResult.
pub const VK_SUCCESS: VkResult = 0;
pub const VK_NOT_READY: VkResult = 1;
pub const VK_TIMEOUT: VkResult = 2;
pub const VK_INCOMPLETE: VkResult = 5;
pub const VK_SUBOPTIMAL_KHR: VkResult = 1_000_001_003;
pub const VK_ERROR_OUT_OF_HOST_MEMORY: VkResult = -1;
pub const VK_ERROR_OUT_OF_DEVICE_MEMORY: VkResult = -2;
pub const VK_ERROR_INITIALIZATION_FAILED: VkResult = -3;
pub const VK_ERROR_DEVICE_LOST: VkResult = -4;
pub const VK_ERROR_MEMORY_MAP_FAILED: VkResult = -5;
pub const VK_ERROR_LAYER_NOT_PRESENT: VkResult = -6;
pub const VK_ERROR_EXTENSION_NOT_PRESENT: VkResult = -7;
pub const VK_ERROR_FEATURE_NOT_PRESENT: VkResult = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
pub const VK_ERROR_TOO_MANY_OBJECTS: VkResult = -10;
pub const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;
pub const VK_ERROR_UNKNOWN: VkResult = -13;
pub const VK_ERROR_SURFACE_LOST_KHR: VkResult = -1_000_000_000;
pub const VK_ERROR_NATIVE_WINDOW_IN_USE_KHR: VkResult = -1_000_000_001;
pub const VK_ERROR_OUT_OF_DATE_KHR: VkResult = -1_000_001_004;
pub const VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT: VkResult = -1_000_255_000;

/// The spec's name for a result code, for error messages.
pub fn result_name(r: VkResult) -> &'static str {
    match r {
        VK_SUCCESS => "VK_SUCCESS",
        VK_NOT_READY => "VK_NOT_READY",
        VK_TIMEOUT => "VK_TIMEOUT",
        VK_INCOMPLETE => "VK_INCOMPLETE",
        VK_SUBOPTIMAL_KHR => "VK_SUBOPTIMAL_KHR",
        VK_ERROR_OUT_OF_HOST_MEMORY => "VK_ERROR_OUT_OF_HOST_MEMORY",
        VK_ERROR_OUT_OF_DEVICE_MEMORY => "VK_ERROR_OUT_OF_DEVICE_MEMORY",
        VK_ERROR_INITIALIZATION_FAILED => "VK_ERROR_INITIALIZATION_FAILED",
        VK_ERROR_DEVICE_LOST => "VK_ERROR_DEVICE_LOST",
        VK_ERROR_MEMORY_MAP_FAILED => "VK_ERROR_MEMORY_MAP_FAILED",
        VK_ERROR_LAYER_NOT_PRESENT => "VK_ERROR_LAYER_NOT_PRESENT",
        VK_ERROR_EXTENSION_NOT_PRESENT => "VK_ERROR_EXTENSION_NOT_PRESENT",
        VK_ERROR_FEATURE_NOT_PRESENT => "VK_ERROR_FEATURE_NOT_PRESENT",
        VK_ERROR_INCOMPATIBLE_DRIVER => "VK_ERROR_INCOMPATIBLE_DRIVER",
        VK_ERROR_TOO_MANY_OBJECTS => "VK_ERROR_TOO_MANY_OBJECTS",
        VK_ERROR_FORMAT_NOT_SUPPORTED => "VK_ERROR_FORMAT_NOT_SUPPORTED",
        VK_ERROR_UNKNOWN => "VK_ERROR_UNKNOWN",
        VK_ERROR_SURFACE_LOST_KHR => "VK_ERROR_SURFACE_LOST_KHR",
        VK_ERROR_NATIVE_WINDOW_IN_USE_KHR => "VK_ERROR_NATIVE_WINDOW_IN_USE_KHR",
        VK_ERROR_OUT_OF_DATE_KHR => "VK_ERROR_OUT_OF_DATE_KHR",
        VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT => {
            "VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT"
        }
        _ => "unknown VkResult",
    }
}

// VkStructureType.
pub const ST_APPLICATION_INFO: VkEnum = 0;
pub const ST_INSTANCE_CREATE_INFO: VkEnum = 1;
pub const ST_DEVICE_QUEUE_CREATE_INFO: VkEnum = 2;
pub const ST_DEVICE_CREATE_INFO: VkEnum = 3;
pub const ST_SUBMIT_INFO: VkEnum = 4;
pub const ST_MEMORY_ALLOCATE_INFO: VkEnum = 5;
pub const ST_FENCE_CREATE_INFO: VkEnum = 8;
pub const ST_SEMAPHORE_CREATE_INFO: VkEnum = 9;
pub const ST_BUFFER_CREATE_INFO: VkEnum = 12;
pub const ST_IMAGE_CREATE_INFO: VkEnum = 14;
pub const ST_IMAGE_VIEW_CREATE_INFO: VkEnum = 15;
pub const ST_SHADER_MODULE_CREATE_INFO: VkEnum = 16;
pub const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: VkEnum = 18;
pub const ST_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO: VkEnum = 19;
pub const ST_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO: VkEnum = 20;
pub const ST_PIPELINE_VIEWPORT_STATE_CREATE_INFO: VkEnum = 22;
pub const ST_PIPELINE_RASTERIZATION_STATE_CREATE_INFO: VkEnum = 23;
pub const ST_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO: VkEnum = 24;
pub const ST_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO: VkEnum = 26;
pub const ST_PIPELINE_DYNAMIC_STATE_CREATE_INFO: VkEnum = 27;
pub const ST_GRAPHICS_PIPELINE_CREATE_INFO: VkEnum = 28;
pub const ST_PIPELINE_LAYOUT_CREATE_INFO: VkEnum = 30;
pub const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: VkEnum = 32;
pub const ST_DESCRIPTOR_POOL_CREATE_INFO: VkEnum = 33;
pub const ST_DESCRIPTOR_SET_ALLOCATE_INFO: VkEnum = 34;
pub const ST_WRITE_DESCRIPTOR_SET: VkEnum = 35;
pub const ST_FRAMEBUFFER_CREATE_INFO: VkEnum = 37;
pub const ST_RENDER_PASS_CREATE_INFO: VkEnum = 38;
pub const ST_COMMAND_POOL_CREATE_INFO: VkEnum = 39;
pub const ST_COMMAND_BUFFER_ALLOCATE_INFO: VkEnum = 40;
pub const ST_COMMAND_BUFFER_BEGIN_INFO: VkEnum = 42;
pub const ST_RENDER_PASS_BEGIN_INFO: VkEnum = 43;
pub const ST_IMAGE_MEMORY_BARRIER: VkEnum = 45;
pub const ST_MEMORY_BARRIER: VkEnum = 46;
pub const ST_SWAPCHAIN_CREATE_INFO_KHR: VkEnum = 1_000_001_000;
pub const ST_PRESENT_INFO_KHR: VkEnum = 1_000_001_001;
pub const ST_WIN32_SURFACE_CREATE_INFO_KHR: VkEnum = 1_000_009_000;

/// `VK_MAKE_API_VERSION(0, 1, 0, 0)`: major in bits 22..28.
pub const VK_API_VERSION_1_0: u32 = 1 << 22;

// VkPhysicalDeviceType.
pub const PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU: VkEnum = 1;
pub const PHYSICAL_DEVICE_TYPE_DISCRETE_GPU: VkEnum = 2;

// VkQueueFlagBits.
pub const QUEUE_GRAPHICS_BIT: VkFlags = 0x1;

// VkMemoryPropertyFlagBits.
pub const MEMORY_PROPERTY_DEVICE_LOCAL_BIT: VkFlags = 0x1;
pub const MEMORY_PROPERTY_HOST_VISIBLE_BIT: VkFlags = 0x2;
pub const MEMORY_PROPERTY_HOST_COHERENT_BIT: VkFlags = 0x4;

// VkBufferUsageFlagBits.
pub const BUFFER_USAGE_TRANSFER_SRC_BIT: VkFlags = 0x1;
pub const BUFFER_USAGE_TRANSFER_DST_BIT: VkFlags = 0x2;
pub const BUFFER_USAGE_VERTEX_BUFFER_BIT: VkFlags = 0x80;

// VkImageUsageFlagBits.
pub const IMAGE_USAGE_TRANSFER_SRC_BIT: VkFlags = 0x1;
pub const IMAGE_USAGE_TRANSFER_DST_BIT: VkFlags = 0x2;
pub const IMAGE_USAGE_SAMPLED_BIT: VkFlags = 0x4;
pub const IMAGE_USAGE_COLOR_ATTACHMENT_BIT: VkFlags = 0x10;

// VkFormat.
pub const FORMAT_R8_UNORM: VkEnum = 9;
pub const FORMAT_R8G8B8A8_UNORM: VkEnum = 37;
pub const FORMAT_B8G8R8A8_UNORM: VkEnum = 44;
pub const FORMAT_R32G32B32A32_UINT: VkEnum = 107;

// VkImageLayout.
pub const IMAGE_LAYOUT_UNDEFINED: VkEnum = 0;
pub const IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL: VkEnum = 2;
pub const IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL: VkEnum = 5;
pub const IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL: VkEnum = 6;
pub const IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL: VkEnum = 7;
pub const IMAGE_LAYOUT_PRESENT_SRC_KHR: VkEnum = 1_000_001_002;

// Assorted enums.
pub const SHARING_MODE_EXCLUSIVE: VkEnum = 0;
pub const IMAGE_TYPE_2D: VkEnum = 1;
pub const IMAGE_VIEW_TYPE_2D: VkEnum = 1;
pub const IMAGE_TILING_OPTIMAL: VkEnum = 0;
pub const SAMPLE_COUNT_1_BIT: VkFlags = 0x1;
pub const COMPONENT_SWIZZLE_IDENTITY: VkEnum = 0;
pub const IMAGE_ASPECT_COLOR_BIT: VkFlags = 0x1;
pub const DESCRIPTOR_TYPE_SAMPLED_IMAGE: VkEnum = 2;
pub const SHADER_STAGE_VERTEX_BIT: VkFlags = 0x1;
pub const SHADER_STAGE_FRAGMENT_BIT: VkFlags = 0x10;
pub const VERTEX_INPUT_RATE_INSTANCE: VkEnum = 1;
pub const PRIMITIVE_TOPOLOGY_TRIANGLE_LIST: VkEnum = 3;
pub const POLYGON_MODE_FILL: VkEnum = 0;
pub const CULL_MODE_NONE: VkFlags = 0;
pub const FRONT_FACE_COUNTER_CLOCKWISE: VkEnum = 0;
/// `R | G | B | A` (`VkColorComponentFlagBits` 0x1, 0x2, 0x4, 0x8).
pub const COLOR_COMPONENT_RGBA: VkFlags = 0xF;
pub const DYNAMIC_STATE_VIEWPORT: VkEnum = 0;
pub const DYNAMIC_STATE_SCISSOR: VkEnum = 1;
pub const ATTACHMENT_LOAD_OP_CLEAR: VkEnum = 1;
pub const ATTACHMENT_LOAD_OP_DONT_CARE: VkEnum = 2;
pub const ATTACHMENT_STORE_OP_STORE: VkEnum = 0;
pub const ATTACHMENT_STORE_OP_DONT_CARE: VkEnum = 1;
pub const PIPELINE_BIND_POINT_GRAPHICS: VkEnum = 0;
pub const SUBPASS_EXTERNAL: u32 = !0;
pub const QUEUE_FAMILY_IGNORED: u32 = !0;
pub const SUBPASS_CONTENTS_INLINE: VkEnum = 0;
pub const COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT: VkFlags = 0x2;
pub const COMMAND_BUFFER_LEVEL_PRIMARY: VkEnum = 0;
pub const COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: VkFlags = 0x1;
pub const FENCE_CREATE_SIGNALED_BIT: VkFlags = 0x1;

// VkPipelineStageFlagBits.
pub const PIPELINE_STAGE_TOP_OF_PIPE_BIT: VkFlags = 0x1;
pub const PIPELINE_STAGE_VERTEX_INPUT_BIT: VkFlags = 0x4;
pub const PIPELINE_STAGE_FRAGMENT_SHADER_BIT: VkFlags = 0x80;
pub const PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT: VkFlags = 0x400;
pub const PIPELINE_STAGE_TRANSFER_BIT: VkFlags = 0x1000;
pub const PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT: VkFlags = 0x2000;
pub const PIPELINE_STAGE_HOST_BIT: VkFlags = 0x4000;

// VkAccessFlagBits.
pub const ACCESS_VERTEX_ATTRIBUTE_READ_BIT: VkFlags = 0x4;
pub const ACCESS_SHADER_READ_BIT: VkFlags = 0x20;
pub const ACCESS_COLOR_ATTACHMENT_WRITE_BIT: VkFlags = 0x100;
pub const ACCESS_TRANSFER_READ_BIT: VkFlags = 0x800;
pub const ACCESS_TRANSFER_WRITE_BIT: VkFlags = 0x1000;
pub const ACCESS_HOST_READ_BIT: VkFlags = 0x2000;

// WSI.
pub const COLOR_SPACE_SRGB_NONLINEAR_KHR: VkEnum = 0;
pub const PRESENT_MODE_IMMEDIATE_KHR: VkEnum = 0;
pub const PRESENT_MODE_MAILBOX_KHR: VkEnum = 1;
pub const PRESENT_MODE_FIFO_KHR: VkEnum = 2;
pub const SURFACE_TRANSFORM_IDENTITY_BIT_KHR: VkFlags = 0x1;
pub const COMPOSITE_ALPHA_OPAQUE_BIT_KHR: VkFlags = 0x1;

pub const KHR_SURFACE: &str = "VK_KHR_surface\0";
pub const KHR_WIN32_SURFACE: &str = "VK_KHR_win32_surface\0";
pub const KHR_SWAPCHAIN: &str = "VK_KHR_swapchain\0";

// ---------------------------------------------------------------- structures

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VkExtent2D {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VkExtent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VkOffset2D {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VkOffset3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VkRect2D {
    pub offset: VkOffset2D,
    pub extent: VkExtent2D,
}

#[repr(C)]
pub struct VkApplicationInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub pApplicationName: *const u8,
    pub applicationVersion: u32,
    pub pEngineName: *const u8,
    pub engineVersion: u32,
    pub apiVersion: u32,
}

#[repr(C)]
pub struct VkInstanceCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub pApplicationInfo: *const VkApplicationInfo,
    pub enabledLayerCount: u32,
    pub ppEnabledLayerNames: *const *const u8,
    pub enabledExtensionCount: u32,
    pub ppEnabledExtensionNames: *const *const u8,
}

/// `VkExtensionProperties`: `char extensionName[256]; uint32_t specVersion;`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkExtensionProperties {
    pub extensionName: [u8; 256],
    pub specVersion: u32,
}

/// `VkPhysicalDeviceProperties`, of which only the head is read.
///
/// The C struct continues with `VkPhysicalDeviceLimits limits` and
/// `VkPhysicalDeviceSparseProperties`, about 540 bytes in all at header 363.
/// Rather than transcribe the ~100 limit fields this backend never reads,
/// the tail is reserved as an over-sized, 8-byte-aligned block: the driver
/// writes `sizeof(VkPhysicalDeviceProperties)` bytes, which fit. The fields
/// declared are at the offsets the C layout gives them (0, 4, 8, 12, 16, 20
/// and 276), independent of what follows.
#[repr(C)]
pub struct VkPhysicalDeviceProperties {
    pub apiVersion: u32,
    pub driverVersion: u32,
    pub vendorID: u32,
    pub deviceID: u32,
    pub deviceType: VkEnum,
    pub deviceName: [u8; 256],
    pub pipelineCacheUUID: [u8; 16],
    /// `limits` and `sparseProperties`, over-reserved (2 KiB).
    pub rest: [u64; 256],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkQueueFamilyProperties {
    pub queueFlags: VkFlags,
    pub queueCount: u32,
    pub timestampValidBits: u32,
    pub minImageTransferGranularity: VkExtent3D,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkMemoryType {
    pub propertyFlags: VkFlags,
    pub heapIndex: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkMemoryHeap {
    pub size: VkDeviceSize,
    pub flags: VkFlags,
}

/// `VK_MAX_MEMORY_TYPES` = 32, `VK_MAX_MEMORY_HEAPS` = 16.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkPhysicalDeviceMemoryProperties {
    pub memoryTypeCount: u32,
    pub memoryTypes: [VkMemoryType; 32],
    pub memoryHeapCount: u32,
    pub memoryHeaps: [VkMemoryHeap; 16],
}

#[repr(C)]
pub struct VkDeviceQueueCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueFamilyIndex: u32,
    pub queueCount: u32,
    pub pQueuePriorities: *const f32,
}

#[repr(C)]
pub struct VkDeviceCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueCreateInfoCount: u32,
    pub pQueueCreateInfos: *const VkDeviceQueueCreateInfo,
    pub enabledLayerCount: u32,
    pub ppEnabledLayerNames: *const *const u8,
    pub enabledExtensionCount: u32,
    pub ppEnabledExtensionNames: *const *const u8,
    /// `const VkPhysicalDeviceFeatures*`; always null (no optional features).
    pub pEnabledFeatures: *const c_void,
}

#[repr(C)]
pub struct VkBufferCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub size: VkDeviceSize,
    pub usage: VkFlags,
    pub sharingMode: VkEnum,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkMemoryRequirements {
    pub size: VkDeviceSize,
    pub alignment: VkDeviceSize,
    pub memoryTypeBits: u32,
}

#[repr(C)]
pub struct VkMemoryAllocateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub allocationSize: VkDeviceSize,
    pub memoryTypeIndex: u32,
}

#[repr(C)]
pub struct VkImageCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub imageType: VkEnum,
    pub format: VkEnum,
    pub extent: VkExtent3D,
    pub mipLevels: u32,
    pub arrayLayers: u32,
    pub samples: VkFlags,
    pub tiling: VkEnum,
    pub usage: VkFlags,
    pub sharingMode: VkEnum,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
    pub initialLayout: VkEnum,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkComponentMapping {
    pub r: VkEnum,
    pub g: VkEnum,
    pub b: VkEnum,
    pub a: VkEnum,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkImageSubresourceRange {
    pub aspectMask: VkFlags,
    pub baseMipLevel: u32,
    pub levelCount: u32,
    pub baseArrayLayer: u32,
    pub layerCount: u32,
}

#[repr(C)]
pub struct VkImageViewCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub image: VkImage,
    pub viewType: VkEnum,
    pub format: VkEnum,
    pub components: VkComponentMapping,
    pub subresourceRange: VkImageSubresourceRange,
}

#[repr(C)]
pub struct VkShaderModuleCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    /// In bytes.
    pub codeSize: usize,
    pub pCode: *const u32,
}

#[repr(C)]
pub struct VkDescriptorSetLayoutBinding {
    pub binding: u32,
    pub descriptorType: VkEnum,
    pub descriptorCount: u32,
    pub stageFlags: VkFlags,
    pub pImmutableSamplers: *const VkSampler,
}

#[repr(C)]
pub struct VkDescriptorSetLayoutCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub bindingCount: u32,
    pub pBindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
pub struct VkDescriptorPoolSize {
    pub ty: VkEnum,
    pub descriptorCount: u32,
}

#[repr(C)]
pub struct VkDescriptorPoolCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub maxSets: u32,
    pub poolSizeCount: u32,
    pub pPoolSizes: *const VkDescriptorPoolSize,
}

#[repr(C)]
pub struct VkDescriptorSetAllocateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub descriptorPool: VkDescriptorPool,
    pub descriptorSetCount: u32,
    pub pSetLayouts: *const VkDescriptorSetLayout,
}

#[repr(C)]
pub struct VkDescriptorImageInfo {
    pub sampler: VkSampler,
    pub imageView: VkImageView,
    pub imageLayout: VkEnum,
}

#[repr(C)]
pub struct VkWriteDescriptorSet {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub dstSet: VkDescriptorSet,
    pub dstBinding: u32,
    pub dstArrayElement: u32,
    pub descriptorCount: u32,
    pub descriptorType: VkEnum,
    pub pImageInfo: *const VkDescriptorImageInfo,
    /// `const VkDescriptorBufferInfo*`; unused (null).
    pub pBufferInfo: *const c_void,
    /// `const VkBufferView*`; unused (null).
    pub pTexelBufferView: *const c_void,
}

#[repr(C)]
pub struct VkPushConstantRange {
    pub stageFlags: VkFlags,
    pub offset: u32,
    pub size: u32,
}

#[repr(C)]
pub struct VkPipelineLayoutCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub setLayoutCount: u32,
    pub pSetLayouts: *const VkDescriptorSetLayout,
    pub pushConstantRangeCount: u32,
    pub pPushConstantRanges: *const VkPushConstantRange,
}

#[repr(C)]
pub struct VkPipelineShaderStageCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub stage: VkFlags,
    pub module: VkShaderModule,
    pub pName: *const u8,
    /// `const VkSpecializationInfo*`; unused (null).
    pub pSpecializationInfo: *const c_void,
}

#[repr(C)]
pub struct VkVertexInputBindingDescription {
    pub binding: u32,
    pub stride: u32,
    pub inputRate: VkEnum,
}

#[repr(C)]
pub struct VkVertexInputAttributeDescription {
    pub location: u32,
    pub binding: u32,
    pub format: VkEnum,
    pub offset: u32,
}

#[repr(C)]
pub struct VkPipelineVertexInputStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub vertexBindingDescriptionCount: u32,
    pub pVertexBindingDescriptions: *const VkVertexInputBindingDescription,
    pub vertexAttributeDescriptionCount: u32,
    pub pVertexAttributeDescriptions: *const VkVertexInputAttributeDescription,
}

#[repr(C)]
pub struct VkPipelineInputAssemblyStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub topology: VkEnum,
    pub primitiveRestartEnable: VkBool32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkViewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub minDepth: f32,
    pub maxDepth: f32,
}

#[repr(C)]
pub struct VkPipelineViewportStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub viewportCount: u32,
    pub pViewports: *const VkViewport,
    pub scissorCount: u32,
    pub pScissors: *const VkRect2D,
}

#[repr(C)]
pub struct VkPipelineRasterizationStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub depthClampEnable: VkBool32,
    pub rasterizerDiscardEnable: VkBool32,
    pub polygonMode: VkEnum,
    pub cullMode: VkFlags,
    pub frontFace: VkEnum,
    pub depthBiasEnable: VkBool32,
    pub depthBiasConstantFactor: f32,
    pub depthBiasClamp: f32,
    pub depthBiasSlopeFactor: f32,
    pub lineWidth: f32,
}

#[repr(C)]
pub struct VkPipelineMultisampleStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub rasterizationSamples: VkFlags,
    pub sampleShadingEnable: VkBool32,
    pub minSampleShading: f32,
    pub pSampleMask: *const u32,
    pub alphaToCoverageEnable: VkBool32,
    pub alphaToOneEnable: VkBool32,
}

#[repr(C)]
pub struct VkPipelineColorBlendAttachmentState {
    pub blendEnable: VkBool32,
    pub srcColorBlendFactor: VkEnum,
    pub dstColorBlendFactor: VkEnum,
    pub colorBlendOp: VkEnum,
    pub srcAlphaBlendFactor: VkEnum,
    pub dstAlphaBlendFactor: VkEnum,
    pub alphaBlendOp: VkEnum,
    pub colorWriteMask: VkFlags,
}

#[repr(C)]
pub struct VkPipelineColorBlendStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub logicOpEnable: VkBool32,
    pub logicOp: VkEnum,
    pub attachmentCount: u32,
    pub pAttachments: *const VkPipelineColorBlendAttachmentState,
    pub blendConstants: [f32; 4],
}

#[repr(C)]
pub struct VkPipelineDynamicStateCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub dynamicStateCount: u32,
    pub pDynamicStates: *const VkEnum,
}

#[repr(C)]
pub struct VkGraphicsPipelineCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub stageCount: u32,
    pub pStages: *const VkPipelineShaderStageCreateInfo,
    pub pVertexInputState: *const VkPipelineVertexInputStateCreateInfo,
    pub pInputAssemblyState: *const VkPipelineInputAssemblyStateCreateInfo,
    /// `const VkPipelineTessellationStateCreateInfo*`; null.
    pub pTessellationState: *const c_void,
    pub pViewportState: *const VkPipelineViewportStateCreateInfo,
    pub pRasterizationState: *const VkPipelineRasterizationStateCreateInfo,
    pub pMultisampleState: *const VkPipelineMultisampleStateCreateInfo,
    /// `const VkPipelineDepthStencilStateCreateInfo*`; null (no depth).
    pub pDepthStencilState: *const c_void,
    pub pColorBlendState: *const VkPipelineColorBlendStateCreateInfo,
    pub pDynamicState: *const VkPipelineDynamicStateCreateInfo,
    pub layout: VkPipelineLayout,
    pub renderPass: VkRenderPass,
    pub subpass: u32,
    pub basePipelineHandle: VkPipeline,
    pub basePipelineIndex: i32,
}

#[repr(C)]
pub struct VkAttachmentDescription {
    pub flags: VkFlags,
    pub format: VkEnum,
    pub samples: VkFlags,
    pub loadOp: VkEnum,
    pub storeOp: VkEnum,
    pub stencilLoadOp: VkEnum,
    pub stencilStoreOp: VkEnum,
    pub initialLayout: VkEnum,
    pub finalLayout: VkEnum,
}

#[repr(C)]
pub struct VkAttachmentReference {
    pub attachment: u32,
    pub layout: VkEnum,
}

#[repr(C)]
pub struct VkSubpassDescription {
    pub flags: VkFlags,
    pub pipelineBindPoint: VkEnum,
    pub inputAttachmentCount: u32,
    pub pInputAttachments: *const VkAttachmentReference,
    pub colorAttachmentCount: u32,
    pub pColorAttachments: *const VkAttachmentReference,
    pub pResolveAttachments: *const VkAttachmentReference,
    pub pDepthStencilAttachment: *const VkAttachmentReference,
    pub preserveAttachmentCount: u32,
    pub pPreserveAttachments: *const u32,
}

#[repr(C)]
pub struct VkSubpassDependency {
    pub srcSubpass: u32,
    pub dstSubpass: u32,
    pub srcStageMask: VkFlags,
    pub dstStageMask: VkFlags,
    pub srcAccessMask: VkFlags,
    pub dstAccessMask: VkFlags,
    pub dependencyFlags: VkFlags,
}

#[repr(C)]
pub struct VkRenderPassCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub attachmentCount: u32,
    pub pAttachments: *const VkAttachmentDescription,
    pub subpassCount: u32,
    pub pSubpasses: *const VkSubpassDescription,
    pub dependencyCount: u32,
    pub pDependencies: *const VkSubpassDependency,
}

#[repr(C)]
pub struct VkFramebufferCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub renderPass: VkRenderPass,
    pub attachmentCount: u32,
    pub pAttachments: *const VkImageView,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
}

#[repr(C)]
pub struct VkCommandPoolCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueFamilyIndex: u32,
}

#[repr(C)]
pub struct VkCommandBufferAllocateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub commandPool: VkCommandPool,
    pub level: VkEnum,
    pub commandBufferCount: u32,
}

#[repr(C)]
pub struct VkCommandBufferBeginInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    /// `const VkCommandBufferInheritanceInfo*`; null for a primary buffer.
    pub pInheritanceInfo: *const c_void,
}

#[repr(C)]
pub struct VkMemoryBarrier {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub srcAccessMask: VkFlags,
    pub dstAccessMask: VkFlags,
}

#[repr(C)]
pub struct VkImageMemoryBarrier {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub srcAccessMask: VkFlags,
    pub dstAccessMask: VkFlags,
    pub oldLayout: VkEnum,
    pub newLayout: VkEnum,
    pub srcQueueFamilyIndex: u32,
    pub dstQueueFamilyIndex: u32,
    pub image: VkImage,
    pub subresourceRange: VkImageSubresourceRange,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkImageSubresourceLayers {
    pub aspectMask: VkFlags,
    pub mipLevel: u32,
    pub baseArrayLayer: u32,
    pub layerCount: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkBufferImageCopy {
    pub bufferOffset: VkDeviceSize,
    pub bufferRowLength: u32,
    pub bufferImageHeight: u32,
    pub imageSubresource: VkImageSubresourceLayers,
    pub imageOffset: VkOffset3D,
    pub imageExtent: VkExtent3D,
}

/// `VkClearValue`, a union of `VkClearColorValue` (16 bytes: `float[4]` /
/// `int32_t[4]` / `uint32_t[4]`) and `VkClearDepthStencilValue` (8 bytes).
/// Only the float colour arm is used, so the union is its 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkClearValue {
    pub float32: [f32; 4],
}

#[repr(C)]
pub struct VkRenderPassBeginInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub renderPass: VkRenderPass,
    pub framebuffer: VkFramebuffer,
    pub renderArea: VkRect2D,
    pub clearValueCount: u32,
    pub pClearValues: *const VkClearValue,
}

#[repr(C)]
pub struct VkSubmitInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub waitSemaphoreCount: u32,
    pub pWaitSemaphores: *const VkSemaphore,
    pub pWaitDstStageMask: *const VkFlags,
    pub commandBufferCount: u32,
    pub pCommandBuffers: *const VkCommandBuffer,
    pub signalSemaphoreCount: u32,
    pub pSignalSemaphores: *const VkSemaphore,
}

#[repr(C)]
pub struct VkFenceCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkSemaphoreCreateInfo {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkWin32SurfaceCreateInfoKHR {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub hinstance: *mut c_void,
    pub hwnd: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceCapabilitiesKHR {
    pub minImageCount: u32,
    pub maxImageCount: u32,
    pub currentExtent: VkExtent2D,
    pub minImageExtent: VkExtent2D,
    pub maxImageExtent: VkExtent2D,
    pub maxImageArrayLayers: u32,
    pub supportedTransforms: VkFlags,
    pub currentTransform: VkFlags,
    pub supportedCompositeAlpha: VkFlags,
    pub supportedUsageFlags: VkFlags,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceFormatKHR {
    pub format: VkEnum,
    pub colorSpace: VkEnum,
}

#[repr(C)]
pub struct VkSwapchainCreateInfoKHR {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub surface: VkSurfaceKHR,
    pub minImageCount: u32,
    pub imageFormat: VkEnum,
    pub imageColorSpace: VkEnum,
    pub imageExtent: VkExtent2D,
    pub imageArrayLayers: u32,
    pub imageUsage: VkFlags,
    pub imageSharingMode: VkEnum,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
    pub preTransform: VkFlags,
    pub compositeAlpha: VkFlags,
    pub presentMode: VkEnum,
    pub clipped: VkBool32,
    pub oldSwapchain: VkSwapchainKHR,
}

#[repr(C)]
pub struct VkPresentInfoKHR {
    pub sType: VkEnum,
    pub pNext: *const c_void,
    pub waitSemaphoreCount: u32,
    pub pWaitSemaphores: *const VkSemaphore,
    pub swapchainCount: u32,
    pub pSwapchains: *const VkSwapchainKHR,
    pub pImageIndices: *const u32,
    pub pResults: *mut VkResult,
}

// ------------------------------------------------------------- entry points

/// `PFN_vkVoidFunction`, as returned by the `Get*ProcAddr` functions.
pub type PFN_vkVoidFunction = Option<unsafe extern "system" fn()>;
pub type PFN_vkGetInstanceProcAddr =
    unsafe extern "system" fn(instance: VkInstance, name: *const u8) -> PFN_vkVoidFunction;

/// `const VkAllocationCallbacks*`; always null (the driver's allocator).
pub type Alloc = *const c_void;

macro_rules! fns {
    ($table:ident { $($field:ident : $name:literal = fn($($arg:ty),*) $(-> $ret:ty)?;)* }) => {
        /// Entry points, resolved once.
        pub struct $table {
            $(pub $field: unsafe extern "system" fn($($arg),*) $(-> $ret)?,)*
        }

        impl $table {
            /// Resolve every entry point with `get`, which returns the raw
            /// address for a NUL-terminated name or `None`.
            pub fn load(
                mut get: impl FnMut(&'static str) -> PFN_vkVoidFunction,
            ) -> Result<$table, &'static str> {
                Ok($table {
                    $($field: {
                        let name = concat!($name, "\0");
                        let f = get(name).ok_or($name)?;
                        // SAFETY: the loader returned this address for
                        // `$name`, whose prototype in vulkan_core.h is the
                        // field's type; function pointers are all the same
                        // size, and `extern "system"` is VKAPI_PTR.
                        unsafe {
                            std::mem::transmute::<
                                unsafe extern "system" fn(),
                                unsafe extern "system" fn($($arg),*) $(-> $ret)?,
                            >(f)
                        }
                    },)*
                })
            }
        }
    };
}

fns!(EntryFns {
    create_instance: "vkCreateInstance" =
        fn(*const VkInstanceCreateInfo, Alloc, *mut VkInstance) -> VkResult;
    enumerate_instance_extension_properties: "vkEnumerateInstanceExtensionProperties" =
        fn(*const u8, *mut u32, *mut VkExtensionProperties) -> VkResult;
});

fns!(InstanceFns {
    destroy_instance: "vkDestroyInstance" = fn(VkInstance, Alloc);
    enumerate_physical_devices: "vkEnumeratePhysicalDevices" =
        fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult;
    get_physical_device_properties: "vkGetPhysicalDeviceProperties" =
        fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties);
    get_physical_device_queue_family_properties: "vkGetPhysicalDeviceQueueFamilyProperties" =
        fn(VkPhysicalDevice, *mut u32, *mut VkQueueFamilyProperties);
    get_physical_device_memory_properties: "vkGetPhysicalDeviceMemoryProperties" =
        fn(VkPhysicalDevice, *mut VkPhysicalDeviceMemoryProperties);
    enumerate_device_extension_properties: "vkEnumerateDeviceExtensionProperties" =
        fn(VkPhysicalDevice, *const u8, *mut u32, *mut VkExtensionProperties) -> VkResult;
    create_device: "vkCreateDevice" =
        fn(VkPhysicalDevice, *const VkDeviceCreateInfo, Alloc, *mut VkDevice) -> VkResult;
    get_device_proc_addr: "vkGetDeviceProcAddr" =
        fn(VkDevice, *const u8) -> PFN_vkVoidFunction;
    // Also reachable through the instance (a loader trampoline); used only
    // to clean up when the device's own table cannot be loaded.
    destroy_device: "vkDestroyDevice" = fn(VkDevice, Alloc);
});

fns!(SurfaceFns {
    destroy_surface: "vkDestroySurfaceKHR" = fn(VkInstance, VkSurfaceKHR, Alloc);
    get_physical_device_surface_support: "vkGetPhysicalDeviceSurfaceSupportKHR" =
        fn(VkPhysicalDevice, u32, VkSurfaceKHR, *mut VkBool32) -> VkResult;
    get_physical_device_surface_capabilities: "vkGetPhysicalDeviceSurfaceCapabilitiesKHR" =
        fn(VkPhysicalDevice, VkSurfaceKHR, *mut VkSurfaceCapabilitiesKHR) -> VkResult;
    get_physical_device_surface_formats: "vkGetPhysicalDeviceSurfaceFormatsKHR" =
        fn(VkPhysicalDevice, VkSurfaceKHR, *mut u32, *mut VkSurfaceFormatKHR) -> VkResult;
    get_physical_device_surface_present_modes: "vkGetPhysicalDeviceSurfacePresentModesKHR" =
        fn(VkPhysicalDevice, VkSurfaceKHR, *mut u32, *mut VkEnum) -> VkResult;
    create_win32_surface: "vkCreateWin32SurfaceKHR" =
        fn(VkInstance, *const VkWin32SurfaceCreateInfoKHR, Alloc, *mut VkSurfaceKHR) -> VkResult;
});

fns!(DeviceFns {
    destroy_device: "vkDestroyDevice" = fn(VkDevice, Alloc);
    get_device_queue: "vkGetDeviceQueue" = fn(VkDevice, u32, u32, *mut VkQueue);
    device_wait_idle: "vkDeviceWaitIdle" = fn(VkDevice) -> VkResult;
    queue_submit: "vkQueueSubmit" =
        fn(VkQueue, u32, *const VkSubmitInfo, VkFence) -> VkResult;
    create_buffer: "vkCreateBuffer" =
        fn(VkDevice, *const VkBufferCreateInfo, Alloc, *mut VkBuffer) -> VkResult;
    destroy_buffer: "vkDestroyBuffer" = fn(VkDevice, VkBuffer, Alloc);
    get_buffer_memory_requirements: "vkGetBufferMemoryRequirements" =
        fn(VkDevice, VkBuffer, *mut VkMemoryRequirements);
    allocate_memory: "vkAllocateMemory" =
        fn(VkDevice, *const VkMemoryAllocateInfo, Alloc, *mut VkDeviceMemory) -> VkResult;
    free_memory: "vkFreeMemory" = fn(VkDevice, VkDeviceMemory, Alloc);
    bind_buffer_memory: "vkBindBufferMemory" =
        fn(VkDevice, VkBuffer, VkDeviceMemory, VkDeviceSize) -> VkResult;
    map_memory: "vkMapMemory" =
        fn(VkDevice, VkDeviceMemory, VkDeviceSize, VkDeviceSize, VkFlags, *mut *mut c_void) -> VkResult;
    unmap_memory: "vkUnmapMemory" = fn(VkDevice, VkDeviceMemory);
    create_image: "vkCreateImage" =
        fn(VkDevice, *const VkImageCreateInfo, Alloc, *mut VkImage) -> VkResult;
    destroy_image: "vkDestroyImage" = fn(VkDevice, VkImage, Alloc);
    get_image_memory_requirements: "vkGetImageMemoryRequirements" =
        fn(VkDevice, VkImage, *mut VkMemoryRequirements);
    bind_image_memory: "vkBindImageMemory" =
        fn(VkDevice, VkImage, VkDeviceMemory, VkDeviceSize) -> VkResult;
    create_image_view: "vkCreateImageView" =
        fn(VkDevice, *const VkImageViewCreateInfo, Alloc, *mut VkImageView) -> VkResult;
    destroy_image_view: "vkDestroyImageView" = fn(VkDevice, VkImageView, Alloc);
    create_shader_module: "vkCreateShaderModule" =
        fn(VkDevice, *const VkShaderModuleCreateInfo, Alloc, *mut VkShaderModule) -> VkResult;
    destroy_shader_module: "vkDestroyShaderModule" = fn(VkDevice, VkShaderModule, Alloc);
    create_descriptor_set_layout: "vkCreateDescriptorSetLayout" =
        fn(VkDevice, *const VkDescriptorSetLayoutCreateInfo, Alloc, *mut VkDescriptorSetLayout) -> VkResult;
    destroy_descriptor_set_layout: "vkDestroyDescriptorSetLayout" =
        fn(VkDevice, VkDescriptorSetLayout, Alloc);
    create_descriptor_pool: "vkCreateDescriptorPool" =
        fn(VkDevice, *const VkDescriptorPoolCreateInfo, Alloc, *mut VkDescriptorPool) -> VkResult;
    destroy_descriptor_pool: "vkDestroyDescriptorPool" = fn(VkDevice, VkDescriptorPool, Alloc);
    allocate_descriptor_sets: "vkAllocateDescriptorSets" =
        fn(VkDevice, *const VkDescriptorSetAllocateInfo, *mut VkDescriptorSet) -> VkResult;
    update_descriptor_sets: "vkUpdateDescriptorSets" =
        fn(VkDevice, u32, *const VkWriteDescriptorSet, u32, *const c_void);
    create_pipeline_layout: "vkCreatePipelineLayout" =
        fn(VkDevice, *const VkPipelineLayoutCreateInfo, Alloc, *mut VkPipelineLayout) -> VkResult;
    destroy_pipeline_layout: "vkDestroyPipelineLayout" = fn(VkDevice, VkPipelineLayout, Alloc);
    create_graphics_pipelines: "vkCreateGraphicsPipelines" =
        fn(VkDevice, VkPipelineCache, u32, *const VkGraphicsPipelineCreateInfo, Alloc, *mut VkPipeline) -> VkResult;
    destroy_pipeline: "vkDestroyPipeline" = fn(VkDevice, VkPipeline, Alloc);
    create_render_pass: "vkCreateRenderPass" =
        fn(VkDevice, *const VkRenderPassCreateInfo, Alloc, *mut VkRenderPass) -> VkResult;
    destroy_render_pass: "vkDestroyRenderPass" = fn(VkDevice, VkRenderPass, Alloc);
    create_framebuffer: "vkCreateFramebuffer" =
        fn(VkDevice, *const VkFramebufferCreateInfo, Alloc, *mut VkFramebuffer) -> VkResult;
    destroy_framebuffer: "vkDestroyFramebuffer" = fn(VkDevice, VkFramebuffer, Alloc);
    create_command_pool: "vkCreateCommandPool" =
        fn(VkDevice, *const VkCommandPoolCreateInfo, Alloc, *mut VkCommandPool) -> VkResult;
    destroy_command_pool: "vkDestroyCommandPool" = fn(VkDevice, VkCommandPool, Alloc);
    allocate_command_buffers: "vkAllocateCommandBuffers" =
        fn(VkDevice, *const VkCommandBufferAllocateInfo, *mut VkCommandBuffer) -> VkResult;
    begin_command_buffer: "vkBeginCommandBuffer" =
        fn(VkCommandBuffer, *const VkCommandBufferBeginInfo) -> VkResult;
    end_command_buffer: "vkEndCommandBuffer" = fn(VkCommandBuffer) -> VkResult;
    cmd_pipeline_barrier: "vkCmdPipelineBarrier" =
        fn(VkCommandBuffer, VkFlags, VkFlags, VkFlags, u32, *const VkMemoryBarrier, u32, *const c_void, u32, *const VkImageMemoryBarrier);
    cmd_copy_buffer_to_image: "vkCmdCopyBufferToImage" =
        fn(VkCommandBuffer, VkBuffer, VkImage, VkEnum, u32, *const VkBufferImageCopy);
    cmd_copy_image_to_buffer: "vkCmdCopyImageToBuffer" =
        fn(VkCommandBuffer, VkImage, VkEnum, VkBuffer, u32, *const VkBufferImageCopy);
    cmd_begin_render_pass: "vkCmdBeginRenderPass" =
        fn(VkCommandBuffer, *const VkRenderPassBeginInfo, VkEnum);
    cmd_end_render_pass: "vkCmdEndRenderPass" = fn(VkCommandBuffer);
    cmd_bind_pipeline: "vkCmdBindPipeline" = fn(VkCommandBuffer, VkEnum, VkPipeline);
    cmd_bind_descriptor_sets: "vkCmdBindDescriptorSets" =
        fn(VkCommandBuffer, VkEnum, VkPipelineLayout, u32, u32, *const VkDescriptorSet, u32, *const u32);
    cmd_bind_vertex_buffers: "vkCmdBindVertexBuffers" =
        fn(VkCommandBuffer, u32, u32, *const VkBuffer, *const VkDeviceSize);
    cmd_push_constants: "vkCmdPushConstants" =
        fn(VkCommandBuffer, VkPipelineLayout, VkFlags, u32, u32, *const c_void);
    cmd_draw: "vkCmdDraw" = fn(VkCommandBuffer, u32, u32, u32, u32);
    cmd_set_viewport: "vkCmdSetViewport" = fn(VkCommandBuffer, u32, u32, *const VkViewport);
    cmd_set_scissor: "vkCmdSetScissor" = fn(VkCommandBuffer, u32, u32, *const VkRect2D);
    create_fence: "vkCreateFence" =
        fn(VkDevice, *const VkFenceCreateInfo, Alloc, *mut VkFence) -> VkResult;
    destroy_fence: "vkDestroyFence" = fn(VkDevice, VkFence, Alloc);
    wait_for_fences: "vkWaitForFences" =
        fn(VkDevice, u32, *const VkFence, VkBool32, u64) -> VkResult;
    reset_fences: "vkResetFences" = fn(VkDevice, u32, *const VkFence) -> VkResult;
    create_semaphore: "vkCreateSemaphore" =
        fn(VkDevice, *const VkSemaphoreCreateInfo, Alloc, *mut VkSemaphore) -> VkResult;
    destroy_semaphore: "vkDestroySemaphore" = fn(VkDevice, VkSemaphore, Alloc);
});

fns!(SwapchainFns {
    create_swapchain: "vkCreateSwapchainKHR" =
        fn(VkDevice, *const VkSwapchainCreateInfoKHR, Alloc, *mut VkSwapchainKHR) -> VkResult;
    destroy_swapchain: "vkDestroySwapchainKHR" = fn(VkDevice, VkSwapchainKHR, Alloc);
    get_swapchain_images: "vkGetSwapchainImagesKHR" =
        fn(VkDevice, VkSwapchainKHR, *mut u32, *mut VkImage) -> VkResult;
    acquire_next_image: "vkAcquireNextImageKHR" =
        fn(VkDevice, VkSwapchainKHR, u64, VkSemaphore, VkFence, *mut u32) -> VkResult;
    queue_present: "vkQueuePresentKHR" = fn(VkQueue, *const VkPresentInfoKHR) -> VkResult;
});

// ------------------------------------------------------------------- loader

type Hmodule = *mut c_void;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> Hmodule;
    fn GetProcAddress(module: Hmodule, name: *const u8) -> *mut c_void;
    fn GetLastError() -> u32;
}

/// Why the loader could not be reached.
pub enum LoadError {
    /// `LoadLibraryExW` failed; the Win32 error code.
    Library(u32),
    /// The DLL, or `vkGetInstanceProcAddr`, lacks this export.
    Symbol(&'static str),
}

/// Load `vulkan-1.dll` from System32 and return `vkGetInstanceProcAddr`.
///
/// The module is never freed: it is reference-counted by Windows, cheap to
/// keep, and unloading a driver stack under its own threads is a known crash.
pub fn load() -> Result<PFN_vkGetInstanceProcAddr, LoadError> {
    let name: Vec<u16> = "vulkan-1.dll\0".encode_utf16().collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call;
    // the reserved file handle is null as documented. Failure returns null.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        // SAFETY: a plain thread-local query, read straight after the failure.
        return Err(LoadError::Library(unsafe { GetLastError() }));
    }
    // SAFETY: a live module handle and a NUL-terminated ASCII name; a missing
    // export returns null, which is checked.
    let p = unsafe { GetProcAddress(module, b"vkGetInstanceProcAddr\0".as_ptr()) };
    if p.is_null() {
        return Err(LoadError::Symbol("vkGetInstanceProcAddr"));
    }
    // SAFETY: a non-null export of vulkan-1.dll named vkGetInstanceProcAddr,
    // whose prototype is `PFN_vkGetInstanceProcAddr`; code and data pointers
    // are the same size on Windows.
    Ok(unsafe { std::mem::transmute::<*mut c_void, PFN_vkGetInstanceProcAddr>(p) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    /// Sizes computed by hand from the C layout rules (x64: pointers and
    /// u64 are 8-aligned, enums and u32 are 4) for structs whose padding is
    /// easy to get wrong.
    #[test]
    fn struct_sizes_match_the_c_layout() {
        assert_eq!(size_of::<VkApplicationInfo>(), 48);
        assert_eq!(size_of::<VkInstanceCreateInfo>(), 64);
        assert_eq!(size_of::<VkExtensionProperties>(), 260);
        assert_eq!(size_of::<VkQueueFamilyProperties>(), 24);
        assert_eq!(size_of::<VkPhysicalDeviceMemoryProperties>(), 520);
        assert_eq!(size_of::<VkDeviceQueueCreateInfo>(), 40);
        assert_eq!(size_of::<VkDeviceCreateInfo>(), 72);
        assert_eq!(size_of::<VkBufferCreateInfo>(), 56);
        assert_eq!(size_of::<VkMemoryRequirements>(), 24);
        assert_eq!(size_of::<VkMemoryAllocateInfo>(), 32);
        assert_eq!(size_of::<VkImageCreateInfo>(), 88);
        assert_eq!(size_of::<VkImageViewCreateInfo>(), 80);
        assert_eq!(size_of::<VkWriteDescriptorSet>(), 64);
        assert_eq!(size_of::<VkGraphicsPipelineCreateInfo>(), 144);
        assert_eq!(size_of::<VkSubpassDescription>(), 72);
        assert_eq!(size_of::<VkImageMemoryBarrier>(), 72);
        assert_eq!(size_of::<VkBufferImageCopy>(), 56);
        assert_eq!(size_of::<VkRenderPassBeginInfo>(), 64);
        assert_eq!(size_of::<VkSubmitInfo>(), 72);
        assert_eq!(size_of::<VkSurfaceCapabilitiesKHR>(), 52);
        assert_eq!(size_of::<VkSwapchainCreateInfoKHR>(), 104);
        assert_eq!(size_of::<VkPresentInfoKHR>(), 64);
        assert_eq!(size_of::<VkClearValue>(), 16);
        assert_eq!(align_of::<VkPhysicalDeviceProperties>(), 8);
    }
}
