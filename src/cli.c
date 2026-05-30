#include "cli.h"

#include "typio_build_config.h"
#include "typio/typio.h"

#include <getopt.h>
#include <stdio.h>
#include <string.h>

static const char *typio_build_display_string(void) {
    static char buf[128];
    if (buf[0])
        return buf;
    if (TYPIO_BUILD_SOURCE_LABEL[0]) {
        snprintf(buf, sizeof(buf), "typio-wayland %s (%s)",
                 TYPIO_VERSION, TYPIO_BUILD_SOURCE_LABEL);
    } else {
        snprintf(buf, sizeof(buf), "typio-wayland %s", TYPIO_VERSION);
    }
    return buf;
}

void typio_options_init(TypioOptions *options) {
    if (!options) {
        return;
    }

    memset(options, 0, sizeof(*options));
}

void typio_print_version(void) {
    printf("%s\n", typio_build_display_string());
    printf("An extensible input method framework supporting multiple engines\n");
}

void typio_print_help(const char *prog) {
    printf("Usage: %s [OPTIONS]\n\n", prog);
    printf("Options:\n");
    printf("  -c, --config DIR    Configuration directory\n");
    printf("  -d, --data DIR      Data directory\n");
    printf("  -E, --engine-dir DIR Engine directory\n");
    printf("  -l, --list          List available engines and exit\n");
    printf("  -v, --verbose       Enable verbose logging\n");
    printf("  -h, --help          Show this help message\n");
    printf("  --version           Show version information\n");
}

int typio_parse_args(TypioOptions *options, int argc, char *argv[]) {
    static struct option long_options[] = {
        {"config", required_argument, 0, 'c'},
        {"data", required_argument, 0, 'd'},
        {"engine-dir", required_argument, 0, 'E'},
        {"list", no_argument, 0, 'l'},
        {"verbose", no_argument, 0, 'v'},
        {"help", no_argument, 0, 'h'},
        {"version", no_argument, 0, 'V'},
        {0, 0, 0, 0}
    };

    int opt;

    if (!options) {
        return 1;
    }

    while ((opt = getopt_long(argc, argv, "c:d:E:lvhV", long_options, nullptr)) != -1) {
        switch (opt) {
            case 'c':
                options->instance_config.config_dir = optarg;
                break;
            case 'd':
                options->instance_config.data_dir = optarg;
                break;
            case 'E':
                options->engine_dir_override = optarg;
                break;
            case 'l':
                options->list_only = true;
                break;
            case 'v':
                options->verbose = true;
                break;
            case 'h':
                typio_print_help(argv[0]);
                return 0;
            case 'V':
                typio_print_version();
                return 0;
            default:
                typio_print_help(argv[0]);
                return 1;
        }
    }

    return -1;
}
