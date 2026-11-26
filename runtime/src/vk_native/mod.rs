//! Native Vulkan backend (alternative to DXVK Native).
//!
//! DXVK is refused on some devices/Turnip builds ("Device does not support
//! required feature 'storageBuffer8BitAccess'" -> `CreateDXGIFactory1` fails
//! with `0x80004005`). This module is a *parallel* renderer that talks Vulkan
//! directly from Rust, so the port can keep DXVK as the default and fall back
//! with a single env var:
//!
//! ```text
//! GOW_GFX_BACKEND=dxvk    (default)  -> the DXVK `.so`s, exactly as today
//! GOW_GFX_BACKEND=native             -> this module
//! ```
//!
//! Nothing here touches `graphics.rs`/`win32.rs`: the backend is selected by
//! [`Backend::from_env`], and the two call sites in those files (documented in
//! the delivery report) only need to branch on it.
//!
//! Scope of this first cut: a *real* headless-capable Vulkan bring-up
//! (instance -> physical device -> device -> queue), a `VkSwapchainKHR` built
//! from an `ANativeWindow*` (or a headless surface for tests) with 2-3 images,
//! and a present loop that acquires an image, clears it red through a render
//! pass, submits and presents. The D3D11 `device`/`context` schematics live in
//! [`d3d11`] as clearly-marked stubs.
//!
//! All entry points are resolved through the `vkGetInstanceProcAddr` the
//! adrenotools loader hands us when a custom driver is active (see
//! `runtime/src/adrenotools.rs`); otherwise the system `libvulkan.so` is used.
//! `ash`'s `loaded` feature provides exactly the `dlopen`-based path for the
//! system loader.

use std::ffi::c_void;

pub mod bridge;
pub mod d3d11;
pub mod d3d11_objects;
pub mod render;
pub mod scene;
pub mod selftest;
pub mod swapchain;
pub mod x11;

/// Which renderer the port should use. Selected once from `GOW_GFX_BACKEND`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// DXVK Native `.so`s (phase 1, current default).
    Dxvk,
    /// This native Rust Vulkan renderer.
    Native,
}

impl Backend {
    /// Read `GOW_GFX_BACKEND`; default is `Dxvk` (do not change behaviour unless
    /// the user opts in). Anything that is not `native`/`dxvk` keeps the
    /// default and is logged once by the caller.
    pub fn from_env() -> Self {
        match std::env::var("GOW_GFX_BACKEND") {
            Ok(v) => Self::parse(&v),
            Err(_) => Backend::Dxvk,
        }
    }

    /// Pure selector, so the mapping is unit-testable without touching the
    /// process environment. Default (and anything unrecognised) is DXVK.
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("native") {
            Backend::Native
        } else {
            if !value.eq_ignore_ascii_case("dxvk") {
                crate::glog!("[gow.vk.native] unknown GOW_GFX_BACKEND={value:?}; using dxvk");
            }
            Backend::Dxvk
        }
    }

    pub fn is_native(self) -> bool {
        matches!(self, Backend::Native)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_selection() {
        assert_eq!(Backend::parse("native"), Backend::Native);
        assert_eq!(Backend::parse("NATIVE"), Backend::Native);
        assert_eq!(Backend::parse("dxvk"), Backend::Dxvk);
        // Default for anything else: DXVK (never silently switch renderer).
        assert_eq!(Backend::parse(""), Backend::Dxvk);
        assert_eq!(Backend::parse("vulkan"), Backend::Dxvk);
        assert!(Backend::Native.is_native());
        assert!(!Backend::Dxvk.is_native());
    }
}

/// The queue families we picked at device creation.
#[derive(Clone, Copy, Debug)]
pub struct QueueFamilies {
    pub graphics: u32,
    pub present: u32,
}

/// Whether the created surface is backed by a real Android window or is the
/// headless test surface. Recorded for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Android,
    Headless,
}

