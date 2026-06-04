/**
 * @file test_session_controller.c
 * @brief Property-based tests for the pure session-controller decisions.
 *
 * Every retired guard from the old reconciler/phase model must first become
 * a passing test here before its imperative code is removed.
 */

#include "engine/session_controller.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int tests_run = 0;
static int tests_passed = 0;

#define TEST(name)                                                           \
    static void test_##name(void);                                           \
    static void run_test_##name(void) {                                      \
        printf("  Running %s... ", #name);                                   \
        tests_run++;                                                         \
        test_##name();                                                       \
        tests_passed++;                                                      \
        printf("OK\n");                                                      \
    }                                                                        \
    static void test_##name(void)

#define ASSERT(expr)                                                         \
    do {                                                                     \
        if (!(expr)) {                                                       \
            printf("FAILED\n");                                              \
            printf("    Assertion failed: %s\n", #expr);                     \
            printf("    At %s:%d\n", __FILE__, __LINE__);                   \
            exit(1);                                                         \
        }                                                                    \
    } while (0)

/* ── Reduce: grab want ─────────────────────────────────────────────────── */

TEST(reduce_connection_dead_forces_none) {
    TypioWlInputFacts facts = {.connection_alive = false, .im_activate_seen = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_NONE);
}

TEST(reduce_suspend_gap_forces_none) {
    TypioWlInputFacts facts = {.connection_alive = true, .suspend_gap_detected = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_NONE);
}

TEST(reduce_deactivate_to_soft_pause) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_deactivate_seen = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_SOFT_PAUSE);
}

TEST(reduce_activate_to_yes) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_activate_seen = true, .engine_present = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_YES);
}

TEST(reduce_done_had_activate_to_yes) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_done_had_activate = true, .engine_present = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_YES);
}

TEST(reduce_no_event_preserves_prev) {
    TypioWlInputFacts facts = {.connection_alive = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_SOFT_PAUSE);
}

TEST(reduce_hard_boundary_overrides_deactivate) {
    /* suspend + deactivate in the same tick: hard boundary wins. */
    TypioWlInputFacts facts = {
        .connection_alive = true,
        .suspend_gap_detected = true,
        .im_deactivate_seen = true,
    };
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_NONE);
}

/* ── Reduce: focus edge detection ──────────────────────────────────────── */

TEST(reduce_none_to_yes_triggers_focus_in) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_activate_seen = true, .engine_present = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == true);
    ASSERT(d.focus_out == false);
}

TEST(reduce_yes_to_none_triggers_focus_out) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_deactivate_seen = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == false);
    ASSERT(d.focus_out == true);
}

TEST(reduce_soft_pause_to_yes_triggers_focus_in) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_activate_seen = true, .engine_present = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == true);
    ASSERT(d.focus_out == false);
}

TEST(reduce_yes_to_soft_pause_triggers_focus_out) {
    TypioWlInputFacts facts = {.connection_alive = true, .im_deactivate_seen = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == false);
    ASSERT(d.focus_out == true);
}

TEST(reduce_stable_yes_no_edge) {
    TypioWlInputFacts facts = {.connection_alive = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == false);
    ASSERT(d.focus_out == false);
}

TEST(reduce_reactivate_no_edge) {
    /* Reactivate: YES → YES (no intervening deactivate, just a new done). */
    TypioWlInputFacts facts = {.connection_alive = true, .im_done_had_activate = true, .engine_present = true};
    TypioWlDesiredState prev = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlDesiredState d = typio_wl_session_reduce(&facts, &prev);
    ASSERT(d.focus_in == false);
    ASSERT(d.focus_out == false);
}

/* ── Diff: grab lifecycle ──────────────────────────────────────────────── */

TEST(diff_none_with_absent_noop) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_ABSENT};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.destroy_grab == false);
    ASSERT(e.create_grab == false);
}

TEST(diff_none_with_ready_destroys) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_READY};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.destroy_grab == true);
    ASSERT(e.clear_preedit == true);
    ASSERT(e.commit == true);
}

TEST(diff_yes_with_absent_creates) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_ABSENT};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.create_grab == true);
    ASSERT(e.scrub_generation == true);
}

TEST(diff_yes_with_broken_rebuilds) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_BROKEN};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.destroy_grab == true);
    ASSERT(e.create_grab == true);
    ASSERT(e.scrub_generation == true);
}

