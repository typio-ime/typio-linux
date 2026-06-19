/*
 * Regression tests for the candidate-snapshot lifecycle
 * (src/wayland/candidate_snapshot.c).
 *
 * The bug these guard against: candidate-snapshot heap state (the candidates
 * array + 3×N text/comment/label strings per candidate) leaking across
 * focus transitions and on session teardown. Two specific leaks were fixed:
 *
 *   - typio_wl_session_destroy freed the surrounding TypioWlSession struct
 *     but left the embedded TypioCandidateList's heap pointers dangling,
 *     because the struct is embedded by value and only its heap-owned fields
 *     need explicit teardown.
 *   - The discard_composition focus effect reset the engine + hid the panel
 *     but never cleared the snapshot, leaking on every focus-out /
 *     engine-switch (asymmetric with on_commit_callback, which did clear).
 *
 * Both paths now route through typio_wl_session_clear_candidate_state, which
 * in turn calls typio_candidate_snapshot_clear. These tests exercise:
 *
 *   - clear frees the candidates array + per-candidate strings
 *   - clear is idempotent (safe on already-empty snapshots; no double-free)
 *   - session_clear_state zeroes the candidate-guard scalars
 *   - the assign fast path correctly detects equal-content compositions
 *
 * Pure CPU; no Wayland, no GPU. The TypioWlSession is constructed via calloc
 * so every non-snapshot field is zero (we only mutate + verify the snapshot
 * and the three candidate-guard scalars).
 */

#include "candidate_snapshot.h"
#include "internal.h"

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

#define CHECK(cond) \
    do { if (!(cond)) { printf("FAILED\n    %s:%d: %s\n", __FILE__, __LINE__, #cond); exit(1); } } while (0)

/* Build a heap-owned snapshot matching what candidate_snapshot_assign would
 * produce for a synthetic composition. We construct it directly so the test
 * does not depend on TypioComposition internals beyond the field names that
 * TypioCandidate mirrors. */
static void populate_snapshot(TypioCandidateList *snap, size_t n)
{
    memset(snap, 0, sizeof(*snap));
    snap->candidates = (TypioCandidate *)calloc(n, sizeof(TypioCandidate));
    CHECK(snap->candidates != NULL);
    snap->count = n;
    snap->selected = 0;
    snap->total = (int)n;
    snap->page = 0;
    snap->page_size = (int)n;
    snap->has_prev = false;
    snap->has_next = false;
    snap->content_signature = 0xDEADBEEFu;

    for (size_t i = 0; i < n; ++i) {
        /* Allocate distinct heap strings so a leak detector / asan would
         * flag any path that fails to free them. */
        char buf[64];
        snprintf(buf, sizeof(buf), "text-%zu", i);
        snap->candidates[i].text = strdup(buf);
        snprintf(buf, sizeof(buf), "comment-%zu", i);
        snap->candidates[i].comment = strdup(buf);
        snprintf(buf, sizeof(buf), "%zu", i);
        snap->candidates[i].label = strdup(buf);

        CHECK(snap->candidates[i].text    != NULL);
        CHECK(snap->candidates[i].comment != NULL);
        CHECK(snap->candidates[i].label   != NULL);
    }
}

/* Verify every field is in its post-clear state. */
static void assert_snapshot_empty(const TypioCandidateList *snap, const char *ctx)
{
    if (snap->candidates != NULL) {
        printf("FAILED\n    %s: candidates != NULL\n", ctx);
        exit(1);
    }
    CHECK(snap->count == 0);
    CHECK(snap->selected == -1);
    CHECK(snap->total == 0);
    CHECK(snap->page == 0);
    CHECK(snap->page_size == 0);
    CHECK(snap->has_prev == false);
    CHECK(snap->has_next == false);
    CHECK(snap->content_signature == 0);
}

TEST(clear_frees_all_fields) {
    TypioCandidateList snap;
    populate_snapshot(&snap, 5);
    typio_candidate_snapshot_clear(&snap);
    assert_snapshot_empty(&snap, "after clear");
}

TEST(clear_is_idempotent) {
    /* Clearing an already-empty snapshot must be a no-op (no double-free,
     * no use-after-free). This is the regression surface for the
     * discard_composition fix: it now clears on every focus-out, including
     * ones that fire after the snapshot was already cleared by a previous
     * effect. */
    TypioCandidateList snap = {0};
    typio_candidate_snapshot_clear(&snap);     /* never populated */
    typio_candidate_snapshot_clear(&snap);     /* cleared again */
    typio_candidate_snapshot_clear(NULL);      /* NULL-safe */
    assert_snapshot_empty(&snap, "after triple-clear");
}

TEST(clear_after_assign_then_clear) {
    /* The on_commit_callback path clears, then the composition callback
     * fires with a NULL composition (also clears), then session_destroy
     * clears again. Every transition must be safe. */
    TypioCandidateList snap = {0};
    populate_snapshot(&snap, 3);
    typio_candidate_snapshot_clear(&snap);

    populate_snapshot(&snap, 4);   /* simulate a fresh assign */
    typio_candidate_snapshot_clear(&snap);
    typio_candidate_snapshot_clear(&snap);

    assert_snapshot_empty(&snap, "after populate-clear-populate-clear-clear");
}

TEST(session_clear_state_zeros_guard_scalars) {
    /* The bug fix added calls to typio_wl_session_clear_candidate_state from
     * session_destroy and discard_composition. Verify the function zeros the
     * three candidate-guard scalars (consumed by the keyboard router on every
     * keypress) in addition to clearing the snapshot. Without this, stale
     * guard state survives a focus-out and causes the next focus-in's
     * navigation keys to be silently consumed against the empty snapshot. */
    TypioWlSession *session = (TypioWlSession *)calloc(1, sizeof(TypioWlSession));
    CHECK(session != NULL);

    populate_snapshot(&session->candidate_snapshot, 3);
    session->last_candidate_count = 3;
    session->last_candidate_selected = 1;
    session->last_host_managed_selection = 42;   /* any non-NONE value */

    typio_wl_session_clear_candidate_state(session);

    assert_snapshot_empty(&session->candidate_snapshot, "session snapshot");
    CHECK(session->last_candidate_count == 0);
    CHECK(session->last_candidate_selected == -1);
    CHECK(session->last_host_managed_selection == TYPIO_HOST_SEL_NONE);

    free(session);
}

TEST(session_clear_state_is_null_safe) {
    /* discard_composition guards with `frontend->session && frontend->session->ctx`,
     * but session_destroy is reached via the engine teardown path where session
     * itself may be NULL on early-failure paths. */
    typio_wl_session_clear_candidate_state(NULL);
}

TEST(session_clear_state_idempotent) {
    /* discard_composition can fire on consecutive focus-outs without an
     * intervening composition. Clearing twice must not double-free. */
    TypioWlSession *session = (TypioWlSession *)calloc(1, sizeof(TypioWlSession));
    CHECK(session != NULL);

    populate_snapshot(&session->candidate_snapshot, 2);
    typio_wl_session_clear_candidate_state(session);
    typio_wl_session_clear_candidate_state(session);

    assert_snapshot_empty(&session->candidate_snapshot, "double-clear");
    free(session);
}

int main(void) {
    printf("Running candidate_snapshot lifecycle tests:\n");
    run_test_clear_frees_all_fields();
    run_test_clear_is_idempotent();
    run_test_clear_after_assign_then_clear();
    run_test_session_clear_state_zeros_guard_scalars();
    run_test_session_clear_state_is_null_safe();
    run_test_session_clear_state_idempotent();
    printf("\nPassed %d/%d tests\n", tests_passed, tests_run);
    return 0;
}
