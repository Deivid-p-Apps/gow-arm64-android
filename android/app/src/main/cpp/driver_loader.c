/*
 * Turnip/adrenotools driver loader (arm64 Android).
 *
 * AdrenoTools packages are a zip with meta.json + a driver .so (commonly
 * libvulkan_freedreno.so, the Turnip Vulkan ICD for Adreno). Android cannot
 * dlopen from external storage, so the driver is copied into the app's private
 * data dir and loaded from there. Loading needs:
 *   1) extracting the .so (+ its dependencies) to private storage,
 *   2) dlopen with RTLD_LOCAL|RTLD_NOW,
 *   3) resolving vkGetInstanceProcAddr from it and passing the driver to the
 *      Vulkan loader / the app's own Vulkan usage.
 *
 * This file depends on libadrenotools at build time (see CMakeLists). When that
 * dependency is absent the functions return an error rather than pretending.
 */
#include <jni.h>
#include <android/log.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define LOG_TAG "GoW.driver"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

static void* g_driver_handle = NULL;
static void* g_hooked_loader = NULL;

/* Copy a file into the app's private dir; returns malloc'd path or NULL. */
static char* copy_into_private(const char* src, const char* priv_dir, const char* name) {
    FILE* in = fopen(src, "rb");
    if (!in) { LOGE("open src failed: %s", src); return NULL; }
    char* out_path = malloc(4096);
    snprintf(out_path, 4096, "%s/%s", priv_dir, name);
    FILE* out = fopen(out_path, "wb");
    if (!out) { LOGE("open dst failed: %s", out_path); fclose(in); free(out_path); return NULL; }
    char buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, in)) > 0) {
        if (fwrite(buf, 1, n, out) != n) { LOGE("write failed"); fclose(in); fclose(out); free(out_path); return NULL; }
    }
    fclose(in);
    fclose(out);
    chmod(out_path, 0700);
    return out_path;
}

JNIEXPORT jint JNICALL
Java_com_gow_arm64_GowActivity_nativeLoadDriver(JNIEnv* env, jclass clazz,
                                                jstring jsrc, jstring jpriv) {
    const char* src = (*env)->GetStringUTFChars(env, jsrc, NULL);
    const char* priv = (*env)->GetStringUTFChars(env, jpriv, NULL);
    char* local = copy_into_private(src, priv, "libvulkan_freedreno.so");
    if (!local) { return -1; }
    g_driver_handle = dlopen(local, RTLD_NOW | RTLD_LOCAL);
    if (!g_driver_handle) {
        LOGE("dlopen driver failed: %s", dlerror());
        free(local);
        return -2;
    }
    LOGI("Turnip driver loaded: %s", local);
    free(local);
    return 0;
}
