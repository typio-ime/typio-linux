/**
 * @file engine_placeholder.c
 * @brief Placeholder engine info for when no engine is loaded.
 */

#include "state/engine_placeholder.h"

const TypioEngineInfo *typio_engine_info_placeholder(void)
{
    static const TypioEngineInfo placeholder = {
        .name = "",
        .display_name = "No engine loaded",
        .description = "No input engine is currently active",
        .author = "",
        .icon = "typio-keyboard-off-symbolic",
        .language = "",
        .type = TYPIO_ENGINE_TYPE_KEYBOARD,
        .required_capabilities = NULL,
        .optional_capabilities = NULL,
    };

    return &placeholder;
}
