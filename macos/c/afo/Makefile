# Builds the afo invoker, its relay variant, and the benchmark helpers into build/.
UNAME_S := $(shell uname -s)
B := build
WARN := -std=c11 -Wall -Wextra -Werror
FREE := -Os -ffreestanding -fno-builtin -fno-stack-protector -fno-asynchronous-unwind-tables -fno-strict-aliasing

ifeq ($(UNAME_S),Linux)
FREE += -fno-pie
LINK := -static -nostdlib -no-pie -Wl,--gc-sections -Wl,-z,norelro -Wl,--build-id=none -s
else
# macOS rejects Mach-O executables without dyld/libSystem, so the entry is main() and only the syscalls are raw.
FREE += -DAFO_LIBSYSTEM
LINK := -Wl,-dead_strip
endif

all: $(B)/afo $(B)/afo-relay $(B)/exit0 $(B)/hello_libc $(B)/spawnbench

$(B):
	mkdir -p $(B)

$(B)/afo: src/afo.c src/*.h | $(B)
	$(CC) $(WARN) $(FREE) -o $@ src/afo.c $(LINK)

$(B)/afo-relay: src/afo.c src/*.h | $(B)
	$(CC) $(WARN) $(FREE) -DAFO_RELAY -o $@ src/afo.c $(LINK)

$(B)/exit0: bench/exit0.c src/*.h | $(B)
	$(CC) $(WARN) $(FREE) -o $@ bench/exit0.c $(LINK)

$(B)/hello_libc: bench/hello_libc.c | $(B)
	$(CC) -O2 -o $@ bench/hello_libc.c

$(B)/spawnbench: bench/spawnbench.c | $(B)
	$(CC) -O2 -o $@ bench/spawnbench.c

clean:
	rm -rf $(B)

.PHONY: all clean
