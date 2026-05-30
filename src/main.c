#include "app.h"
#include "cli.h"
#include "plugin_loader.h"

#include <stdio.h>

int main(int argc, char *argv[]) {
    TypioOptions options;
    TypioApp app;
    int parse_result;
    int exit_code;

    typio_options_init(&options);
    parse_result = typio_parse_args(&options, argc, argv);
    if (parse_result >= 0) {
        return parse_result;
    }

    /* The host owns plugin discovery: resolve the directory search list
     * and wire in the dlopen-based loader. Core stays platform-neutral. */
    const char *const *engine_dirs =
        typio_engine_dirs_build(options.engine_dir_override);
    options.instance_config.engine_dirs = engine_dirs;
    options.instance_config.plugin_loader = typio_plugin_load_dir;

    bool ok = typio_app_init(&app, &options.instance_config, options.verbose, argv);
    /* new_with_config copied the dir strings into the instance; safe to free. */
    typio_engine_dirs_free(engine_dirs);
    if (!ok) {
        return 1;
    }

    if (options.list_only) {
        typio_app_list_engines(&app);
        typio_app_shutdown(&app);
        return 0;
    }

    exit_code = typio_app_run(&app);
    typio_app_shutdown(&app);
    return typio_app_finish(&app, exit_code);
}
