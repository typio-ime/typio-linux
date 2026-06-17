/**
 * @file test_controller_icon.c
 * @brief Pure-function tests for the language-only icon resolution chain.
 *
 * Exercises `typio_resolve_language_icon` (ADR-0033) without constructing a
 * TypioInstance or TypioRegistry — the function is pure over its inputs, so
 * each precedence layer is probed directly.
 *
 * Layers under test:
 *   1. [languages.<tag>].icon config override
 *   2. language badge (rendered text)
 *   3. generic typio-keyboard-symbolic (active, no icon)
 *   4. typio-keyboard-off-symbolic (nothing active)
 */

#include "state/controller.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "typio/abi/config.h"

static void assert_icon(const char *case_name,
                        const char *expected_icon,
                        bool expected_badge,
                        const char *expected_badge_text,
                        char *actual_icon,
                        bool actual_badge,
                        char *actual_badge_text) {
    bool ok = actual_icon && strcmp(actual_icon, expected_icon) == 0 &&
              actual_badge == expected_badge &&
              ((expected_badge_text == nullptr && actual_badge_text == nullptr) ||
               (expected_badge_text && actual_badge_text &&
                strcmp(actual_badge_text, expected_badge_text) == 0));
    printf("  %-44s %s\n", case_name, ok ? "OK" : "FAIL");
    if (!ok) {
        fprintf(stderr,
                "    expected icon=%s badge=%d badge_text=%s\n"
                "    actual   icon=%s badge=%d badge_text=%s\n",
                expected_icon, expected_badge, expected_badge_text ? expected_badge_text : "<null>",
                actual_icon ? actual_icon : "<null>", actual_badge,
                actual_badge_text ? actual_badge_text : "<null>");
        abort();
    }
    free(actual_icon);
    free(actual_badge_text);
}

static void test_layer4_nothing_active(void) {
    printf("test_layer4_nothing_active\n");
    bool is_badge = false;
    char *badge_text = nullptr;
    char *icon = typio_resolve_language_icon(nullptr, false, nullptr,
                                             &is_badge, &badge_text);
    assert_icon("null tag, no engine → off",
                "typio-keyboard-off-symbolic", false, nullptr,
                icon, is_badge, badge_text);
}

static void test_layer3_engine_only_no_language(void) {
    printf("test_layer3_engine_only_no_language\n");
    bool is_badge = false;
    char *badge_text = nullptr;
    /* Engine active but no language (legacy engine-cycling install). Falls
     * to the generic "on" glyph because there is no tag to badge from. */
    char *icon = typio_resolve_language_icon(nullptr, true, nullptr,
                                             &is_badge, &badge_text);
    assert_icon("null tag, engine active → generic keyboard",
                "typio-keyboard-symbolic", false, nullptr,
                icon, is_badge, badge_text);
}

static void test_layer2_language_badge(void) {
    printf("test_layer2_language_badge\n");
    bool is_badge = false;
    char *badge_text = nullptr;
    /* zh has a badge glyph in the table; no per-language config. */
    char *icon = typio_resolve_language_icon("zh", false, nullptr,
                                             &is_badge, &badge_text);
    assert_icon("zh → 中 badge",
                "typio-keyboard-symbolic", true, "中",
                icon, is_badge, badge_text);

    /* Script-qualified tag matches on the primary subtag. */
    is_badge = false;
    badge_text = nullptr;
    icon = typio_resolve_language_icon("zh-Hans", false, nullptr,
                                       &is_badge, &badge_text);
    assert_icon("zh-Hans → 中 badge (script subtag ignored)",
                "typio-keyboard-symbolic", true, "中",
                icon, is_badge, badge_text);

    /* Latin tags uppercase to the badge form. */
    is_badge = false;
    badge_text = nullptr;
    icon = typio_resolve_language_icon("en", false, nullptr,
                                       &is_badge, &badge_text);
    assert_icon("en → EN badge",
                "typio-keyboard-symbolic", true, "EN",
                icon, is_badge, badge_text);
}