TEST(diff_soft_pause_with_absent_creates) {
    /* Grab was silently dropped while paused; rebuild it. */
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_ABSENT};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.create_grab == true);
    ASSERT(e.scrub_generation == true);
}

TEST(diff_soft_pause_with_ready_noop) {
    /* Normal paused state: grab retained, nothing to do. */
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_READY};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.destroy_grab == false);
    ASSERT(e.create_grab == false);
}

TEST(diff_yes_with_needs_keymap_noop) {
    /* Mid-activation handshake: grab exists, waiting for keymap. */
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_NEEDS_KEYMAP};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.destroy_grab == false);
    ASSERT(e.create_grab == false);
}

/* ── Diff: focus edges ─────────────────────────────────────────────────── */

TEST(diff_focus_in_effect) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_YES, .focus_in = true};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_READY};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.send_focus_in == true);
    ASSERT(e.send_focus_out == false);
}

TEST(diff_focus_out_effect) {
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE, .focus_out = true};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_READY};
    TypioWlEffectSet e = typio_wl_session_diff(&d, &a);
    ASSERT(e.send_focus_in == false);
    ASSERT(e.send_focus_out == true);
}

/* ── Diff: idempotency scenarios ───────────────────────────────────────── */

TEST(diff_idempotent_stable_none) {
    /* Stable NONE + ABSENT: repeated diff produces no effects. */
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_NONE};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_ABSENT};
    TypioWlEffectSet e1 = typio_wl_session_diff(&d, &a);
    TypioWlEffectSet e2 = typio_wl_session_diff(&d, &a);
    ASSERT(memcmp(&e1, &e2, sizeof(e1)) == 0);
    ASSERT(e1.destroy_grab == false);
    ASSERT(e1.create_grab == false);
}

TEST(diff_idempotent_stable_yes_ready) {
    /* Stable YES + READY: repeated diff produces no effects. */
    TypioWlDesiredState d = {.grab = TYPIO_WL_GRAB_WANT_YES};
    TypioWlActualState  a = {.grab = TYPIO_WL_GRAB_RES_READY};
    TypioWlEffectSet e1 = typio_wl_session_diff(&d, &a);
    TypioWlEffectSet e2 = typio_wl_session_diff(&d, &a);
    ASSERT(memcmp(&e1, &e2, sizeof(e1)) == 0);
}

/* ── Null pointer safety ──────────────────────────────────────────────── */

TEST(reduce_null_returns_safe_defaults) {
    TypioWlDesiredState d = typio_wl_session_reduce(NULL, NULL);
    ASSERT(d.grab == TYPIO_WL_GRAB_WANT_NONE);
    ASSERT(d.focus_in == false);
    ASSERT(d.focus_out == false);
}

TEST(diff_null_returns_empty_effects) {
    TypioWlEffectSet e = typio_wl_session_diff(NULL, NULL);
    ASSERT(e.destroy_grab == false);
    ASSERT(e.create_grab == false);
}

/* ── Main ─────────────────────────────────────────────────────────────── */

int main(void)
{
    printf("Running session controller tests:\n");

    run_test_reduce_connection_dead_forces_none();
    run_test_reduce_suspend_gap_forces_none();
    run_test_reduce_deactivate_to_soft_pause();
    run_test_reduce_activate_to_yes();
    run_test_reduce_done_had_activate_to_yes();
    run_test_reduce_no_event_preserves_prev();
    run_test_reduce_hard_boundary_overrides_deactivate();

    run_test_reduce_none_to_yes_triggers_focus_in();
    run_test_reduce_yes_to_none_triggers_focus_out();
    run_test_reduce_soft_pause_to_yes_triggers_focus_in();
    run_test_reduce_yes_to_soft_pause_triggers_focus_out();
    run_test_reduce_stable_yes_no_edge();
    run_test_reduce_reactivate_no_edge();

    run_test_diff_none_with_absent_noop();
    run_test_diff_none_with_ready_destroys();
    run_test_diff_yes_with_absent_creates();
    run_test_diff_yes_with_broken_rebuilds();
    run_test_diff_soft_pause_with_absent_creates();
    run_test_diff_soft_pause_with_ready_noop();
    run_test_diff_yes_with_needs_keymap_noop();

    run_test_diff_focus_in_effect();
    run_test_diff_focus_out_effect();

    run_test_diff_idempotent_stable_none();
    run_test_diff_idempotent_stable_yes_ready();

    run_test_reduce_null_returns_safe_defaults();
    run_test_diff_null_returns_empty_effects();

    printf("\n%d/%d tests passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
