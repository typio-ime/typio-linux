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
    include_directories('../src/frontend'),
    include_directories('../src/input/policy'),
    include_directories('../src/input/wayland'),
    include_directories('../src/ui'),
    include_directories('../src/ui/panel'),
    include_directories('../src/engine'),
    include_directories('../src/platform'),
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
    ['input/policy/test_repeat_guard.c', '../src/input/policy/repeat_guard.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('repeat_guard', test_repeat_guard)

test_frontend_aux = executable('test_frontend_aux',
    ['frontend/test_frontend_aux.c', '../src/frontend/frontend_aux.c'],
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
    ['frontend/test_panel_scheduler.c', '../src/frontend/panel_scheduler.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('panel_scheduler', test_panel_scheduler)

test_shortcut_chord = executable('test_shortcut_chord',
    ['input/policy/test_shortcut_chord.c', '../src/input/policy/chords.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('shortcut_chord', test_shortcut_chord)

test_key_arbiter = executable('test_key_arbiter',
    ['input/wayland/test_key_arbiter.c', '../src/input/wayland/arbiter.c',
     '../src/input/policy/chords.c', protocol_headers],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: libtypio_dep,
)
test('key_arbiter', test_key_arbiter)

test_modifier_policy = executable('test_modifier_policy',
    ['input/policy/test_modifier_policy.c', '../src/input/policy/modifiers.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('modifier_policy', test_modifier_policy)

test_key_tracking = executable('test_key_tracking',
    ['input/policy/test_key_tracking.c', '../src/input/policy/tracker.c'],
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

test_session_controller = executable('test_session_controller',
    ['engine/test_session_controller.c', '../src/engine/session_controller.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('session_controller', test_session_controller)

test_reconnect_backoff = executable('test_reconnect_backoff',
    ['engine/test_reconnect_backoff.c', '../src/engine/backoff.c'],
    include_directories: wayland_test_inc,
    dependencies: libtypio_dep,
)
test('reconnect_backoff', test_reconnect_backoff)

test_state_machine_properties = executable('test_state_machine_properties',
    ['engine/test_state_machine_properties.c',
     '../src/engine/session_controller.c',
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
    ['input/wayland/test_vk_bridge.c', '../src/input/wayland/bridge.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: [libtypio_dep, wayland_client_dep, xkbcommon_dep],
)
test('vk_bridge', test_vk_bridge)

test_candidate_guard = executable('test_candidate_guard',
    ['frontend/test_candidate_guard.c', '../src/frontend/candidate_guard.c'],
    include_directories: [wayland_test_inc, top_inc],
    dependencies: libtypio_dep,
)
test('candidate_guard', test_candidate_guard)
