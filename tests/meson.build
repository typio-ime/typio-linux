# ── Host tests ──────────────────────────────────────────────────────────────
# typio/ headers + libtypio-core come from libtypio_dep; host internals are
# pulled in by source + include path.
#
# Tests are organised by the subsystem they exercise, mirroring the
# src/ layout. The four integration tests (test_key_route, test_uds_ipc,
# test_server_app, test_recent_log_dump, test_startup_health) need the
# TypioRegistry plugin-only registration path that the daemon has not
# yet adopted, and are not wired into the build. test_key_route
# additionally still references the old `TypioEngineManager` API
# (replaced by `TypioRegistry`) and the removed
# `TypioWlTextUiBackend` shim, so it needs a port before it can
# compile.

wayland_test_inc = [
    include_directories('../src'),
    include_directories('../src/wayland'),
    include_directories('../src/wayland/keyboard/policy'),
    include_directories('../src/wayland/keyboard'),
    include_directories('../src/ui'),
    include_directories('../src/ui/panel'),
    include_directories('../src/engine'),
]

test_preedit_format = executable('test_preedit_format',
    ['ui/test_inline_ui.c', '../src/ui/preedit.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('preedit_format', test_preedit_format)

test_startup_guard = executable('test_startup_guard',
    ['engine/test_startup_guard.c', '../src/engine/startup.c',
     '../src/engine/boundary.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('startup_guard', test_startup_guard)

test_boundary_bridge = executable('test_boundary_bridge',
    ['engine/test_boundary_bridge.c', '../src/engine/boundary.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('boundary_bridge', test_boundary_bridge)

test_repeat_guard = executable('test_repeat_guard',
    ['wayland/keyboard/policy/test_repeat_guard.c', '../src/wayland/keyboard/policy/repeat_guard.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('repeat_guard', test_repeat_guard)

test_frontend_aux = executable('test_frontend_aux',
    ['wayland/test_frontend_aux.c', '../src/wayland/frontend_aux.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('frontend_aux', test_frontend_aux)

test_text_ui_state = executable('test_text_ui_state',
    ['ui/test_text_ui_state.c', '../src/ui/state.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('text_ui_state', test_text_ui_state)

test_panel_scheduler = executable('test_panel_scheduler',
    ['wayland/test_panel_scheduler.c', '../src/wayland/panel_scheduler.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('panel_scheduler', test_panel_scheduler)

test_shortcut_chord = executable('test_shortcut_chord',
    ['wayland/keyboard/policy/test_shortcut_chord.c', '../src/wayland/keyboard/policy/chords.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('shortcut_chord', test_shortcut_chord)

test_key_arbiter = executable('test_key_arbiter',
    ['wayland/keyboard/test_key_arbiter.c', '../src/wayland/keyboard/arbiter.c',
     '../src/wayland/keyboard/policy/chords.c', protocol_headers],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: libtypio_dep,
)
test('key_arbiter', test_key_arbiter)

test_modifier_policy = executable('test_modifier_policy',
    ['wayland/keyboard/policy/test_modifier_policy.c', '../src/wayland/keyboard/policy/modifiers.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('modifier_policy', test_modifier_policy)

test_key_tracking = executable('test_key_tracking',
    ['wayland/keyboard/policy/test_key_tracking.c', '../src/wayland/keyboard/policy/tracker.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('key_tracking', test_key_tracking)

test_resume_model = executable('test_resume_model',
    'engine/test_resume_model.c',
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('resume_model', test_resume_model)

test_focus_controller = executable('test_focus_controller',
    ['engine/test_focus_controller.c', '../src/engine/focus_controller.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('focus_controller', test_focus_controller)

test_reconnect_backoff = executable('test_reconnect_backoff',
    ['engine/test_reconnect_backoff.c', '../src/engine/backoff.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('reconnect_backoff', test_reconnect_backoff)

test_state_machine_properties = executable('test_state_machine_properties',
    ['engine/test_state_machine_properties.c',
     '../src/engine/focus_controller.c',
     '../src/engine/backoff.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('state_machine_properties', test_state_machine_properties)

test_glyph_pack = executable('test_glyph_pack',
    ['ui/test_glyph_pack.c', '../src/ui/panel/glyph_pack.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('glyph_pack', test_glyph_pack)

test_vk_bridge = executable('test_vk_bridge',
    ['wayland/keyboard/test_vk_bridge.c', '../src/wayland/keyboard/bridge.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: [libtypio_dep, wayland_client_dep, xkbcommon_dep],
)
test('vk_bridge', test_vk_bridge)

test_candidate_guard = executable('test_candidate_guard',
    ['wayland/test_candidate_guard.c', '../src/wayland/candidate_guard.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: libtypio_dep,
)
test('candidate_guard', test_candidate_guard)

# ── Tray ──────────────────────────────────────────────────────────────────
# The language-badge rasteriser needs FreeType + HarfBuzz + Fontconfig. Resolve
# them locally (test scope does not see src/'s deps) and only register the test
# when present, so a minimal toolchain still builds the rest of the suite.
badge_ft = dependency('freetype2', required: false)
badge_hb = dependency('harfbuzz', required: false)
badge_fc = dependency('fontconfig', required: false)
if badge_ft.found() and badge_hb.found() and badge_fc.found()
    test_icon_badge = executable('test_icon_badge',
        ['tray/test_icon_badge.c', '../src/tray/icon_badge.c'],
        include_directories: [wayland_test_inc, top_inc],
        dependencies: [libtypio_dep, badge_ft, badge_hb, badge_fc],
    )
    test('icon_badge', test_icon_badge)
endif

# Menu model structure tests (ADR-0033): builds a TypioInstance fixture with
# synthetic engines/languages and asserts the resulting in-memory menu tree's
# shape (IDs, labels, radio state, submenu nesting, orphan section, script
# disambiguation). Decoupled from sd_bus — that side is verified end-to-end.
test_menu_model = executable('test_menu_model',
    ['tray/test_menu_model.c',
     '../src/tray/menu_model.c',
     '../src/state/controller.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: [libtypio_dep],
)
test('menu_model', test_menu_model)

# ── State controller ──────────────────────────────────────────────────────
# Pure-function tests for the icon resolution chain (ADR-0033). The resolver
# takes primitive inputs + a TypioConfig, so no TypioInstance fixture is
# needed. controller.c is the only source pulled in.
test_controller_icon = executable('test_controller_icon',
    ['state/test_controller_icon.c', '../src/state/controller.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: [libtypio_dep],
)
test('controller_icon', test_controller_icon)

# Pure-function tests for the language display helpers (endonym, badge,
# menu label with script disambiguation). Same source set as the icon test.
test_language_helpers = executable('test_language_helpers',
    ['state/test_language_helpers.c', '../src/state/controller.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: [libtypio_dep],
)
test('language_helpers', test_language_helpers)

# ── Fuzzers ─────────────────────────────────────────────────────────────────
# Not run by `meson test`; build with -Denable_fuzzers=true and run the
# binaries directly (see docs/dev/testing.md).
if get_option('enable_fuzzers')
    if cc.get_id() != 'clang'
        error('-Denable_fuzzers=true requires clang (libFuzzer)')
    endif
    fuzz_sanitize = ['-fsanitize=fuzzer,address,undefined']
    executable('fuzz_tip_json',
        ['fuzz/fuzz_tip_json.c', '../src/ipc/tip_json.c'],
        include_directories: wayland_test_inc,
        c_args: fuzz_sanitize,
        link_args: fuzz_sanitize,
    )
endif
