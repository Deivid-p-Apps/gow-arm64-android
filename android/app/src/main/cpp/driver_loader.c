/*
 * Deprecated: the Turnip/adrenotools driver is no longer loaded here.
 *
 * This file used to `dlopen` a driver `.so` straight into the process. That is
 * NOT how custom Adreno drivers work: the Android Vulkan loader keeps an
 * isolated (classloader) linker namespace and never enumerates a driver that was
 * merely dlopened, so the loaded Turnip was inert.
 *
 * The driver is now loaded through **libadrenotools** and routed into DXVK by the
 * Rust runtime:
 *   - runtime/src/adrenotools.rs  opens the chosen driver via
 *     adrenotools_open_libvulkan() and exposes the resulting loader's
 *     vkGetInstanceProcAddr;
 *   - runtime/src/jni.rs          nativeLoadDriver* / nativeSetDriver call it;
 *   - runtime/src/vk.rs           resolves Vulkan through that loader;
 *   - runtime/src/graphics.rs     hands the loader to both DXVK libraries via
 *     dxvk_vulkan_set_get_instance_proc_addr().
 *
 * See docs/GRAPHICS.md §"Custom driver (libadrenotools)".
 *
 * The `gow_glue` target is kept (build.gradle.kts runs externalNativeBuild) but
 * is intentionally empty; the JNI entry points ship in libgow_runtime.so.
 */
int gow_driver_loader_placeholder(void) { return 0; }
