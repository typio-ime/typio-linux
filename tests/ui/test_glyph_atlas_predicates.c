/*
 * Regression tests for the glyph-atlas pure decision predicates + the
 * queue/flush mechanics (src/ui/panel/glyph_atlas.c).
 *
 * The bugs these guard against:
 *
 *   - glyph_atlas_reclaim historically only honoured the packer-exhaustion
 *     trigger even though the header documented "75% load OR packer
 *     exhaustion". The load-factor arm is the one that fires when many small
 *     glyphs fill the hash table without saturating the texture, lengthening
 *     probe chains toward O(n) per glyph.
 *
 *   - The batched-upload queue (the fix that turned N per-glyph vkQueueSubmit
 *     round-trips into one per frame) had no observable behaviour without a
 *     GPU. We expose three test-only hooks (pending-count, upload-fn
 *     injection, slot-only init) so the queue contract can be verified on a
 *     CPU-only runner.
 *
 *   - The flush failure path (slot.mark_drawable=false) had no test surface
 *     because it required both a GPU and an induced failure. The upload-fn
 *     injection lets a stub return false to verify the marking.
 *
 * Pure CPU; no GPU, no FreeType, no Fontconfig.
 */

#include "glyph_atlas.h"

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

/* ── glyph_atlas_should_reclaim: the load-factor contract ─────────────────── */

TEST(reclaim_neither_trigger) {
    /* Healthy atlas: low load, packer not exhausted. Must NOT reclaim. */
    CHECK(!glyph_atlas_should_reclaim(0,     false, 131072, 75));
    CHECK(!glyph_atlas_should_reclaim(1000,  false, 131072, 75));
    CHECK(!glyph_atlas_should_reclaim(98303, false, 131072, 75));  /* 74.99% */
}

TEST(reclaim_packer_exhausted_only) {
    /* Packer exhausted but load factor well under threshold: must reclaim.
     * This was the ONLY trigger the historical implementation honoured. */
    CHECK(glyph_atlas_should_reclaim(0,    true, 131072, 75));
    CHECK(glyph_atlas_should_reclaim(100,  true, 131072, 75));
}

TEST(reclaim_load_factor_only) {
    /* THIS is the regression case the historical bug missed: load factor
     * crossed 75% but the texture still has shelf space. The header
     * documented the trigger; the implementation ignored it. Verifying it
     * now fires is the direct guard against reintroducing the bug. */
    CHECK(glyph_atlas_should_reclaim(98304, false, 131072, 75));  /* exactly 75% */
    CHECK(glyph_atlas_should_reclaim(120000, false, 131072, 75));
    CHECK(glyph_atlas_should_reclaim(131072, false, 131072, 75));  /* 100% */
}

TEST(reclaim_both_triggers) {
    /* Both fire: the "reason" string in the debug log distinguishes, but the
     * predicate just returns true. */
    CHECK(glyph_atlas_should_reclaim(98304, true, 131072, 75));
    CHECK(glyph_atlas_should_reclaim(131072, true, 131072, 75));
}

TEST(reclaim_threshold_edge_is_inclusive) {
    /* The threshold is "live_count >= threshold", so exactly at 75% we
     * reclaim (not strict-greater-than). This matches the historical comment
     * and avoids leaving a single entry stranded at the boundary. */
    uint32_t cap = 1000;
    uint32_t threshold_75 = (uint32_t)((uint64_t)cap * 75 / 100);  /* 750 */
    CHECK(threshold_75 == 750);
    CHECK(!glyph_atlas_should_reclaim(749, false, cap, 75));
    CHECK(glyph_atlas_should_reclaim(750, false, cap, 75));
    CHECK(glyph_atlas_should_reclaim(751, false, cap, 75));
}

TEST(reclaim_zero_capacity_is_safe) {
    /* Defensive: a caller with a zero-capacity table must not divide by zero.
     * Returns false because there is nothing to reclaim. */
    CHECK(!glyph_atlas_should_reclaim(0, false, 0, 75));
    CHECK(!glyph_atlas_should_reclaim(10, false, 0, 75));
}

