# CMake toolchain for cross-compiling to IBM Z (s390x-unknown-linux-gnu) from
# an x86_64 Linux host with the distro GNU cross toolchain
# (`apt-get install gcc-s390x-linux-gnu g++-s390x-linux-gnu`) — what
# scripts/install/build_onnxruntime_s390x.sh and the onnxruntime-s390x.yml
# workflow build ONNX Runtime with (#504).
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR s390x)
set(CMAKE_C_COMPILER s390x-linux-gnu-gcc)
set(CMAKE_CXX_COMPILER s390x-linux-gnu-g++)
set(CMAKE_AR s390x-linux-gnu-ar)
set(CMAKE_RANLIB s390x-linux-gnu-ranlib)
set(CMAKE_STRIP s390x-linux-gnu-strip)
set(CMAKE_FIND_ROOT_PATH /usr/s390x-linux-gnu)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
# qemu-user runs target binaries when cmake asks for one (try_run); the
# library build itself never does.
set(CMAKE_CROSSCOMPILING_EMULATOR "qemu-s390x-static;-L;/usr/s390x-linux-gnu")
# Eigen's ZVector (z13 vector facility) kernels do not compile with GCC 13
# (Complex.h: brace-init to `__vector`, `plog<Packet1cd>` template mismatch —
# onnxruntime#32475). ONNX Runtime's s390x SIMD lives in MLAS, so Eigen runs
# its scalar paths.
set(CMAKE_CXX_FLAGS_INIT "-DEIGEN_DONT_VECTORIZE")
