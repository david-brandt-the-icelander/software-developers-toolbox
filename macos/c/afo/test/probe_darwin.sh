#!/bin/sh
# Tries linker configurations for a minimal Mach-O and reports which ones the macOS kernel will run.
cd "$(dirname "$0")/.." || exit 1
mkdir -p build/probe
cflags="-std=c11 -Os -ffreestanding -fno-builtin -fno-stack-protector -fno-asynchronous-unwind-tables"
plat="-Wl,-platform_version,macos,11.0,11.0"

try() {
	name=$1
	shift
	out=build/probe/$name
	if ! cc $cflags -o "$out" bench/exit0.c "$@" 2>"$out.err"; then
		echo "$name: link failed: $(head -c 200 "$out.err")"
		return
	fi
	codesign -f -s - "$out" 2>/dev/null
	"$out"
	echo "$name: exit status $?"
}

try control_libsystem -DAFO_LIBSYSTEM
try static_plain -static -nostdlib -Wl,-e,__start
try static_platform -static -nostdlib -Wl,-e,__start $plat
try dyld_main_no_libsystem -DAFO_LIBSYSTEM -nostdlib -Wl,-e,_main $plat
try dyld_main_no_libsystem_noplat -DAFO_LIBSYSTEM -nostdlib -Wl,-e,_main
