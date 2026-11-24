//! Vulkan bootstrap: instance, physical/logical device, and a surface bound to
//! the Android ANativeWindow (`vkCreateAndroidSurfaceKHR`). This is shared by
//! phase 1 (handing an instance to DXVK Native) and phase 2 (the native backend).
//!
//! Every entry point is resolved through `vkGetInstanceProcAddr`: the one from
//! the libadrenotools loader when a custom driver is active (see
//! `runtime/src/adrenotools.rs`), otherwise the system `libvulkan.so`.

pub struct VkContext {
    pub instance: u64,
    pub physical_device: u64,
    pub device: u64,
    pub queue: u64,
    pub queue_family: u32,
    pub surface: u64,
}

/// Minimal Vulkan entry points, resolved from a loader's `vkGetInstanceProcAddr`.
struct VkFns {
    create_instance: unsafe extern "system" fn(*const u8, *const u8, *const u8, *mut u64) -> i32,
    enumerate_physical_devices: unsafe extern "system" fn(u64, *mut u32, *mut u64) -> i32,
    create_device: unsafe extern "system" fn(u64, *const u8, *const u8, *mut u64) -> i32,
    get_device_queue: unsafe extern "system" fn(u64, u32, u32, *mut u64),
    create_android_surface: unsafe extern "system" fn(u64, *const u8, *const u8, *mut u64) -> i32,
    destroy_instance: unsafe extern "system" fn(u64, *const u8),
}

use std::ffi::c_void;

/// `vkGetInstanceProcAddr`, either from the system loader or from the
/// libadrenotools-opened loader (the custom Turnip driver).
type Gipa = unsafe extern "system" fn(u64, *const u8) -> usize;

/// Locate a `vkGetInstanceProcAddr`: the custom-driver loader when
/// libadrenotools opened one, else the system `libvulkan.so`.
///
/// A custom driver can only be reached through the loader libadrenotools
/// returned (`adrenotools_open_libvulkan`); that loader must be used for every
/// Vulkan call, not just for the instance, or the instance would belong to a
/// different driver than the one chosen. See `runtime/src/adrenotools.rs`.
unsafe fn get_instance_proc_addr() -> Option<Gipa> {
    if let Some(gipa) = crate::adrenotools::instance_proc_addr() {
        crate::glog!("[gow.vk] using the libadrenotools loader's vkGetInstanceProcAddr");
        return Some(std::mem::transmute(gipa));
    }

    extern "C" {
        fn dlopen(p: *const std::os::raw::c_char, f: i32) -> *mut c_void;
        fn dlsym(h: *mut c_void, s: *const std::os::raw::c_char) -> *mut c_void;
    }
    // Android ships the loader as `libvulkan.so` (no `.so.1` suffix, unlike
    // desktop Linux). Try both.
    let mut h = std::ptr::null_mut();
    for name in ["libvulkan.so", "libvulkan.so.1"] {
        if let Ok(c) = std::ffi::CString::new(name) {
            h = dlopen(c.as_ptr(), 2);
            if !h.is_null() {
                crate::glog!("[gow.vk] using {name}");
                break;
            }
        }
    }
    if h.is_null() {
        return None;
    }
    let sym = dlsym(h, b"vkGetInstanceProcAddr\0".as_ptr() as *const std::os::raw::c_char);
    if sym.is_null() {
        return None;
    }
    Some(std::mem::transmute(sym))
}

/// Resolve one entry point through a `vkGetInstanceProcAddr`.
unsafe fn resolve(gipa: Gipa, instance: u64, name: &str) -> usize {
    let c = std::ffi::CString::new(name).unwrap();
    gipa(instance, c.as_ptr() as *const u8)
}

