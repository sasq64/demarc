cmake_minimum_required(VERSION 3.5)
project(cbmconvert)

add_executable(cbmconvert
    archive.c c2n.c image.c lynx.c main.c
    read.c t64.c unarc.c unark.c util.c write.c)

add_executable(zip2disk zip2disk.c)
add_executable(disk2zip disk2zip.c)
