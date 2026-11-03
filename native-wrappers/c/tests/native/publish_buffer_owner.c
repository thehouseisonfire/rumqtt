/* Private C allocation owner, linked only by publish-buffer-proof. */
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

uint8_t *publish_buffer_fixture_allocate(size_t len) {
    uint8_t *data = malloc(len ? len : 1);
    if (!data)
        abort();
    memset(data, 42, len);
    return data;
}

void publish_buffer_fixture_release(void *userdata, const uint8_t *data, size_t len) {
    size_t *calls = userdata;
    (void)len;
    ++*calls;
    free((void *)data);
}
