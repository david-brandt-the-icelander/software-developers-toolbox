# Builds the afo invoker and the benchmark helpers into build/, and runs the host unit tests.
UNAME_S := $(shell uname -s)
B := build
WARN := -std=c11 -Wall -Wextra -Werror
FREE := -Os -ffreestanding -fno-builtin -fno-stack-protector -fno-asynchronous-unwind-tables -fno-strict-aliasing

ifeq ($(UNAME_S),Linux)
FREE += -fno-pie
# noseparate-code packs the binary into two LOAD segments (R+X, RW), one page fault fewer per spawn.
LINK := -static -nostdlib -no-pie -Wl,--gc-sections -Wl,-z,norelro -Wl,--build-id=none -Wl,-z,noseparate-code -s
else
# macOS rejects Mach-O executables without dyld/libSystem, so the entry is main() and only the syscalls are raw.
FREE += -DAFO_LIBSYSTEM
LINK := -Wl,-dead_strip
endif

all: $(B)/afo $(B)/exit0 $(B)/hello_libc $(B)/spawnbench

$(B):
	mkdir -p $(B)

$(B)/afo: src/afo.c src/*.h | $(B)
	$(CC) $(WARN) $(FREE) -o $@ src/afo.c $(LINK)

$(B)/exit0: bench/exit0.c src/*.h | $(B)
	$(CC) $(WARN) $(FREE) -o $@ bench/exit0.c $(LINK)

$(B)/hello_libc: bench/hello_libc.c | $(B)
	$(CC) -O2 -o $@ bench/hello_libc.c

$(B)/spawnbench: bench/spawnbench.c | $(B)
	$(CC) -O2 -o $@ bench/spawnbench.c

# Host unit tests against the mock syscall layer.
$(B)/unit: test/unit.c test/sys_mock.h src/afo_core.h src/sys.h | $(B)
	$(CC) -std=c11 -Wall -Wextra -Werror -g -O0 -DAFO_TEST -o $@ test/unit.c

unit: $(B)/unit
	$(B)/unit

# Branch coverage of the invoker logic from the unit tests, via gcov.
coverage:
	mkdir -p $(B)/cov
	$(CC) -std=c11 -g -O0 --coverage -DAFO_TEST -o $(B)/cov/unit test/unit.c
	cd $(B)/cov && ./unit && gcov -b -c unit.gcda > gcov.txt
	sh test/coverage.sh $(B)/cov/gcov.txt 95

clean:
	rm -rf $(B)

.PHONY: all clean unit coverage
