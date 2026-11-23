cmake_minimum_required(VERSION 3.22)
project(gow_runtime C CXX)

# The runtime .so is produced by cargo-ndk and placed under jniLibs/arm64-v8a
# by the CI workflow (cargo ndk -o android/app/src/main/jniLibs). This CMake
# file exists for any C/C++ glue (e.g. libadrenotools, DXVK Native later) and to
# satisfy the Gradle externalNativeBuild hook.

add_library(gow_glue SHARED gow_glue.c)
target_link_libraries(gow_glue log android)
