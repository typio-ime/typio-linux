/**
 * @file test_language_helpers.c
 * @brief Pure-function tests for the language display helpers.
 *
 * Covers:
 *   - typio_language_endonym      (short display name)
 *   - typio_language_badge         (icon-sized glyph)
 *   - typio_language_menu_label    (disambiguated menu label, ADR-0033)
 *
 * These helpers feed the tray menu labels and the rendered badge pixmaps.
 * They are pure over their inputs so no TypioInstance fixture is needed.
 */

#include "state/controller.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void check_streq(const char *case_name,
                        const char *expected,
                        const char *actual) {
    bool ok = (expected == actual) ||
              (expected && actual && strcmp(expected, actual) == 0);
    printf("  %-50s %s\n", case_name, ok ? "OK" : "FAIL");
    if (!ok) {
        fprintf(stderr, "    expected=\"%s\" actual=\"%s\"\n",
                expected ? expected : "<null>",
                actual ? actual : "<null>");
        abort();
    }
}

/* ── typio_language_endonym ────────────────────────────────────────────── */

static void test_endonym_known(void) {
    printf("test_endonym_known\n");
    check_streq("zh → 中文",        "中文",     typio_language_endonym("zh"));
    check_streq("en → English",     "English",  typio_language_endonym("en"));
    check_streq("ja → 日本語",      "日本語",   typio_language_endonym("ja"));
    check_streq("ar → العربية",     "العربية",  typio_language_endonym("ar"));
    check_streq("ary → الدارجة",    "الدارجة",  typio_language_endonym("ary"));
    /* Extended-table coverage (P2a). */
    check_streq("uk → Українська",  "Українська", typio_language_endonym("uk"));
    check_streq("vi → Tiếng Việt",  "Tiếng Việt", typio_language_endonym("vi"));
    check_streq("pt → Português",   "Português",  typio_language_endonym("pt"));
    check_streq("hi → हिन्दी",       "हिन्दी",     typio_language_endonym("hi"));
}

static void test_endonym_matches_primary_subtag(void) {
    printf("test_endonym_matches_primary_subtag\n");
    /* Script and region suffixes must not change the endonym. */
    check_streq("zh-Hans → 中文",   "中文", typio_language_endonym("zh-Hans"));
    check_streq("zh-Hant → 中文",   "中文", typio_language_endonym("zh-Hant"));
    check_streq("en-US → English",  "English", typio_language_endonym("en-US"));
}

static void test_endonym_null_and_empty(void) {
    printf("test_endonym_null_and_empty\n");
    check_streq("NULL tag → NULL", nullptr, typio_language_endonym(nullptr));
    check_streq("empty tag → NULL", nullptr, typio_language_endonym(""));
}

/* ── typio_language_badge ──────────────────────────────────────────────── */

static void test_badge_known(void) {
    printf("test_badge_known\n");
    char out[32];
    typio_language_badge("zh", out, sizeof(out));
    check_streq("zh → 中", "中", out);

    typio_language_badge("en", out, sizeof(out));
    check_streq("en → EN", "EN", out);

    typio_language_badge("ary", out, sizeof(out));
    check_streq("ary → الد", "الد", out);

    typio_language_badge("ru", out, sizeof(out));
    check_streq("ru → Рус", "Рус", out);

    /* Extended-table coverage (P2a). */
    typio_language_badge("el", out, sizeof(out));
    check_streq("el → Ελ", "Ελ", out);

    typio_language_badge("bn", out, sizeof(out));
    check_streq("bn → বা", "বা", out);

    typio_language_badge("th", out, sizeof(out));
    check_streq("th → ไ", "ไ", out);
}

static void test_badge_uppercase_fallback(void) {
    printf("test_badge_uppercase_fallback\n");
    char out[32];
    /* Tags outside the table fall back to the uppercased primary subtag. */
    typio_language_badge("xx", out, sizeof(out));
    check_streq("xx → XX (uppercased)", "XX", out);

    /* Only the primary subtag is uppercased; suffixes are dropped. */
    typio_language_badge("xx-Latn", out, sizeof(out));
    check_streq("xx-Latn → XX (primary only)", "XX", out);

    typio_language_badge("xyz-foo", out, sizeof(out));
    check_streq("xyz-foo → XYZ (unknown primary)", "XYZ", out);
}

static void test_badge_null_and_empty(void) {
    printf("test_badge_null_and_empty\n");
    char out[32] = "untouched";
    typio_language_badge(nullptr, out, sizeof(out));
    check_streq("NULL tag → empty string", "", out);

    typio_language_badge("", out, sizeof(out));
    check_streq("empty tag → empty string", "", out);
}

