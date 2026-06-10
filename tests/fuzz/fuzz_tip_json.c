/**
 * @file fuzz_tip_json.c
 * @brief libFuzzer harness for the TIP JSON read helpers.
 *
 * tip_json.c parses bytes received from UDS clients, making it the
 * daemon's only parser of externally supplied input (see
 * docs/explanation/security-model.md). The harness exercises every
 * read-side entry point against arbitrary NUL-terminated input.
 *
 * Build with -Denable_fuzzers=true (requires clang), then run:
 *   ./build/tests/fuzz_tip_json corpus/
 */

#include "ipc/tip_json.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size);

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    /* The wire layer hands the dispatcher a NUL-terminated payload. */
    char *json = malloc(size + 1);
    if (!json)
        return 0;
    memcpy(json, data, size);
    json[size] = '\0';

    char *method = tip_json_extract_string(json, "method");
    free(method);
    char *key = tip_json_extract_string(json, "key");
    free(key);

    int id = 0;
    (void)tip_json_extract_id(json, &id);

    const char *params = NULL;
    size_t params_len = 0;
    (void)tip_json_extract_params(json, &params, &params_len);

    char *topics[8];
    int n = tip_json_extract_string_array(json, "topics", topics, 8);
    for (int i = 0; i < n; i++)
        free(topics[i]);

    free(json);
    return 0;
}
