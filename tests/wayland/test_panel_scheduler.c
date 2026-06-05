#include "panel_scheduler.h"

#include <stdio.h>
#include <stdlib.h>

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
    } while (0)

#define ASSERT_EQ(a, b) ASSERT((a) == (b))

TEST(mark_dirty_supersedes_retry) {
    ASSERT_EQ(typio_wl_panel_scheduler_mark_dirty(TYPIO_WL_PANEL_SCHEDULE_IDLE),
              TYPIO_WL_PANEL_SCHEDULE_DIRTY);
    ASSERT_EQ(typio_wl_panel_scheduler_mark_dirty(TYPIO_WL_PANEL_SCHEDULE_RETRY),
              TYPIO_WL_PANEL_SCHEDULE_DIRTY);
}

TEST(complete_tracks_present_retry_only) {
    ASSERT_EQ(typio_wl_panel_scheduler_complete(TYPIO_PANEL_UPDATE_OK),
              TYPIO_WL_PANEL_SCHEDULE_IDLE);
    ASSERT_EQ(typio_wl_panel_scheduler_complete(TYPIO_PANEL_UPDATE_FAIL),
              TYPIO_WL_PANEL_SCHEDULE_IDLE);
    ASSERT_EQ(typio_wl_panel_scheduler_complete(TYPIO_PANEL_UPDATE_RETRY),
              TYPIO_WL_PANEL_SCHEDULE_RETRY);
}

TEST(flush_requires_focused_context) {
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_DIRTY,
                                                    true, true, true), true);
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_RETRY,
                                                    true, true, true), true);
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_IDLE,
                                                    true, true, true), false);
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_DIRTY,
                                                    false, true, true), false);
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_DIRTY,
                                                    true, false, true), false);
    ASSERT_EQ(typio_wl_panel_scheduler_should_flush(TYPIO_WL_PANEL_SCHEDULE_DIRTY,
                                                    true, true, false), false);
}

TEST(retry_shortens_poll_timeout_only_when_flushable) {
    ASSERT_EQ(typio_wl_panel_scheduler_poll_timeout_ms(TYPIO_WL_PANEL_SCHEDULE_DIRTY,
                                                       true, 100), 100);
    ASSERT_EQ(typio_wl_panel_scheduler_poll_timeout_ms(TYPIO_WL_PANEL_SCHEDULE_RETRY,
                                                       false, 100), 100);
    ASSERT_EQ(typio_wl_panel_scheduler_poll_timeout_ms(TYPIO_WL_PANEL_SCHEDULE_RETRY,
                                                       true, 100), 16);
    ASSERT_EQ(typio_wl_panel_scheduler_poll_timeout_ms(TYPIO_WL_PANEL_SCHEDULE_RETRY,
                                                       true, 8), 8);
}

int main(void) {
    printf("Running panel scheduler tests:\n");
    run_test_mark_dirty_supersedes_retry();
    run_test_complete_tracks_present_retry_only();
    run_test_flush_requires_focused_context();
    run_test_retry_shortens_poll_timeout_only_when_flushable();
    printf("\nPassed %d/%d tests\n", tests_passed, tests_run);
    return 0;
}
