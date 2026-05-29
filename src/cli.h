#ifndef TYPIO_DAEMON_CLI_H
#define TYPIO_DAEMON_CLI_H

#include "typio/runtime/instance.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypiodOptions {
    TypioInstanceConfig instance_config;
    /** Engine directory from -E/--engine-dir, or NULL. The full
     *  engine_dirs list is assembled in the host before init. */
    const char *engine_dir_override;
    bool list_only;
    bool verbose;
} TypiodOptions;

void typiod_options_init(TypiodOptions *options);
int typiod_parse_args(TypiodOptions *options, int argc, char *argv[]);
void typiod_print_help(const char *prog);
void typiod_print_version(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_DAEMON_CLI_H */
