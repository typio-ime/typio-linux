# ── Host tests ──────────────────────────────────────────────────────────────
# typio/ headers + libtypio-core come from libtypio_dep; host internals are
# pulled in by source + include path.
#
# NOTE: many tests depend on the removed TypioEngineManager mock-registration
# API (typio_engine_manager_register_keyboard / _register_voice / etc.) and
# the dropped TypioEngineInfo.version / TypioInstanceConfig.default_engine
# fields. They are commented out here until the daemon adopts the new
# TypioRegistry plugin-only registration path. Re-enable on a per-test basis
# once each one is ported.

wayland_test_inc = [
    include_directories('../src'),
    include_directories('../src/frontend'),
    include_directories('../src/input'),
    include_directories('../src/ui'),
    include_directories('../src/ui/panel'),
    include_directories('../src/engine'),
    include_directories('../src/bridge'),
    include_directories('../src/platform'),
]

test_preedit_format = executable('test_preedit_format',
    ['test_inline_ui.c', '../src/ui/preedit.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('preedit_format', test_preedit_format)

test_startup_guard = executable('test_startup_guard',
    ['test_startup_guard.c', '../src/engine/startup.c',
     '../src/bridge/boundary.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('startup_guard', test_startup_guard)

test_boundary_bridge = executable('test_boundary_bridge',
    ['test_boundary_bridge.c', '../src/bridge/boundary.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('boundary_bridge', test_boundary_bridge)

test_repeat_guard = executable('test_repeat_guard',
    ['test_repeat_guard.c', '../src/input/repeat_guard.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('repeat_guard', test_repeat_guard)

test_frontend_aux = executable('test_frontend_aux',
    ['test_frontend_aux.c', '../src/frontend/frontend_aux.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('frontend_aux', test_frontend_aux)

test_text_ui_state = executable('test_text_ui_state',
    ['test_text_ui_state.c', '../src/ui/state.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('text_ui_state', test_text_ui_state)

test_panel_scheduler = executable('test_panel_scheduler',
    ['test_panel_scheduler.c', '../src/frontend/panel_scheduler.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('panel_scheduler', test_panel_scheduler)

test_shortcut_chord = executable('test_shortcut_chord',
    ['test_shortcut_chord.c', '../src/input/chords.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('shortcut_chord', test_shortcut_chord)

test_key_arbiter = executable('test_key_arbiter',
    ['test_key_arbiter.c', '../src/input/arbiter.c', '../src/input/chords.c',
     protocol_headers],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: libtypio_dep,
)
test('key_arbiter', test_key_arbiter)

test_modifier_policy = executable('test_modifier_policy',
    ['test_modifier_policy.c', '../src/input/modifiers.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('modifier_policy', test_modifier_policy)

test_key_tracking = executable('test_key_tracking',
    ['test_key_tracking.c', '../src/input/tracker.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('key_tracking', test_key_tracking)

test_resume_model = executable('test_resume_model', 'test_resume_model.c',
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('resume_model', test_resume_model)

test_session_controller = executable('test_session_controller',
    ['test_session_controller.c', '../src/engine/session_controller.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('session_controller', test_session_controller)

test_reconnect_backoff = executable('test_reconnect_backoff',
    ['test_reconnect_backoff.c', '../src/engine/backoff.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('reconnect_backoff', test_reconnect_backoff)

test_state_machine_properties = executable('test_state_machine_properties',
    ['test_state_machine_properties.c',
     '../src/engine/session_controller.c',
     '../src/engine/backoff.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('state_machine_properties', test_state_machine_properties)

test_glyph_pack = executable('test_glyph_pack',
    ['test_glyph_pack.c', '../src/ui/panel/glyph_pack.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('glyph_pack', test_glyph_pack)
