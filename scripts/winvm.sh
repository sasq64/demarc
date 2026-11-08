#!/usr/bin/env bash
# Build and run demarc natively in the Omarchy Windows VM over SSH.
# One-time VM setup: run \\host.lan\Data\setup-dev.ps1 in the VM (see ~/Windows).
#
#   scripts/winvm.sh sync              copy the source to C:\dev\demarc
#   scripts/winvm.sh build [args]      sync + cargo build --profile release-fast
#   scripts/winvm.sh cargo <args>      sync + any cargo command
#   scripts/winvm.sh run <args>        run demarc.exe; local path arguments are
#                                      uploaded, files the run writes come back
#                                      to ./winvm-out
#   scripts/winvm.sh gui <args>        like run, but in the logged-in RDP
#                                      session, so windows and audio work
#   scripts/winvm.sh ssh [cmd]         shell (cmd.exe) in the VM
set -euo pipefail

HOST="${WINVM_HOST:-winvm}"
PROFILE="${PROFILE:-release-fast}"
SRC='C:\dev\demarc'
RUN='C:\dev\run'
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

win() { ssh -o BatchMode=yes "$HOST" "$@"; }

sync() {
    win "if not exist $SRC mkdir $SRC"
    cd "$ROOT"
    { git ls-files -z; git ls-files -z -o --exclude-standard -- src crates system scripts testdata files; } |
        tar -c --null -T - -f - | win "tar -xf - -C ${SRC//\\//}"
}

cmd="${1:-}"
shift || true
case "$cmd" in
sync) sync ;;
build)
    sync
    win "cd /d $SRC && cargo build --profile $PROFILE $*"
    ;;
cargo)
    sync
    win "cd /d $SRC && cargo $*"
    ;;
run | gui)
    win "(if exist $RUN rmdir /s /q $RUN) & mkdir $RUN\\in"
    args=()
    for a in "$@"; do
        pre="" p="$a"
        [[ $a == --*=* ]] && pre="${a%%=*}=" p="${a#*=}"
        if [ -e "$p" ]; then
            full=$(realpath "$p")
            tar -c -C "$(dirname "$full")" -f - "$(basename "$full")" | win "tar -xf - -C ${RUN//\\//}/in"
            args+=("$pre\"in\\$(basename "$full")\"")
        else
            args+=("$a")
        fi
    done
    exe="$SRC\\target\\$PROFILE\\demarc.exe ${args[*]}"
    env="set RUST_LOG=${RUST_LOG:-info}&& set RUST_BACKTRACE=1&&"
    rc=0
    if [ "$cmd" = run ]; then
        win "cd /d $RUN && $env $exe" || rc=$?
    else
        # SSH sessions have no desktop; a scheduled task with /it runs in the
        # logged-in (RDP) session instead.
        printf '@cd /d %s\r\n@%s %s > log.txt 2>&1\r\n@echo exit=%%errorlevel%% > done.txt\r\n' "$RUN" "$env" "$exe" |
            win "powershell -c \"\$input | Set-Content $RUN\\go.bat\""
        win "schtasks /create /tn demarc-run /tr $RUN\\go.bat /sc once /st 23:59 /it /f >nul && schtasks /run /tn demarc-run >nul"
        until win "if exist $RUN\\done.txt (exit 0) else (exit 1)"; do sleep 2; done
        win "type $RUN\\log.txt"
        win "type $RUN\\done.txt"
    fi
    mkdir -p winvm-out
    win "tar -cf - -C ${RUN//\\//} --exclude ./in ." | tar -x -C winvm-out
    exit $rc
    ;;
ssh) ssh "$HOST" "$@" ;;
*)
    sed -n '2,14p' "$0"
    exit 1
    ;;
esac
