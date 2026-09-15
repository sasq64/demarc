#!/usr/bin/env bash
# Build the two disc images a PCem machine config needs to run DOS content:
# a FreeDOS boot floppy, and a hard disc holding the program itself.
#
#     scripts/make-pc-disks.sh boot  testdata/pc/fdboot.img
#     scripts/make-pc-disks.sh data  testdata/pc/2ndreality.img testdata/pc/2ndreality/
#
# The images are committed, so this is only run when they need regenerating.
# It is here because a binary blob nobody can rebuild is a liability, and
# because the memory layout below is the whole reason the demo runs at all.
#
# The boot floppy is FreeDOS 1.3, cut down to the kernel, the shell and JEMMEX.
# JEMMEX matters twice over: DOS=HIGH,UMB puts the kernel, the buffers and the
# resident shell out of the way -- without it a 640K machine has only ~506K
# free, and DOS-era demos routinely want more -- and it provides the EMS that a
# Sound Blaster mixdown usually needs on top. It boots to C:\ and runs
# C:\AUTORUN.BAT if the hard disc has one, so the floppy stays content-agnostic
# and every PC config in testdata/ can share it.
#
# Needs mtools. FreeDOS is GPLv2 and redistributable; see DOC/KERNEL/COPYING in
# the kernel package.
set -euo pipefail

FD_EDITION="https://www.ibiblio.org/pub/micro/pc-stuff/freedos/files/distributions/1.3/official/FD13-FloppyEdition.zip"
FD_JEMM="https://www.ibiblio.org/pub/micro/pc-stuff/freedos/files/repositories/1.3/base/jemm.zip"

# Geometry of the generated hard disc. The .cfg has to repeat these as
# hdc_cylinders / hdc_heads / hdc_sectors -- PCem takes the geometry from the
# config, not from the image, and gets a corrupt-looking disc if they disagree.
CYLINDERS=120
HEADS=4
SECTORS=17

usage() {
    echo "usage: $0 boot <out.img>" >&2
    echo "       $0 data <out.img> <dir>" >&2
    exit 2
}

fetch_freedos() {
    work="$1"
    curl --proto '=https' --tlsv1.2 -fLsS --retry 3 -o "$work/floppies.zip" "$FD_EDITION"
    curl --proto '=https' --tlsv1.2 -fLsS --retry 3 -o "$work/jemm.zip" "$FD_JEMM"
    # 720k/x86BOOT.img is the smallest official bootable image, and the source
    # of the one thing that cannot be reconstructed with mtools: a FAT12 boot
    # sector that knows how to load KERNEL.SYS.
    unzip -qoj "$work/floppies.zip" '720k/x86BOOT.img' -d "$work"
    unzip -qoj "$work/jemm.zip" 'BIN/JEMMEX.EXE' -d "$work"
    mcopy -i "$work/x86BOOT.img" -D o \
        ::/freedos/bin/kernl386.sys ::/freedos/bin/command.com \
        ::/freedos/bin/mem.exe "$work/"
}

build_boot() {
    out="$1"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN
    fetch_freedos "$work"

    # Format from scratch rather than editing the FreeDOS image in place, so
    # every sector we do not use is a zero and the committed image compresses.
    # mformat writes the BPB; the FreeDOS jump, OEM name and boot code are
    # transplanted around it, which is safe because the boot code reads the
    # geometry out of that same BPB.
    rm -f "$out"
    dd if=/dev/zero of="$out" bs=1024 count=1440 status=none
    mformat -i "$out" -f 1440 -v FDBOOT ::
    dd if="$work/x86BOOT.img" of="$out" bs=1 count=11 conv=notrunc status=none
    dd if="$work/x86BOOT.img" of="$out" bs=1 skip=62 seek=62 count=450 conv=notrunc status=none

    printf 'DEVICE=A:\\JEMMEX.EXE NOVME NOINVLPG\r\nDOS=HIGH,UMB\r\n!LASTDRIVE=C\r\n!BUFFERS=10\r\n!FILES=10\r\nSHELLHIGH=A:\\COMMAND.COM A:\\ /E:256 /P\r\n' \
        > "$work/FDCONFIG.SYS"
    printf '@ECHO OFF\r\nSET COMSPEC=A:\\COMMAND.COM\r\nPROMPT $P$G\r\nC:\r\nCD \\\r\nIF EXIST AUTORUN.BAT CALL AUTORUN.BAT\r\n' \
        > "$work/AUTOEXEC.BAT"

    # KERNEL.SYS first, so it lands at the start of the data area contiguously.
    mcopy -i "$out" "$work/kernl386.sys" ::/KERNEL.SYS
    mcopy -i "$out" "$work/command.com" ::/COMMAND.COM
    mcopy -i "$out" "$work/JEMMEX.EXE" ::/JEMMEX.EXE
    mcopy -i "$out" "$work/mem.exe" ::/MEM.EXE
    mcopy -i "$out" "$work/FDCONFIG.SYS" "$work/AUTOEXEC.BAT" ::
    mdir -i "$out" ::
}

build_data() {
    out="$1"
    dir="$2"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN

    rm -f "$out"
    dd if=/dev/zero of="$out" bs=512 count=$((CYLINDERS * HEADS * SECTORS)) status=none
    cat > "$work/mtoolsrc" <<EOF
drive c: file="$(realpath "$out")" partition=1 \
    cylinders=$CYLINDERS heads=$HEADS sectors=$SECTORS mformat_only
EOF
    export MTOOLSRC="$work/mtoolsrc"
    mpartition -I c:
    mpartition -c -a c:
    mformat c:
    mlabel c:DOS
    # -s to take subdirectories; the shell expands the glob so dotfiles stay out.
    mcopy -s "$dir"/* c:
    mdir c:
}

[ $# -ge 2 ] || usage
case "$1" in
    boot) [ $# -eq 2 ] || usage; build_boot "$2" ;;
    data) [ $# -eq 3 ] || usage; build_data "$2" "$3" ;;
    *) usage ;;
esac