/* ── typio_language_menu_label ─────────────────────────────────────────── */

static void test_menu_label_no_script(void) {
    printf("test_menu_label_no_script\n");
    char out[64];
    /* Primary tag or region-qualified tag: bare endonym, no qualifier. */
    typio_language_menu_label("zh", out, sizeof(out));
    check_streq("zh → 中文", "中文", out);

    typio_language_menu_label("en", out, sizeof(out));
    check_streq("en → English", "English", out);

    typio_language_menu_label("en-US", out, sizeof(out));
    check_streq("en-US → English (no script qualifier)", "English", out);

    /* Region suffix doesn't change endonym — pt-BR still renders as Português. */
    typio_language_endonym("pt-BR");
    check_streq("pt-BR → Português (region ignored)", "Português",
                typio_language_endonym("pt-BR"));

    /* A truly unlisted primary tag still falls through to the raw tag. */
    check_streq("xx-Region → xx-Region (fallback)", "xx-Region",
                typio_language_endonym("xx-Region"));
}

static void test_menu_label_script_disambiguation(void) {
    printf("test_menu_label_script_disambiguation\n");
    char out[64];
    /* The bug ADR-0033 fixes: zh-Hans and zh-Hant render distinctly in the
     * menu instead of both collapsing to "中文". */
    typio_language_menu_label("zh-Hans", out, sizeof(out));
    check_streq("zh-Hans → 中文 (简)", "中文 (简)", out);

    typio_language_menu_label("zh-Hant", out, sizeof(out));
    check_streq("zh-Hant → 中文 (繁)", "中文 (繁)", out);

    typio_language_menu_label("sr-Latn", out, sizeof(out));
    check_streq("sr-Latn → sr-Latn (Latin qualifier, tag fallback)",
                "sr-Latn (Latin)", out);

    typio_language_menu_label("sr-Cyrl", out, sizeof(out));
    check_streq("sr-Cyrl → sr-Cyrl (Cyrillic qualifier, tag fallback)",
                "sr-Cyrl (Cyrillic)", out);

    /* P2a: extended script table coverage. */
    typio_language_menu_label("uz-Arab", out, sizeof(out));
    check_streq("uz-Arab → uz-Arab (Arabic)", "uz-Arab (Arabic)", out);

    typio_language_menu_label("bn-Deva", out, sizeof(out));
    check_streq("bn-Deva → বাংলা (Devanagari)",
                "বাংলা (Devanagari)", out);

    typio_language_menu_label("ja-Hira", out, sizeof(out));
    check_streq("ja-Hira → 日本語 (Hiragana)", "日本語 (Hiragana)", out);
}

static void test_menu_label_unknown_script_passthrough(void) {
    printf("test_menu_label_unknown_script_passthrough\n");
    char out[64];
    /* An unrecognised 4-letter script subtag passes through verbatim so the
     * two entries remain distinguishable instead of collapsing. Subtags with
     * the wrong length (e.g. 3 letters) are not treated as scripts. */
    typio_language_menu_label("en-Abcd", out, sizeof(out));
    check_streq("en-Abcd → English (Abcd)", "English (Abcd)", out);

    typio_language_menu_label("en-Foo", out, sizeof(out));
    check_streq("en-Foo (3 letters, not a script) → English",
                "English", out);
}

static void test_menu_label_underscore_separator(void) {
    printf("test_menu_label_underscore_separator\n");
    char out[64];
    /* Underscore is also a valid BCP 47 separator (some legacy data uses it). */
    typio_language_menu_label("zh_Hans", out, sizeof(out));
    check_streq("zh_Hans → 中文 (简)", "中文 (简)", out);
}

static void test_menu_label_null_safety(void) {
    printf("test_menu_label_null_safety\n");
    char out[64] = "untouched";
    /* NULL or empty tag must not crash; output is empty. */
    typio_language_menu_label(nullptr, out, sizeof(out));
    check_streq("NULL tag → empty", "", out);

    typio_language_badge("", out, sizeof(out));
    check_streq("empty tag → empty", "", out);
}

int main(void) {
    test_endonym_known();
    test_endonym_matches_primary_subtag();
    test_endonym_null_and_empty();

    test_badge_known();
    test_badge_uppercase_fallback();
    test_badge_null_and_empty();

    test_menu_label_no_script();
    test_menu_label_script_disambiguation();
    test_menu_label_unknown_script_passthrough();
    test_menu_label_underscore_separator();
    test_menu_label_null_safety();

    printf("all language helper tests passed\n");
    return 0;
}