/// A live Vulkan instance + physical device + logical device + graphics queue.
///
/// This is the native analogue of `vk::VkContext`, but self-contained: it owns
/// the `ash::Entry` (so the loader stays open), the instance, the device and
/// the queue. It is `Send + Sync` (ash handles/fn tables are), so it can live in
/// a process-global for the import resolver.
pub struct NativeDevice {
    entry: ash::Entry,
    pub instance: ash::Instance,
    pub physical_device: ash::vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: ash::vk::Queue,
    pub families: QueueFamilies,
    pub surface: ash::vk::SurfaceKHR,
    pub surface_kind: SurfaceKind,
    surface_loader: ash::khr::surface::Instance,
    pub device_name: String,
    pub api_version: u32,
    /// Vulkan-1.2 `storageBuffer8BitAccess`, the feature DXVK demanded and the
    /// driver we run on reported as missing. Logged at bring-up so the diagnosis
    /// travels with every session.
    pub storage_buffer_8bit: bool,
}

/// Build an `ash::Entry` from an externally supplied `vkGetInstanceProcAddr`
/// (the pointer libadrenotools returns for the chosen driver). `gipa == 0`
/// yields `None` so the caller can fall back to the system loader.
unsafe fn entry_from_gipa(gipa: usize) -> Option<ash::Entry> {
    if gipa == 0 {
        return None;
    }
    let f: ash::vk::PFN_vkGetInstanceProcAddr = std::mem::transmute(gipa);
    Some(ash::Entry::from_static_fn(ash::StaticFn {
        get_instance_proc_addr: f,
    }))
}

/// Locate a Vulkan entry point: the custom-driver loader when adrenotools opened
/// one, else the system `libvulkan.so` (`ash::Entry::load`).
pub unsafe fn load_entry() -> Option<ash::Entry> {
    if let Some(gipa) = crate::adrenotools::instance_proc_addr() {
        crate::glog!("[gow.vk.native] entry via adrenotools loader gipa=0x{gipa:x}");
        return entry_from_gipa(gipa);
    }
    match ash::Entry::load() {
        Ok(e) => {
            crate::glog!("[gow.vk.native] entry via system libvulkan.so");
            Some(e)
        }
        Err(e) => {
            crate::glog!("[gow.vk.native] Entry::load failed: {e:?}");
            None
        }
    }
}

fn cstr_name_contains(
    exts: &[ash::vk::ExtensionProperties],
    name: &std::ffi::CStr,
) -> bool {
    exts.iter().any(|e| {
        // `extension_name_as_c_str` exists on ash's ExtensionProperties.
        e.extension_name_as_c_str().map(|c| c == name).unwrap_or(false)
    })
}

impl NativeDevice {
    /// Bring up Vulkan using an externally supplied `vkGetInstanceProcAddr`.
    ///
    /// This is the *exact* contract with libadrenotools: the loader it opens
    /// gives us one `vkGetInstanceProcAddr`, and every entry point must come
    /// from it so the instance is backed by the chosen driver (Turnip), not the
    /// system loader. A raw ICD's `vk_icdGetInstanceProcAddr` works here too, so
    /// the driver path can be tested off-device (see the probe example).
    ///
    /// # Safety
    /// `gipa` must be a valid `vkGetInstanceProcAddr` that stays alive for the
    /// lifetime of the returned device.
    pub unsafe fn new_with_gipa(gipa: usize, window: *mut c_void) -> Option<Self> {
        Self::bring_up(entry_from_gipa(gipa)?, window)
    }

    /// Bring up Vulkan for `window`.
    ///
    /// * `window != null` -> `VK_KHR_surface` + `VK_KHR_android_surface`
    ///   (`vkCreateAndroidSurfaceKHR`), the real on-device path.
    /// * `window == null` -> `VK_KHR_surface` + `VK_EXT_headless_surface`, used
    ///   by the Termux harness (no `ANativeWindow` outside an Android app).
    pub fn new(window: *mut c_void) -> Option<Self> {
        let entry = unsafe { load_entry()? };
        Self::bring_up(entry, window)
    }