impl VkContext {
    /// Bring up a Vulkan instance/device and a surface for the ANativeWindow.
    pub fn new(anative_window: *mut c_void, app_name: &str) -> Option<Self> {
        unsafe {
            let gipa = get_instance_proc_addr()?;
            let create_instance: unsafe extern "system" fn(
                *const u8,
                *const u8,
                *const u8,
                *mut u64,
            ) -> i32 = std::mem::transmute(resolve(gipa, 0, "vkCreateInstance"));

            // VkApplicationInfo (minimal, sType=0, apiVersion 1.1)
            #[repr(C)] struct AppInfo { s: u32, _p: usize, name: *const u8, _e: u32, _ev: u32, api: u32, _av: u32, _ae: u32 }
            let name = std::ffi::CString::new(app_name).unwrap();
            let app = AppInfo { s: 0, _p: 0, name: name.as_ptr() as *const u8, _e: 0, _ev: 0, api: (1 << 22) | (1 << 12), _av: 0, _ae: 0 };
            // extensions: VK_KHR_surface + VK_KHR_android_surface
            let ext_names = [b"VK_KHR_surface\0".as_ptr(), b"VK_KHR_android_surface\0".as_ptr()];
            let mut inst: u64 = 0;
            if create_instance(&app as *const _ as *const u8, std::ptr::null(), ext_names.as_ptr() as *const u8, &mut inst) != 0 {
                crate::glog!("[gow.vk] vkCreateInstance failed");
                return None;
            }

            let f = VkFns {
                create_instance,
                enumerate_physical_devices: std::mem::transmute(resolve(gipa, inst, "vkEnumeratePhysicalDevices")),
                create_device: std::mem::transmute(resolve(gipa, inst, "vkCreateDevice")),
                get_device_queue: std::mem::transmute(resolve(gipa, inst, "vkGetDeviceQueue")),
                create_android_surface: std::mem::transmute(resolve(gipa, inst, "vkCreateAndroidSurfaceKHR")),
                destroy_instance: std::mem::transmute(resolve(gipa, inst, "vkDestroyInstance")),
            };
            let mut count: u32 = 0;
            (f.enumerate_physical_devices)(inst, &mut count, std::ptr::null_mut());
            if count == 0 {
                crate::glog!("[gow.vk] no Vulkan physical devices");
                (f.destroy_instance)(inst, std::ptr::null());
                return None;
            }
            let mut devs = vec![0u64; count as usize];
            (f.enumerate_physical_devices)(inst, &mut count, devs.as_mut_ptr());
            let physical = devs[0];

            // Android surface from the ANativeWindow.
            #[repr(C)] struct AndroidSurface { s: u32, _p: usize, _f: u32, window: *mut c_void }
            let surf_info = AndroidSurface { s: 1000019000, _p: 0, _f: 0, window: anative_window };
            let mut surface: u64 = 0;
            if (f.create_android_surface)(inst, &surf_info as *const _ as *const u8, std::ptr::null(), &mut surface) != 0 {
                crate::glog!("[gow.vk] vkCreateAndroidSurfaceKHR failed");
            }

            // One graphics queue from family 0 (correct selection comes with the
            // full backend; the family is queried there).
            #[repr(C)] struct DevQueue { s: u32, _p: usize, family: u32, count: u32, prio: *const f32 }
            let prio = 1.0f32;
            let qinfo = DevQueue { s: 0, _p: 0, family: 0, count: 1, prio: &prio };
            let mut device: u64 = 0;
            if (f.create_device)(physical, std::ptr::null(), &qinfo as *const _ as *const u8, &mut device) != 0 {
                crate::glog!("[gow.vk] vkCreateDevice failed");
                return None;
            }
            let mut queue: u64 = 0;
            (f.get_device_queue)(device, 0, 0, &mut queue);

            crate::glog!("[gow.vk] device ready: instance=0x{inst:x} physical=0x{physical:x} surface=0x{surface:x}");
            Some(VkContext { instance: inst, physical_device: physical, device, queue, queue_family: 0, surface })
        }
    }
}
