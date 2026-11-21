# Builds klv_dump against an installed mpegts (`make install`):
#
#   cmake -S examples/c -B build -DCMAKE_PREFIX_PATH=/usr/local
#   cmake --build build
#
# -DMPEGTS_STATIC=ON links the static library instead.
cmake_minimum_required(VERSION 3.10)
project(mpegts_examples C)

option(MPEGTS_STATIC "Link mpegts statically" OFF)
find_package(mpegts 0.2 REQUIRED)

add_executable(klv_dump klv_dump.c)
if(MPEGTS_STATIC)
  target_link_libraries(klv_dump PRIVATE mpegts::mpegts_static)
else()
  target_link_libraries(klv_dump PRIVATE mpegts::mpegts)
endif()
