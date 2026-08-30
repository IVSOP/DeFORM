# C test for the pong FFI bindings.
#
#   cargo build -p pong_ffi
#   make -C examples/pong_ffi/c run
#
# The rpath is baked in so ./smoke finds libpong_ffi.so without LD_LIBRARY_PATH.
# Override TARGET to link a release build: `make TARGET=../../../target/release`.

CRATES := ../../..
TARGET ?= $(CRATES)/target/debug

CFLAGS += -std=gnu11 -Wall -Wextra -I ../include -I $(CRATES)/deform_ffi/include
LDFLAGS += -L $(TARGET) -Wl,-rpath,$(abspath $(TARGET))
LDLIBS += -lpong_ffi

# Explicit recipe: the built-in `%: %.c` rule needs matching stems, and the binary
# is `smoke` while the source is `ffi_test.c`.
smoke: ffi_test.c
	$(CC) $(CFLAGS) $(LDFLAGS) $< $(LDLIBS) -o $@

run: smoke
	./smoke

clean:
	rm -f smoke

.PHONY: run clean