/* ── Queue mechanics (no GPU via upload-fn injection) ─────────────────────── */

typedef struct {
    uint32_t   call_count;
    size_t     last_count;
    bool       next_result;
    uint32_t   fail_after_n_calls;   /* 0 = never fail */
    uint32_t   call_index;
} UploadStub;

static bool stub_upload(const GlyphUploadRegion *regions, size_t count, void *user)
{
    UploadStub *s = (UploadStub *)user;
    s->call_count++;
    s->call_index++;
    s->last_count = count;
    if (s->fail_after_n_calls != 0 && s->call_index >= s->fail_after_n_calls) {
        return false;
    }
    (void)regions;
    return s->next_result;
}

static void reset_stub(UploadStub *s, bool result)
{
    memset(s, 0, sizeof(*s));
    s->next_result = result;
}

TEST(flush_empty_queue_is_noop) {
    /* Flush on an empty queue must NOT call the upload backend and must
     * return true. This is the steady-state path: a warmed atlas has zero
     * misses per frame, so flush is invoked by do_present but does nothing. */
    UploadStub stub;
    reset_stub(&stub, true);
    glyph_atlas_test_reset();
    glyph_atlas_set_upload_fn(stub_upload, &stub);

    CHECK(glyph_atlas_pending_count() == 0);
    CHECK(glyph_atlas_flush() == true);
    CHECK(stub.call_count == 0);   /* no work, no submit */

    glyph_atlas_set_upload_fn(NULL, NULL);
    glyph_atlas_test_reset();
}

TEST(flush_batches_into_one_submit) {
    /* Push N entries; flush must call the backend EXACTLY ONCE with count==N.
     * This is the headline contract of the batched-upload fix: previously
     * each cache miss was its own vkQueueSubmit, so a 10-row CJK panel
     * re-warming after an atlas reclaim did ~tens of submits per frame. */
    UploadStub stub;
    reset_stub(&stub, true);
    glyph_atlas_test_reset();
    glyph_atlas_set_upload_fn(stub_upload, &stub);

    const uint32_t N = 32;
    for (uint32_t i = 0; i < N; ++i) {
        /* Synthetic 1-byte payload; the stub doesn't read it. */
        uint8_t *byte = (uint8_t *)malloc(1);
        CHECK(byte != NULL);
        *byte = (uint8_t)i;
        CHECK(glyph_atlas_test_push_pending(i, /*x=*/i, /*y=*/0,
                                              /*w=*/1, /*h=*/1,
                                              byte, /*bytes=*/1));
    }
    CHECK(glyph_atlas_pending_count() == N);

    CHECK(glyph_atlas_flush() == true);
    CHECK(stub.call_count == 1);
    CHECK(stub.last_count == N);
    CHECK(glyph_atlas_pending_count() == 0);

    glyph_atlas_set_upload_fn(NULL, NULL);
    glyph_atlas_test_reset();
}

TEST(flush_marks_slots_non_drawable_on_failure) {
    /* When the upload backend fails (fence timeout, OOM, simulated here),
     * every queued slot must be marked non-drawable so the render pass
     * samples nothing instead of garbage. The slot-marking path needs
     * g_atlas.slots to exist; test_init_slots allocates just that array
     * without a GPU. */
    UploadStub stub;
    reset_stub(&stub, false);   /* every call fails */
    glyph_atlas_test_reset();
    glyph_atlas_test_init_slots();
    glyph_atlas_set_upload_fn(stub_upload, &stub);

    /* Pre-mark slots as drawable so we can observe the failure flipping them. */
    for (uint32_t i = 0; i < 4; ++i) {
        glyph_atlas_test_set_drawable(i, true);
    }

    for (uint32_t i = 0; i < 4; ++i) {
        uint8_t *byte = (uint8_t *)malloc(1);
        CHECK(byte != NULL);
        CHECK(glyph_atlas_test_push_pending(/*slot_index=*/i, i, 0, 1, 1,
                                              byte, 1));
    }

    CHECK(glyph_atlas_flush() == false);   /* failure surfaces as false */
    CHECK(stub.call_count == 1);

    for (uint32_t i = 0; i < 4; ++i) {
        CHECK(glyph_atlas_test_get_drawable(i) == false);
    }

    glyph_atlas_set_upload_fn(NULL, NULL);
    glyph_atlas_test_reset();
}