    /// Shared bring-up: instance -> physical device -> device -> queue -> surface.
    fn bring_up(entry: ash::Entry, window: *mut c_void) -> Option<Self> {
        let inst_exts = unsafe { entry.enumerate_instance_extension_properties(None) }.ok()?;
        let want_android = !window.is_null();
        let surface_ext = if want_android {
            ash::vk::KHR_ANDROID_SURFACE_NAME
        } else {
            ash::vk::EXT_HEADLESS_SURFACE_NAME
        };
        if !cstr_name_contains(&inst_exts, ash::vk::KHR_SURFACE_NAME) {
            crate::glog!("[gow.vk.native] driver lacks VK_KHR_surface");
            return None;
        }
        if !cstr_name_contains(&inst_exts, surface_ext) {
            crate::glog!(
                "[gow.vk.native] driver lacks {} (android={want_android})",
                surface_ext.to_string_lossy()
            );
            return None;
        }

        let mut ext_ptrs: Vec<*const std::os::raw::c_char> = vec![
            ash::vk::KHR_SURFACE_NAME.as_ptr(),
            surface_ext.as_ptr(),
        ];
        // VK_KHR_get_physical_device_properties2 is core in 1.1; request it only
        // if the instance is likely older. We ask for api 1.1 and use core 1.1
        // entry points, so nothing extra is required here.
        let _ = &mut ext_ptrs;

        let app_name = std::ffi::CString::new("God of War (gow-arm64)").unwrap();
        let app_info = ash::vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .engine_name(std::ffi::CStr::from_bytes_with_nul(b"gow-native\0").unwrap())
            .engine_version(1)
            .api_version(ash::vk::make_api_version(0, 1, 1, 0));
        let inst_ci = ash::vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&ext_ptrs);
        let instance = unsafe {
            match entry.create_instance(&inst_ci, None) {
                Ok(i) => i,
                Err(e) => {
                    crate::glog!("[gow.vk.native] vkCreateInstance failed: {e:?}");
                    return None;
                }
            }
        };

