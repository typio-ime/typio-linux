#ifndef TYPIO_DAEMON_CLI_H
#define TYPIO_DAEMON_CLI_H

#include "typio/runtime/instance.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioOptions {
    TypioInstanceConfig instance_config;
    /** Engine directory from -E/--engine-dir, or NULL. The full
     *  engine_dirs list is assembled in the host before init. */
    const char *engine_dir_override;
    bool verbose;
} TypioOptions;

void typio_options_init(TypioOptions *options);
int typio_parse_args(TypioOptions *options, int argc, char *argv[]);
void typio_print_help(const char *prog);
void typio_print_version(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_DAEMON_CLI_H */