TEST(flush_failure_with_no_slots_is_safe) {
    /* The failure path must be NULL-safe: a queue-only test environment that
     * never called test_init_slots should not crash when flush fails. */
    UploadStub stub;
    reset_stub(&stub, false);
    glyph_atlas_test_reset();        /* no slots allocated */
    glyph_atlas_set_upload_fn(stub_upload, &stub);

    for (uint32_t i = 0; i < 3; ++i) {
        uint8_t *byte = (uint8_t *)malloc(1);
        CHECK(byte != NULL);
        CHECK(glyph_atlas_test_push_pending(i, i, 0, 1, 1, byte, 1));
    }
    CHECK(glyph_atlas_flush() == false);   /* must not crash */
    CHECK(glyph_atlas_pending_count() == 0);

    glyph_atlas_set_upload_fn(NULL, NULL);
    glyph_atlas_test_reset();
}

TEST(push_overflow_triggers_inline_flush) {
    /* Pushing past GLYPH_PENDING_CAP must inline-flush to make room rather
     * than crash or silently drop entries. The overflow is unreachable in
     * production (a single panel frame cannot surface >1024 unique glyphs)
     * but the contract is to degrade gracefully. */
    UploadStub stub;
    reset_stub(&stub, true);
    glyph_atlas_test_reset();
    glyph_atlas_set_upload_fn(stub_upload, &stub);

    /* Fill exactly to the cap. */
    for (uint32_t i = 0; i < GLYPH_PENDING_CAP; ++i) {
        uint8_t *byte = (uint8_t *)malloc(1);
        CHECK(byte != NULL);
        CHECK(glyph_atlas_test_push_pending(i, i, 0, 1, 1, byte, 1));
    }
    CHECK(glyph_atlas_pending_count() == GLYPH_PENDING_CAP);
    CHECK(stub.call_count == 0);   /* no overflow yet */

    /* One more push: triggers the inline flush, then the push succeeds into
     * the now-empty queue. The stub should have been called once (the inline
     * flush) with count == GLYPH_PENDING_CAP. */
    uint8_t *extra = (uint8_t *)malloc(1);
    CHECK(extra != NULL);
    CHECK(glyph_atlas_test_push_pending(0, 0, 0, 1, 1, extra, 1));
    CHECK(stub.call_count == 1);
    CHECK(stub.last_count == GLYPH_PENDING_CAP);
    CHECK(glyph_atlas_pending_count() == 1);   /* the extra entry */

    glyph_atlas_set_upload_fn(NULL, NULL);
    glyph_atlas_test_reset();
}

int main(void) {
    printf("Running glyph_atlas predicate + queue tests:\n");
    run_test_reclaim_neither_trigger();
    run_test_reclaim_packer_exhausted_only();
    run_test_reclaim_load_factor_only();
    run_test_reclaim_both_triggers();
    run_test_reclaim_threshold_edge_is_inclusive();
    run_test_reclaim_zero_capacity_is_safe();
    run_test_flush_empty_queue_is_noop();
    run_test_flush_batches_into_one_submit();
    run_test_flush_marks_slots_non_drawable_on_failure();
    run_test_flush_failure_with_no_slots_is_safe();
    run_test_push_overflow_triggers_inline_flush();
    printf("\nPassed %d/%d tests\n", tests_passed, tests_run);
    return 0;
}