        let devices = unsafe { instance.enumerate_physical_devices() }.ok()?;
        if devices.is_empty() {
            crate::glog!("[gow.vk.native] no physical devices");
            return None;
        }
        // Prefer a discrete/integrated GPU by name; fall back to the first.
        let physical_device = devices[0];
        let props = unsafe { instance.get_physical_device_properties(physical_device) };
        let device_name = unsafe {
            std::ffi::CStr::from_ptr(props.device_name.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        let api_version = props.api_version;
        crate::glog!(
            "[gow.vk.native] device {device_name:?} api={}.{}.{}",
            api_version >> 22,
            (api_version >> 12) & 0x3ff,
            api_version & 0xfff
        );

        // storageBuffer8BitAccess: the exact feature DXVK rejects the adapter for.
        let storage_buffer_8bit = {
            let mut f12 = ash::vk::PhysicalDeviceVulkan12Features::default();
            let mut f2 = ash::vk::PhysicalDeviceFeatures2::default().push_next(&mut f12);
            unsafe { instance.get_physical_device_features2(physical_device, &mut f2) };
            f12.storage_buffer8_bit_access == ash::vk::TRUE
        };
        crate::glog!("[gow.vk.native] storageBuffer8BitAccess={storage_buffer_8bit}");

        // Surface.
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let (surface, surface_kind) = if want_android {
            let loader = ash::khr::android_surface::Instance::new(&entry, &instance);
            let ci = ash::vk::AndroidSurfaceCreateInfoKHR::default()
                .window(window as *mut ash::vk::ANativeWindow);
            match unsafe { loader.create_android_surface(&ci, None) } {
                Ok(s) => (s, SurfaceKind::Android),
                Err(e) => {
                    crate::glog!("[gow.vk.native] vkCreateAndroidSurfaceKHR failed: {e:?}");
                    return None;
                }
            }
        } else {
            let loader = ash::ext::headless_surface::Instance::new(&entry, &instance);
            let ci = ash::vk::HeadlessSurfaceCreateInfoEXT::default();
            match unsafe { loader.create_headless_surface(&ci, None) } {
                Ok(s) => (s, SurfaceKind::Headless),
                Err(e) => {
                    crate::glog!("[gow.vk.native] vkCreateHeadlessSurfaceEXT failed: {e:?}");
                    return None;
                }
            }
        };

        // Queue family with graphics (and present, when the surface can present).
        let qfams = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let mut graphics = None;
        let mut present = None;
        for (i, q) in qfams.iter().enumerate() {
            let i = i as u32;
            if q.queue_flags.contains(ash::vk::QueueFlags::GRAPHICS) && graphics.is_none() {
                graphics = Some(i);
            }
            let can_present = unsafe {
                surface_loader
                    .get_physical_device_surface_support(physical_device, i, surface)
                    .unwrap_or(false)
            };
            if can_present && present.is_none() {
                present = Some(i);
            }
        }
        let graphics = match graphics {
            Some(g) => g,
            None => {
                crate::glog!("[gow.vk.native] no graphics queue family");
                return None;
            }
        };
        let present = present.unwrap_or(graphics);
        crate::glog!("[gow.vk.native] queue families graphics={graphics} present={present}");

        // Device extensions: swapchain (needed once we present).
        let dexts =
            unsafe { instance.enumerate_device_extension_properties(physical_device) }.ok()?;
        if !cstr_name_contains(&dexts, ash::vk::KHR_SWAPCHAIN_NAME) {
            crate::glog!("[gow.vk.native] driver lacks VK_KHR_swapchain");
            return None;
        }
        let dev_ext_ptrs: [*const std::os::raw::c_char; 1] =
            [ash::vk::KHR_SWAPCHAIN_NAME.as_ptr()];
        // One queue for both families when they coincide, else two.
        let mut families: Vec<u32> = vec![graphics];
        if present != graphics {
            families.push(present);
        }
        let prio = [1.0f32];
        let q_infos: Vec<ash::vk::DeviceQueueCreateInfo> = families
            .iter()
            .map(|&f| {
                ash::vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(f)
                    .queue_priorities(&prio)
            })
            .collect();
        let dev_ci = ash::vk::DeviceCreateInfo::default()
            .queue_create_infos(&q_infos)
            .enabled_extension_names(&dev_ext_ptrs);
        let device = unsafe {
            match instance.create_device(physical_device, &dev_ci, None) {
                Ok(d) => d,
                Err(e) => {
                    crate::glog!("[gow.vk.native] vkCreateDevice failed: {e:?}");
                    return None;
                }
            }
        };
        let queue = unsafe { device.get_device_queue(graphics, 0) };
        crate::glog!("[gow.vk.native] device ready (surface={surface_kind:?})");

        Some(NativeDevice {
            entry,
            instance,
            physical_device,
            device,
            queue,
            families: QueueFamilies { graphics, present },
            surface,
            surface_kind,
            surface_loader,
            device_name,
            api_version,
            storage_buffer_8bit,
        })
    }

    /// The loader handle, for building extension objects (swapchain) elsewhere.
    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    pub fn surface_loader(&self) -> &ash::khr::surface::Instance {
        &self.surface_loader
    }

    /// Create a swapchain on this device's surface. `extent` is a hint for
    /// headless surfaces (Android surfaces report their own `currentExtent`).
    pub fn create_swapchain(
        &self,
        preferred_extent: ash::vk::Extent2D,
        vsync: bool,
    ) -> Option<swapchain::Swapchain> {
        swapchain::Swapchain::new(self, preferred_extent, vsync)
    }
}
