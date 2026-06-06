#include "app.h"
#include "cli.h"
#include "engine_loader.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char *argv[]) {
    TypioOptions options;
    TypioApp app;
    int parse_result;
    int exit_code;

    typio_options_init(&options);
    parse_result = typio_parse_args(&options, argc, argv);
    if (parse_result >= 0) {
        free((void *)options.engine_dirs);
        return parse_result;
    }

    /* The host owns engine manifest discovery. Core stays platform-neutral. */
    const char *const *engine_dirs =
        typio_engine_dirs_build(options.engine_dirs, options.engine_dir_count);
    options.instance_config.engine_dirs = engine_dirs;
    options.instance_config.plugin_loader = typio_engine_loader_load_dir;

    bool ok = typio_app_init(&app, &options.instance_config, options.verbose, argv);
    /* new_with_config copied the dir strings into the instance; safe to free. */
    typio_engine_dirs_free(engine_dirs);
    /* Free the -E accumulator array; its entries are borrowed argv pointers. */
    free((void *)options.engine_dirs);
    if (!ok) {
        return 1;
    }

    exit_code = typio_app_run(&app);
    typio_app_shutdown(&app);
    return typio_app_finish(&app, exit_code);
}
