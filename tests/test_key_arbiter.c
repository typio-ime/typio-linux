/**
 * @file test_key_arbiter.c
 * @brief Tests for key event arbiter state machine
 */

#include "input/wayland/arbiter.h"
#include "internal.h"
#include "typio/abi/event.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ── Test framework ─────────────────────────────────────────────── */

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

/* ── Mock tracking ──────────────────────────────────────────────── */

#define MAX_RECORDED 32

typedef struct {
    bool is_press;
    uint32_t key;
    uint32_t keysym;
    uint32_t modifiers;
    uint32_t unicode;
    uint32_t time;
} RecordedKeyEvent;

typedef struct {
    uint32_t key;
    uint32_t time;
    uint32_t state;
} RecordedVkEvent;

static RecordedKeyEvent recorded_keys[MAX_RECORDED];
static size_t recorded_key_count;
static RecordedVkEvent recorded_vk[MAX_RECORDED];
static size_t recorded_vk_count;
static int engine_switch_count;

static void reset_mocks(void) {
    recorded_key_count = 0;
    recorded_vk_count = 0;
    engine_switch_count = 0;
}

/* ── Mock implementations ───────────────────────────────────────── */

/* Called by arbiter on passthrough / replay */
void typio_wl_keyboard_process_key_press(TypioWlKeyboard *keyboard,
                                         TypioWlSession *session,
                                         uint32_t key, uint32_t keysym,
                                         uint32_t modifiers, uint32_t unicode,
                                         uint32_t time) {
    (void)keyboard; (void)session;
    if (recorded_key_count < MAX_RECORDED) {
        recorded_keys[recorded_key_count++] = (RecordedKeyEvent){
            .is_press = true, .key = key, .keysym = keysym,
            .modifiers = modifiers, .unicode = unicode, .time = time,
        };
    }
    /* Simulate what real key_route does for modifiers: set tracking-forwarded */
    TypioWlFrontend *fe = keyboard->frontend;
    if (key < TYPIO_WL_MAX_TRACKED_KEYS)
        fe->tracker->states[key] = TYPIO_KEY_TRACK_FORWARDED;
}

void typio_wl_keyboard_process_key_release(TypioWlKeyboard *keyboard,
                                           TypioWlSession *session,
                                           uint32_t key, uint32_t keysym,
                                           uint32_t modifiers, uint32_t unicode,
                                           uint32_t time) {
    (void)keyboard; (void)session;
    if (recorded_key_count < MAX_RECORDED) {
        recorded_keys[recorded_key_count++] = (RecordedKeyEvent){
            .is_press = false, .key = key, .keysym = keysym,
            .modifiers = modifiers, .unicode = unicode, .time = time,
        };
    }
    TypioWlFrontend *fe = keyboard->frontend;
    if (key < TYPIO_WL_MAX_TRACKED_KEYS)
        fe->tracker->states[key] = TYPIO_KEY_TRACK_IDLE;
}

/* Called by arbiter_release_orphaned_keys */
void typio_wl_vk_forward_key(struct TypioWlKeyboard *keyboard,
                              uint32_t time, uint32_t key,
                              uint32_t state, uint32_t unicode) {
    (void)keyboard; (void)unicode;
    if (recorded_vk_count < MAX_RECORDED) {
        recorded_vk[recorded_vk_count++] = (RecordedVkEvent){
            .key = key, .time = time, .state = state,
        };
    }
}

/* Called by arbiter_consume → typio_registry_next_keyboard */
static int fake_registry;  /* just a non-NULL pointer target */
TypioRegistry *typio_instance_get_registry(
    [[maybe_unused]] TypioInstance *instance) {
    return (TypioRegistry *)&fake_registry;
}

TypioResult typio_registry_next_keyboard(
    [[maybe_unused]] TypioRegistry *registry) {
    engine_switch_count++;
    return TYPIO_OK;
}

/* Composition / preedit teardown stubs hit by arbiter_consume before the
 * engine switch. The arbiter only needs them to return cleanly. */
void typio_wl_set_preedit([[maybe_unused]] TypioWlFrontend *frontend,
                          [[maybe_unused]] const char *text,
                          [[maybe_unused]] int cursor_begin,
                          [[maybe_unused]] int cursor_end) {
}

void typio_wl_commit([[maybe_unused]] TypioWlFrontend *frontend) {
}

void typio_wl_panel_coordinator_hide([[maybe_unused]] TypioWlFrontend *frontend,
                                     [[maybe_unused]] TypioWlUiOwner owner) {
}

void typio_wl_session_cancel_ui_tracking(
    [[maybe_unused]] TypioWlSession *session) {
}

/* Trace stub */
void typio_wl_trace(
    [[maybe_unused]] TypioWlFrontend *frontend,
    [[maybe_unused]] const char *category,
    [[maybe_unused]] const char *format, ...) {
}

/* ── Test helpers ───────────────────────────────────────────────── */

