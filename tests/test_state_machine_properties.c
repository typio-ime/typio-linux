/**
 * @file test_state_machine_properties.c
 * @brief Property tests for the resilience state machines.
 *
 * Dependency-free: a small LCG drives randomized inputs through the pure
 * decision functions and checks invariants against an independent
 * re-derivation of each spec. Seeds are fixed so failures reproduce. This
 * complements the example-based tests by exercising long random sequences,
 * including the edge transitions (clock ties, cooldown boundaries,
 * threshold boundaries) that hand-written cases tend to miss.
 */

#include "session_controller.h"
#include "backoff.h"
#include "resume_model.h"

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

/* Deterministic LCG (Numerical Recipes constants). */
static uint32_t rng_state = 0;
static uint32_t rng_next(void) {
    rng_state = rng_state * 1664525u + 1013904223u;
    return rng_state;
}
static uint32_t rng_below(uint32_t bound) {
    return bound ? rng_next() % bound : 0;
}

#define ITERATIONS 20000

/* ---- session_controller pure predicates ------------------------------ */
/*
 * Property: every pure predicate is a total function of the actual state
 * and an independent re-derivation must agree, across the full Cartesian
 * product of small axis spaces. The exhaustive enumeration over the
 * 2*2*4 = 16 input-context/grab states is the strongest possible test for
 * a branch-only function: if it ever disagrees, the spec and the code have
 * drifted.
 */
TEST(can_route_keys_exhaustive) {
    for (int focused = 0; focused < 2; ++focused) {
        for (int g = 0; g < 4; ++g) {
            TypioWlActualState actual = {
                .ic_focused = (bool)focused,
                .grab = (TypioWlGrabResourceState)g,
            };
            bool got = typio_wl_session_can_route_keys(&actual);
            /* Spec: routed iff focused AND grab is READY. */
            bool want = focused && g == TYPIO_WL_GRAB_RES_READY;
            ASSERT(got == want);
        }
    }
}

TEST(can_route_modifiers_exhaustive) {
    for (int focused = 0; focused < 2; ++focused) {
        for (int g = 0; g < 4; ++g) {
            TypioWlActualState actual = {
                .ic_focused = (bool)focused,
                .grab = (TypioWlGrabResourceState)g,
            };
            bool got = typio_wl_session_can_route_modifiers(&actual);
            /* Spec: routed iff grab is not ABSENT and not BROKEN. */
            bool want = g != TYPIO_WL_GRAB_RES_ABSENT &&
                        g != TYPIO_WL_GRAB_RES_BROKEN;
            ASSERT(got == want);
        }
    }
}

TEST(is_transitioning_exhaustive) {
    for (int focused = 0; focused < 2; ++focused) {
        for (int g = 0; g < 4; ++g) {
            TypioWlActualState actual = {
                .ic_focused = (bool)focused,
                .grab = (TypioWlGrabResourceState)g,
            };
            bool got = typio_wl_session_is_transitioning(&actual);
            /* Spec: transitioning iff focused AND grab is ABSENT, NEEDS_KEYMAP,
             * or BROKEN. The "focused but no grab" case is the start of the
             * activation handshake. */
            bool want = focused &&
                        (g == TYPIO_WL_GRAB_RES_ABSENT ||
                         g == TYPIO_WL_GRAB_RES_NEEDS_KEYMAP ||
                         g == TYPIO_WL_GRAB_RES_BROKEN);
            ASSERT(got == want);
        }
    }
}

/* Property: classify_done is total and matches the documented truth table
 * over all 2^3 = 8 (was_active, now_active, activate_seen) combinations. */
TEST(classify_done_exhaustive) {
    static const TypioWlDoneAction truth[2][2][2] = {
        /* was_active=false */
        { /* now_active=false */ { TYPIO_WL_DONE_NOOP,           TYPIO_WL_DONE_NOOP },
          /* now_active=true  */ { TYPIO_WL_DONE_FIRST_ACTIVATE, TYPIO_WL_DONE_FIRST_ACTIVATE } },
        /* was_active=true */
        { /* now_active=false */ { TYPIO_WL_DONE_DEACTIVATE,     TYPIO_WL_DONE_DEACTIVATE },
          /* now_active=true  */ { TYPIO_WL_DONE_NOOP,           TYPIO_WL_DONE_REACTIVATE } },
    };
    for (int w = 0; w < 2; ++w) {
        for (int n = 0; n < 2; ++n) {
            for (int a = 0; a < 2; ++a) {
                TypioWlDoneAction got = typio_wl_session_classify_done(
                    (bool)w, (bool)n, (bool)a);
                ASSERT(got == truth[w][n][a]);
            }
        }
    }
}

