cmake_minimum_required(VERSION 3.22)
project(gow_runtime C CXX)

# Native glue that ships inside the APK: the Turnip/adrenotools driver loader and
# (later) DXVK Native. The Rust runtime .so is produced by cargo-ndk and placed
# under jniLibs/arm64-v8a by the CI workflow.

add_library(gow_glue SHARED gow_glue.c driver_loader.c)
target_link_libraries(gow_glue log android dl)