/* Use fixed keycodes for Ctrl_L=29, Shift_L=42 (typical evdev) */
#define KC_CTRL  29
#define KC_SHIFT 42
#define KC_A     30

static TypioWlFrontend test_frontend;
static TypioWlKeyboard test_keyboard;
static TypioWlSession test_session;

static void setup(void) {
    memset(&test_frontend, 0, sizeof(test_frontend));
    memset(&test_keyboard, 0, sizeof(test_keyboard));
    memset(&test_session, 0, sizeof(test_session));
    test_frontend.tracker = calloc(1, sizeof(TypioWlKeyTracker));
    test_keyboard.frontend = &test_frontend;
    /* Default engine-switch chord is Ctrl+Shift (modifier-only). Set it
     * directly so the test stays independent of the config loader. */
    test_frontend.shortcuts.switch_engine = (TypioShortcutBinding){
        .modifiers = TYPIO_MOD_CTRL | TYPIO_MOD_SHIFT,
        .keysym = 0,
    };
    typio_wl_key_arbiter_init(&test_keyboard.arbiter);
    reset_mocks();
}

static void press(uint32_t key, uint32_t keysym, uint32_t time) {
    /* Simulate physical modifier update (done before arbiter in real code) */
    if (keysym == TYPIO_KEY_Control_L || keysym == TYPIO_KEY_Control_R)
        test_keyboard.physical_modifiers |= TYPIO_MOD_CTRL;
    if (keysym == TYPIO_KEY_Shift_L || keysym == TYPIO_KEY_Shift_R)
        test_keyboard.physical_modifiers |= TYPIO_MOD_SHIFT;
    if (keysym == TYPIO_KEY_Alt_L || keysym == TYPIO_KEY_Alt_R)
        test_keyboard.physical_modifiers |= TYPIO_MOD_ALT;

    uint32_t mods = test_keyboard.physical_modifiers;
    typio_wl_key_arbiter_press(&test_keyboard.arbiter, &test_keyboard,
                               &test_session, key, keysym, mods, 0, time);
}

static void release(uint32_t key, uint32_t keysym, uint32_t time) {
    /* Simulate physical modifier update */
    if (keysym == TYPIO_KEY_Control_L || keysym == TYPIO_KEY_Control_R)
        test_keyboard.physical_modifiers &= ~(uint32_t)TYPIO_MOD_CTRL;
    if (keysym == TYPIO_KEY_Shift_L || keysym == TYPIO_KEY_Shift_R)
        test_keyboard.physical_modifiers &= ~(uint32_t)TYPIO_MOD_SHIFT;
    if (keysym == TYPIO_KEY_Alt_L || keysym == TYPIO_KEY_Alt_R)
        test_keyboard.physical_modifiers &= ~(uint32_t)TYPIO_MOD_ALT;

    uint32_t mods = test_keyboard.physical_modifiers;
    typio_wl_key_arbiter_release(&test_keyboard.arbiter, &test_keyboard,
                                 &test_session, key, keysym, mods, 0, time);
}

/* ── Tests ──────────────────────────────────────────────────────── */

TEST(idle_passthrough) {
    setup();
    /* Single Ctrl press — should pass through immediately */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(recorded_key_count == 1);
    ASSERT(recorded_keys[0].is_press == true);
    ASSERT(recorded_keys[0].key == KC_CTRL);

    release(KC_CTRL, TYPIO_KEY_Control_L, 200);
    ASSERT(recorded_key_count == 2);
    ASSERT(recorded_keys[1].is_press == false);
    ASSERT(engine_switch_count == 0);
}

TEST(chord_consume) {
    setup();
    /* Ctrl+Shift chord: Ctrl↓ Shift↓ Shift↑ Ctrl↑ */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(recorded_key_count == 1);  /* Ctrl passed through */

    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);
    ASSERT(recorded_key_count == 1);  /* Shift buffered, not dispatched */

    release(KC_SHIFT, TYPIO_KEY_Shift_L, 200);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);
    ASSERT(recorded_key_count == 1);  /* Still buffered */

    release(KC_CTRL, TYPIO_KEY_Control_L, 210);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(engine_switch_count == 1);
    /* Engine should never have seen Shift press/release */
    ASSERT(recorded_key_count == 1);  /* Only the initial Ctrl pass-through */
}

TEST(chord_consume_releases_orphaned_key) {
    setup();
    /* Ctrl↓ (forwarded) → Shift↓ (buffered) → Shift↑ → Ctrl↑ (consume) */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    ASSERT(test_frontend.tracker->states[KC_CTRL] == TYPIO_KEY_TRACK_FORWARDED);

    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    release(KC_SHIFT, TYPIO_KEY_Shift_L, 200);
    release(KC_CTRL, TYPIO_KEY_Control_L, 210);

    /* The Ctrl release should have been forwarded to vk */
    ASSERT(recorded_vk_count == 1);
    ASSERT(recorded_vk[0].key == KC_CTRL);
    /* Key state should be cleared */
    ASSERT(test_frontend.tracker->states[KC_CTRL] == TYPIO_KEY_TRACK_IDLE);
}

