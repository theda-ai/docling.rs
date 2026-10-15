# CMake toolchain for cross-compiling to IBM Z (s390x-unknown-linux-gnu) with
# zig as the C/C++ compiler and linker — what scripts/install/build_onnxruntime_s390x.sh
# builds ONNX Runtime with by default (#504). zig targets glibc 2.28 directly
# and links LLVM's libc++ statically, so the library runs on RHEL 8/9 era
# mainframe Linux; the distro gcc cross toolchain (s390x-linux-gnu.toolchain.cmake)
# pins it to its own glibc 2.39 / GCC 13 libstdc++ instead. The build script
# writes the `zig cc`/`zig c++` wrapper scripts into ZIG_WRAPPER_DIR — they add
# `-target s390x-linux-gnu.2.28 -mcpu=z15` and enable the vector facility
# (+vector, +vector-enhancements-1/2) explicitly: MLAS's s390x kernels are
# built for z15 (`-march=z15`, which zig ignores while force-disabling the
# vector features of its own CPU model).
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR s390x)
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES ZIG_WRAPPER_DIR)
if(NOT ZIG_WRAPPER_DIR)
  message(FATAL_ERROR "ZIG_WRAPPER_DIR is not set — run scripts/install/build_onnxruntime_s390x.sh, which generates the zig wrappers")
endif()
set(CMAKE_C_COMPILER "${ZIG_WRAPPER_DIR}/zig-cc")
set(CMAKE_CXX_COMPILER "${ZIG_WRAPPER_DIR}/zig-cxx")
set(CMAKE_AR "${ZIG_WRAPPER_DIR}/zig-ar")
set(CMAKE_RANLIB "${ZIG_WRAPPER_DIR}/zig-ranlib")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(CMAKE_CROSSCOMPILING_EMULATOR "qemu-s390x-static;-L;/usr/s390x-linux-gnu")
# Eigen's ZVector kernels: see s390x-linux-gnu.toolchain.cmake.
set(CMAKE_CXX_FLAGS_INIT "-DEIGEN_DONT_VECTORIZE")