/* Property: the predicates are mutually consistent with each other.
 * can_route_keys ⇒ !is_transitioning (we never route while transitioning). */
TEST(predicates_are_mutually_consistent) {
    for (int focused = 0; focused < 2; ++focused) {
        for (int g = 0; g < 4; ++g) {
            TypioWlActualState actual = {
                .ic_focused = (bool)focused,
                .grab = (TypioWlGrabResourceState)g,
            };
            bool routed = typio_wl_session_can_route_keys(&actual);
            bool transit = typio_wl_session_is_transitioning(&actual);
            if (routed)
                ASSERT(!transit);
            /* The focus-required pair is also consistent: no route when !focused. */
            if (!focused) {
                ASSERT(!routed);
            }
        }
    }
}

/* ---- resume_model ---------------------------------------------------- */
TEST(resume_gap_matches_spec) {
    for (uint32_t seed = 1; seed <= 8; ++seed) {
        rng_state = seed * 0x9E3779B1u;
        for (int i = 0; i < ITERATIONS; ++i) {
            uint64_t mono = rng_below(10000);
            uint64_t boot = rng_below(10000);
            uint64_t thresh = rng_below(5000);
            uint64_t gap = 123456; /* poison */

            bool got = typio_wl_resume_gap_exceeded(mono, boot, thresh, &gap);

            uint64_t want_gap = (boot > mono) ? (boot - mono) : 0;
            bool want = (boot > mono) && (want_gap >= thresh);

            ASSERT(gap == want_gap);
            ASSERT(got == want);
            /* A fire always corresponds to a strictly positive gap. */
            if (got)
                ASSERT(gap > 0);
        }
    }
}

TEST(resume_cooldown_matches_spec) {
    for (uint32_t seed = 1; seed <= 8; ++seed) {
        rng_state = seed * 2246822519u;
        for (int i = 0; i < ITERATIONS; ++i) {
            uint64_t last = rng_below(100000);
            uint64_t now = last + rng_below(20000); /* now >= last */
            uint64_t cooldown = rng_below(10000);

            bool got = typio_wl_resume_in_cooldown(now, last, cooldown);
            bool want = (last != 0) && (now - last < cooldown);
            ASSERT(got == want);
        }
    }
}

/* Composed resume detector: fires gated by cooldown must never fire twice
 * within a cooldown window. */
TEST(resume_detector_respects_cooldown_window) {
    for (uint32_t seed = 1; seed <= 8; ++seed) {
        rng_state = seed * 22695477u + 1u;
        const uint64_t cooldown = 5000;
        uint64_t last_fire = 0;
        uint64_t prev_fire = 0;
        uint64_t now = 0;

        for (int i = 0; i < ITERATIONS / 4; ++i) {
            now += rng_below(2000);
            /* A would-be fire this tick (gap detector said yes). */
            bool wants_fire = (rng_below(2) == 0);
            if (!wants_fire)
                continue;
            if (typio_wl_resume_in_cooldown(now, last_fire, cooldown))
                continue; /* suppressed */
            /* Actually fires. */
            if (prev_fire != 0)
                ASSERT(now - prev_fire >= cooldown);
            prev_fire = now;
            last_fire = now;
        }
    }
}

/* ---- reconnect_backoff ---------------------------------------------- */
TEST(backoff_is_monotonic_and_bounded_random) {
    for (int i = 0; i < ITERATIONS; ++i) {
        rng_state = (uint32_t)i * 2654435761u + 11u;
        uint32_t a = rng_next();
        uint32_t delay = typio_wl_reconnect_delay_ms(a);
        ASSERT(delay >= TYPIO_WL_RECONNECT_BASE_DELAY_MS ||
               delay == TYPIO_WL_RECONNECT_MAX_DELAY_MS);
        ASSERT(delay <= TYPIO_WL_RECONNECT_MAX_DELAY_MS);
        if (a + 1 != 0) {
            ASSERT(typio_wl_reconnect_delay_ms(a + 1) >= delay ||
                   delay == TYPIO_WL_RECONNECT_MAX_DELAY_MS);
        }
    }
}

int main(void) {
    printf("Running state machine property tests:\n");

    run_test_can_route_keys_exhaustive();
    run_test_can_route_modifiers_exhaustive();
    run_test_is_transitioning_exhaustive();
    run_test_classify_done_exhaustive();
    run_test_predicates_are_mutually_consistent();
    run_test_resume_gap_matches_spec();
    run_test_resume_cooldown_matches_spec();
    run_test_resume_detector_respects_cooldown_window();
    run_test_backoff_is_monotonic_and_bounded_random();

    printf("\n%d/%d tests passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