static void test_layer2_unknown_tag_uses_uppercase_fallback(void) {
    printf("test_layer2_unknown_tag_uses_uppercase_fallback\n");
    /* typio_language_badge uppercases the primary subtag for tags not in the
     * table, so 'xx' yields a 'XX' badge rather than falling through to the
     * generic icon. This is the documented fallback (controller.h:113). */
    bool is_badge = false;
    char *badge_text = nullptr;
    char *icon = typio_resolve_language_icon("xx", false, nullptr,
                                             &is_badge, &badge_text);
    assert_icon("unknown alphabetic tag → uppercased badge",
                "typio-keyboard-symbolic", true, "XX",
                icon, is_badge, badge_text);

    /* A script/region suffix only contributes the primary subtag to the
     * fallback badge. */
    is_badge = false;
    badge_text = nullptr;
    icon = typio_resolve_language_icon("xx-Latn", false, nullptr,
                                        &is_badge, &badge_text);
     assert_icon("unknown tag with script → primary-subtag badge",
                 "typio-keyboard-symbolic", true, "XX",
                 icon, is_badge, badge_text);
}

static void test_layer1_config_override(void) {
    printf("test_layer1_config_override\n");
    /* Synthesize a config with [languages.zh] icon = "my-zh-icon-symbolic".
     * Layer 1 must shadow the layer 2 badge. */
    TypioConfig *cfg = typio_config_load_string(
        "[languages.zh]\n"
        "icon = \"my-zh-icon-symbolic\"\n");
    assert(cfg);

    bool is_badge = false;
    char *badge_text = nullptr;
    char *icon = typio_resolve_language_icon("zh", false, cfg,
                                             &is_badge, &badge_text);
    assert_icon("zh with [languages.zh].icon → config icon (badge suppressed)",
                "my-zh-icon-symbolic", false, nullptr,
                icon, is_badge, badge_text);

    /* Configured icon for one tag does not leak into another. */
    is_badge = false;
    badge_text = nullptr;
    icon = typio_resolve_language_icon("en", false, cfg,
                                       &is_badge, &badge_text);
    assert_icon("en with [languages.zh].icon set → en badge (no leak)",
                "typio-keyboard-symbolic", true, "EN",
                icon, is_badge, badge_text);

    typio_config_free(cfg);
}

static void test_layer1_config_override_script_qualified(void) {
    printf("test_layer1_config_override_script_qualified\n");
    /* The lookup key is the full tag, so [languages.zh-Hans] and
     * [languages.zh] are distinct overrides. */
    TypioConfig *cfg = typio_config_load_string(
        "[languages.zh-Hans]\n"
        "icon = \"hans-only-icon\"\n");
    assert(cfg);

    bool is_badge = false;
    char *badge_text = nullptr;
    char *icon = typio_resolve_language_icon("zh-Hans", false, cfg,
                                             &is_badge, &badge_text);
    assert_icon("zh-Hans with [languages.zh-Hans].icon → config icon",
                "hans-only-icon", false, nullptr,
                icon, is_badge, badge_text);

    /* zh (primary) does NOT pick up the zh-Hans override — falls to badge. */
    is_badge = false;
    badge_text = nullptr;
    icon = typio_resolve_language_icon("zh", false, cfg,
                                       &is_badge, &badge_text);
    assert_icon("zh with only [languages.zh-Hans].icon → 中 badge (key is exact)",
                "typio-keyboard-symbolic", true, "中",
                icon, is_badge, badge_text);

    typio_config_free(cfg);
}

static void test_null_config_is_safe(void) {
    printf("test_null_config_is_safe\n");
    bool is_badge = false;
    char *badge_text = nullptr;
    /* cfg=NULL must skip layer 1 without crashing. */
    char *icon = typio_resolve_language_icon("ja", false, nullptr,
                                             &is_badge, &badge_text);
    assert_icon("ja, cfg=NULL → あ badge",
                "typio-keyboard-symbolic", true, "あ",
                icon, is_badge, badge_text);
}

static void test_null_out_params_defensive(void) {
    printf("test_null_out_params_defensive\n");
    /* Even with NULL out params the function must return a non-NULL icon
     * the caller can free. */
    char *icon = typio_resolve_language_icon("zh", false, nullptr, nullptr, nullptr);
    assert(icon);
    assert(strcmp(icon, "typio-keyboard-symbolic") == 0 ||
           strcmp(icon, "typio-keyboard-off-symbolic") == 0);
    printf("  %-44s OK\n", "NULL out params → non-NULL icon, no crash");
    free(icon);
}

int main(void) {
    test_layer4_nothing_active();
    test_layer3_engine_only_no_language();
    test_layer2_language_badge();
    test_layer2_unknown_tag_uses_uppercase_fallback();
    test_layer1_config_override();
    test_layer1_config_override_script_qualified();
    test_null_config_is_safe();
    test_null_out_params_defensive();
    printf("all controller icon tests passed\n");
    return 0;
}
