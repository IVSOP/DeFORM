/* C test for the pong FFI bindings: drives the offline backend end to end.
 * Build and run it with `make run` (see the Makefile next to this file).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "pong_ffi.h"

typedef DeformByteBuffer ByteBuffer;

static ByteBuffer buf(const char *s) {
  ByteBuffer b = {(uint8_t *)s, strlen(s)};
  return b;
}

/* prints and frees */
static void show(const char *label, ByteBuffer b) {
  printf("%s: %.*s\n\n", label, (int)b.size, b.ptr);
  deform_free_bytes(b.ptr, b.size);
}

/* pulls the integer out of {"data":<n>} */
static void *handle_of(ByteBuffer b) {
  char *copy = strndup((char *)b.ptr, b.size);
  char *d = strstr(copy, "\"data\":");
  void *h = d ? (void *)(uintptr_t)strtoull(d + 7, NULL, 10) : NULL;
  free(copy);
  return h;
}

int main(void) {
  const char *me = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
  const char *bot = "1nc1nerator11111111111111111111111111111111";
  char players[256];
  snprintf(players, sizeof players, "[\"%s\",\"%s\"]", me, bot);

  /* 1. a binding per backend, returning a leaked pointer or an error */
  ByteBuffer init = pong_new_offline_client(buf(me), buf(players), 0, 16667);
  printf("init: %.*s\n\n", (int)init.size, init.ptr);
  void *client = handle_of(init);
  deform_free_bytes(init.ptr, init.size);
  if (!client) {
    printf("init failed\n");
    return 1;
  }

  /* 3. set_inputs from json */
  show("set_inputs", pong_set_inputs(client, buf("{\"direction\":1}")));
  show(
      "set_inputs bad json",
      pong_set_inputs(client, buf("{\"direction\":\"up\"}")));

  usleep(200000);

  /* 2. print the state as json */
  show("read_state", pong_read_state(client));

  /* error paths */
  show("read_state(NULL)", pong_read_state(NULL));

  show("shutdown", pong_shutdown(client));
  pong_free_client(client);
  printf("freed, no crash\n");
  return 0;
}
