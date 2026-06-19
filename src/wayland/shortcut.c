/**
 * @file shortcut.c
 * @brief Cached shortcut bindings used by the Wayland frontend
 */

#include "shortcut.h"

#include "typio/abi/config.h"
#include "typio/abi/shortcut.h"

#include <string.h>

static void load_one(TypioShortcutBinding *out,
                     const TypioConfig *config,
                     const char *action_id) {
    if (!out)
        return;
    memset(out, 0, sizeof(*out));
    if (typio_shortcut_get(config, action_id, out))
        return;
    typio_shortcut_default(action_id, out);
}

void typio_shortcut_config_load(TypioShortcutConfig *out,
                                const TypioConfig *config) {
    if (!out)
        return;

    load_one(&out->switch_language, config, "switch_language");
    load_one(&out->emergency_exit,  config, "exit");
    load_one(&out->voice_ptt,       config, "voice_ptt");
    load_one(&out->summon_indicator, config, "summon_indicator");
}