TEST(chord_reverse_order) {
    setup();
    /* Shift first, then Ctrl: Shift↓ Ctrl↓ Ctrl↑ Shift↑ */
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 100);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(recorded_key_count == 1);

    press(KC_CTRL, TYPIO_KEY_Control_L, 110);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);

    release(KC_CTRL, TYPIO_KEY_Control_L, 200);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);

    release(KC_SHIFT, TYPIO_KEY_Shift_L, 210);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(engine_switch_count == 1);

    /* Shift was forwarded before buffering; its release was consumed.
     * VK should have received the release. */
    ASSERT(recorded_vk_count == 1);
    ASSERT(recorded_vk[0].key == KC_SHIFT);
}

TEST(cancel_on_non_modifier) {
    setup();
    /* Ctrl↓ Shift↓ A↓ — should replay Shift and process A */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);

    size_t before = recorded_key_count;
    press(KC_A, 0x61 /* XKB_KEY_a */, 120);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(engine_switch_count == 0);
    /* Should have replayed Shift press + dispatched A press */
    ASSERT(recorded_key_count == before + 2);
    ASSERT(recorded_keys[before].is_press == true);
    ASSERT(recorded_keys[before].keysym == TYPIO_KEY_Shift_L);
    ASSERT(recorded_keys[before + 1].is_press == true);
    ASSERT(recorded_keys[before + 1].keysym == 0x61 /* XKB_KEY_a */);
}

TEST(cancel_on_alt) {
    setup();
    /* Ctrl↓ Shift↓ Alt↓ — cancel because Alt appeared */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);

    size_t before = recorded_key_count;
    press(56, TYPIO_KEY_Alt_L, 120);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(engine_switch_count == 0);
    /* Replayed Shift + dispatched Alt */
    ASSERT(recorded_key_count == before + 2);
}

TEST(timestamps_preserved_on_replay) {
    setup();
    press(KC_CTRL, TYPIO_KEY_Control_L, 1000);
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 1050);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);

    /* Cancel with non-modifier */
    press(KC_A, 0x61 /* XKB_KEY_a */, 1100);

    /* The replayed Shift should have original timestamp 1050 */
    ASSERT(recorded_keys[1].time == 1050);
    ASSERT(recorded_keys[1].keysym == TYPIO_KEY_Shift_L);
    /* The A should have timestamp 1100 */
    ASSERT(recorded_keys[2].time == 1100);
}

TEST(no_engine_events_on_consume) {
    setup();
    /* Full chord — engine should see ZERO key events from the buffered period */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    size_t after_ctrl = recorded_key_count;

    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    release(KC_SHIFT, TYPIO_KEY_Shift_L, 200);
    release(KC_CTRL, TYPIO_KEY_Control_L, 210);

    /* No additional key events dispatched after Ctrl pass-through */
    ASSERT(recorded_key_count == after_ctrl);
    ASSERT(engine_switch_count == 1);
}

TEST(reset_clears_mid_chord_buffering) {
    setup();
    /* Begin a chord but stop mid-buffer (Ctrl held, Shift buffered) — this is
     * the state a focus-out can leave behind: the soft pause retains the grab,
     * so the arbiter is not torn down. */
    press(KC_CTRL, TYPIO_KEY_Control_L, 100);
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 110);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_BUFFERING);
    ASSERT(test_keyboard.arbiter.buffer_count > 0);

    /* typio_wl_keyboard_pause() calls this on the focus-out boundary. It must
     * return the FSM to IDLE with an empty buffer so the retained grab does not
     * carry a half-formed chord into the next activation. */
    typio_wl_key_arbiter_reset(&test_keyboard.arbiter);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(test_keyboard.arbiter.buffer_count == 0);

    /* Simulate the modifier scrub the pause also performs, then prove a fresh
     * chord on the next activation consumes cleanly — no stale state misfires
     * or suppresses the switch. */
    test_keyboard.physical_modifiers = 0;
    reset_mocks();

    press(KC_CTRL, TYPIO_KEY_Control_L, 300);
    press(KC_SHIFT, TYPIO_KEY_Shift_L, 310);
    release(KC_SHIFT, TYPIO_KEY_Shift_L, 320);
    release(KC_CTRL, TYPIO_KEY_Control_L, 330);
    ASSERT(test_keyboard.arbiter.state == TYPIO_ARBITER_IDLE);
    ASSERT(engine_switch_count == 1);
}

/* ── Main ───────────────────────────────────────────────────────── */

int main(void) {
    printf("Running key arbiter tests:\n");
    run_test_idle_passthrough();
    run_test_chord_consume();
    run_test_chord_consume_releases_orphaned_key();
    run_test_chord_reverse_order();
    run_test_cancel_on_non_modifier();
    run_test_cancel_on_alt();
    run_test_timestamps_preserved_on_replay();
    run_test_no_engine_events_on_consume();
    run_test_reset_clears_mid_chord_buffering();
    printf("\n%d/%d tests passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
