# librife_demarc: rife-ncnn-vulkan behind a small C API, for crates/rife.
cmake_minimum_required(VERSION 3.9)
project(rife_demarc)

set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(RIFE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../rife-ncnn-vulkan/src)
add_subdirectory(${RIFE_DIR} rife EXCLUDE_FROM_ALL)

add_library(rife_demarc SHARED rife_c.cpp ${RIFE_DIR}/rife.cpp ${RIFE_DIR}/warp.cpp)
add_dependencies(rife_demarc generate-spirv)
target_include_directories(rife_demarc PRIVATE ${RIFE_DIR} ${CMAKE_CURRENT_BINARY_DIR}/rife)
target_link_libraries(rife_demarc ncnn ${Vulkan_LIBRARY})
set_target_properties(rife_demarc PROPERTIES CXX_VISIBILITY_PRESET hidden)
