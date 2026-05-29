#include "app.h"
#include "cli.h"
#include "plugin_loader.h"

#include <stdio.h>

int main(int argc, char *argv[]) {
    TypiodOptions options;
    TypiodApp app;
    int parse_result;
    int exit_code;

    typiod_options_init(&options);
    parse_result = typiod_parse_args(&options, argc, argv);
    if (parse_result >= 0) {
        return parse_result;
    }

    /* The host owns plugin discovery: resolve the directory search list
     * and wire in the dlopen-based loader. Core stays platform-neutral. */
    const char *const *engine_dirs =
        typiod_engine_dirs_build(options.engine_dir_override);
    options.instance_config.engine_dirs = engine_dirs;
    options.instance_config.plugin_loader = typiod_plugin_load_dir;

    bool ok = typiod_app_init(&app, &options.instance_config, options.verbose, argv);
    /* new_with_config copied the dir strings into the instance; safe to free. */
    typiod_engine_dirs_free(engine_dirs);
    if (!ok) {
        return 1;
    }

    if (options.list_only) {
        typiod_app_list_engines(&app);
        typiod_app_shutdown(&app);
        return 0;
    }

    exit_code = typiod_app_run(&app);
    typiod_app_shutdown(&app);
    return typiod_app_finish(&app, exit_code);
}
