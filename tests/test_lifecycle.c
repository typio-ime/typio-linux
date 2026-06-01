/**
 * @file test_lifecycle.c
 * @brief Lifecycle timing helper tests
 */

#include "lifecycle.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int tests_run = 0;
static int tests_passed = 0;

#define TEST(name) \
    static void test_##name(void); \
    static void run_test_##name(void) { \
        printf("  Running %s... ", #name); \
        tests_run++; \
        test_##name(); \
        tests_passed++; \
        printf("OK\n"); \
    } \
    static void test_##name(void)

#define ASSERT(expr) \
    do { \
        if (!(expr)) { \
            printf("FAILED\n"); \
            printf("    Assertion failed: %s\n", #expr); \
            printf("    At %s:%d\n", __FILE__, __LINE__); \
            exit(1); \
        } \
    } while(0)

TEST(names_are_stable) {
    ASSERT(strcmp(typio_wl_lifecycle_phase_name(TYPIO_WL_PHASE_INACTIVE),
                  "INACTIVE") == 0);
    ASSERT(strcmp(typio_wl_lifecycle_phase_name(TYPIO_WL_PHASE_ACTIVATING),
                  "ACTIVATING") == 0);
    ASSERT(strcmp(typio_wl_lifecycle_phase_name(TYPIO_WL_PHASE_ACTIVE),
                  "ACTIVE") == 0);
    ASSERT(strcmp(typio_wl_lifecycle_phase_name(TYPIO_WL_PHASE_DEACTIVATING),
                  "DEACTIVATING") == 0);
}

TEST(valid_transitions_follow_timing_model) {
    ASSERT(typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_INACTIVE, TYPIO_WL_PHASE_ACTIVATING));
    ASSERT(typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_ACTIVATING, TYPIO_WL_PHASE_ACTIVE));
    ASSERT(typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_ACTIVE, TYPIO_WL_PHASE_DEACTIVATING));
    ASSERT(typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_DEACTIVATING, TYPIO_WL_PHASE_INACTIVE));
    /* Re-activation: the compositor re-asserts focus on a new field while we
     * are still ACTIVE, without an intervening deactivate. */
    ASSERT(typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_ACTIVE, TYPIO_WL_PHASE_ACTIVATING));
}

TEST(rejects_unexpected_shortcuts_between_phases) {
    ASSERT(!typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_INACTIVE, TYPIO_WL_PHASE_ACTIVE));
    ASSERT(!typio_wl_lifecycle_transition_is_valid(
        TYPIO_WL_PHASE_ACTIVE, TYPIO_WL_PHASE_INACTIVE));
}

TEST(only_active_phase_allows_key_events) {
    ASSERT(!typio_wl_lifecycle_phase_allows_key_events(
        TYPIO_WL_PHASE_INACTIVE));
    ASSERT(!typio_wl_lifecycle_phase_allows_key_events(
        TYPIO_WL_PHASE_ACTIVATING));
    ASSERT(typio_wl_lifecycle_phase_allows_key_events(
        TYPIO_WL_PHASE_ACTIVE));
    ASSERT(!typio_wl_lifecycle_phase_allows_key_events(
        TYPIO_WL_PHASE_DEACTIVATING));
}

TEST(activating_and_active_phases_allow_modifier_events) {
    ASSERT(!typio_wl_lifecycle_phase_allows_modifier_events(
        TYPIO_WL_PHASE_INACTIVE));
    ASSERT(typio_wl_lifecycle_phase_allows_modifier_events(
        TYPIO_WL_PHASE_ACTIVATING));
    ASSERT(typio_wl_lifecycle_phase_allows_modifier_events(
        TYPIO_WL_PHASE_ACTIVE));
    ASSERT(!typio_wl_lifecycle_phase_allows_modifier_events(
        TYPIO_WL_PHASE_DEACTIVATING));
}

/* classify_done(was_active, now_active, activate_seen) is the single source of
 * truth for how a compositor `done` changes IME state. Cover the full truth
 * table. */
TEST(classify_done_activate_on_inactive_to_active) {
    /* Becoming active is ACTIVATE regardless of whether the activate edge was
     * recorded (it always is, but the classification must not depend on it). */
    ASSERT(typio_wl_lifecycle_classify_done(false, true, true) ==
           TYPIO_WL_DONE_ACTIVATE);
    ASSERT(typio_wl_lifecycle_classify_done(false, true, false) ==
           TYPIO_WL_DONE_ACTIVATE);
}

TEST(classify_done_deactivate_on_active_to_inactive) {
    ASSERT(typio_wl_lifecycle_classify_done(true, false, true) ==
           TYPIO_WL_DONE_DEACTIVATE);
    ASSERT(typio_wl_lifecycle_classify_done(true, false, false) ==
           TYPIO_WL_DONE_DEACTIVATE);
}

TEST(classify_done_reactivate_only_when_active_to_active_with_activate) {
    /* Staying active *with* a fresh activate this batch = a real re-activation
     * (new field) -> REACTIVATE. */
    ASSERT(typio_wl_lifecycle_classify_done(true, true, true) ==
           TYPIO_WL_DONE_REACTIVATE);
    /* Staying active *without* an activate = a plain text-state update -> NONE.
     * This is the guard that prevents rebuilding focus mid-composition. */
    ASSERT(typio_wl_lifecycle_classify_done(true, true, false) ==
           TYPIO_WL_DONE_NONE);
}

TEST(classify_done_none_when_staying_inactive) {
    ASSERT(typio_wl_lifecycle_classify_done(false, false, true) ==
           TYPIO_WL_DONE_NONE);
    ASSERT(typio_wl_lifecycle_classify_done(false, false, false) ==
           TYPIO_WL_DONE_NONE);
}

int main(void) {
    printf("Running lifecycle tests:\n");

    run_test_names_are_stable();
    run_test_valid_transitions_follow_timing_model();
    run_test_rejects_unexpected_shortcuts_between_phases();
    run_test_only_active_phase_allows_key_events();
    run_test_activating_and_active_phases_allow_modifier_events();
    run_test_classify_done_activate_on_inactive_to_active();
    run_test_classify_done_deactivate_on_active_to_inactive();
    run_test_classify_done_reactivate_only_when_active_to_active_with_activate();
    run_test_classify_done_none_when_staying_inactive();

    printf("\n%d/%d tests passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
