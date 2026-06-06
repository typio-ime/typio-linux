# ── Typio Wayland host ───────────────────────────────────────────

# ── Wayland protocol generation ───────────────────────────────────────────
protocols = []
# Header-only subset of the generated protocol targets, exported for host
# unit tests that include internal.h (which pulls the client-protocol headers)
# but do not link the protocol marshalling code.
protocol_headers = []
if enable_wayland
    protocol_xmls = {
        'input-method-unstable-v2': meson.project_source_root() / 'protocols' / 'input-method-unstable-v2.xml',
        'virtual-keyboard-unstable-v1': meson.project_source_root() / 'protocols' / 'virtual-keyboard-unstable-v1.xml',
        'fractional-scale-v1': meson.project_source_root() / 'protocols' / 'fractional-scale-v1.xml',
        'viewporter': meson.project_source_root() / 'protocols' / 'viewporter.xml',
        'ext-foreign-toplevel-list-v1': meson.project_source_root() / 'protocols' / 'ext-foreign-toplevel-list-v1.xml',
    }

    foreach name, xml : protocol_xmls
        header = custom_target(name + '-client-header',
            output: name + '-client-protocol.h',
            input: xml,
            command: [wayland_scanner, 'client-header', '@INPUT@', '@OUTPUT@'],
        )
        code = custom_target(name + '-protocol-code',
            output: name + '-protocol.c',
            input: xml,
            command: [wayland_scanner, 'private-code', '@INPUT@', '@OUTPUT@'],
        )
        protocols += [header, code]
        protocol_headers += [header]
    endforeach
endif

# ── Source files ──────────────────────────────────────────────────────────
host_sources = files(
    'main.c',
    'app.c',
    'engine_loader.c',
    'state/controller.c',
    'state/engine_placeholder.c',
    'cli.c',
    'notify/health.c',
    'ipc/tip_protocol.c',
    'ipc/tip_json.c',
    'ipc/uds_server.c',
    'ipc/ipc_bus.c',
    'state/service.c',
)

if enable_wayland
    host_sources += files(
        'wayland/frontend_aux.c',
        'wayland/candidate_guard.c',
        'engine/boundary.c',
        'wayland/foreign/identity.c',
        'wayland/keyboard/arbiter.c',
        'wayland/keyboard/debug.c',
        'wayland/keyboard/policy/tracker.c',
        'wayland/keyboard/router.c',
        'wayland/keyboard/repeat.c',
        'engine/session_controller.c',
        'wayland/session_effects.c',
        'wayland/keyboard/policy/modifiers.c',
        'wayland/event_loop.c',
        'wayland/panel_scheduler.c',
        'wayland/panel_coordinator.c',
        'wayland/runtime_config.c',
        'wayland/watchdog.c',
        'wayland/keyboard/xkb.c',
        'wayland/aux_handler.c',
        'wayland/aux_adapters.c',
        'wayland/frontend_bind.c',
        'wayland/frontend_runtime.c',
        'ui/state.c',
        'engine/backoff.c',
        'wayland/keyboard/policy/repeat_guard.c',
        'engine/logind/resume.c',
        'wayland/keyboard/policy/chords.c',
        'engine/startup.c',
        'wayland/keyboard/bridge.c',
        'wayland/frontend.c',
        'wayland/indicator.c',
        'ui/preedit.c',
        'wayland/input_method.c',
        'wayland/keyboard/keyboard.c',
        'wayland/shortcut.c',
        'wayland/trace.c',
    )

    if flux_dep.found()
        host_sources += files(
            'ui/panel/panel.c',
            'ui/panel/surface.c',
            'ui/panel/layout.c',
            'ui/panel/paint.c',
            'ui/panel/text_shaper.c',
            'ui/panel/glyph_atlas.c',
            'ui/panel/glyph_upload.c',
            'ui/panel/font_cache.c',
            'ui/panel/font_resolve.c',
            'ui/panel/device.c',
            'ui/panel/glyph_pack.c',
            'ui/panel/theme.c',
        )
    else
        host_sources += files(
            'ui/panel/stub.c',
        )
    endif
endif

if enable_systray
    host_sources += files(
        'tray/bus.c',
        'tray/sni.c',
    )
endif

if enable_voice
    host_sources += files(
        'audio/pw_capture.c',
    )
endif

# ── Dependencies ──────────────────────────────────────────────────────────
host_deps = [libtypio_dep, vulkan_dep]

if enable_wayland
    host_deps += [wayland_client_dep, xkbcommon_dep]
    if flux_dep.found()
        host_deps += flux_dep
    endif
    fontconfig_dep = dependency('fontconfig', required: true)
    harfbuzz_dep = dependency('harfbuzz', required: true)
    freetype_dep = dependency('freetype2', required: true)
    host_deps += [fontconfig_dep, harfbuzz_dep, freetype_dep]
endif

if have_libsystemd
    host_deps += dbus_dep
endif

if enable_voice
    host_deps += pipewire_dep
endif

# ── Include directories ───────────────────────────────────────────────────
# The typio/ headers come from libtypio_dep; top_inc is for the generated
# typio_build_config.h at the project root.
host_inc = [
    top_inc,
    include_directories('.'),
    include_directories('notify'),
    include_directories('state'),
]

if enable_wayland
    host_inc += [
        include_directories('wayland'),
        include_directories('wayland/keyboard/policy'),
        include_directories('wayland/keyboard'),
        include_directories('ui'),
        include_directories('ui/panel'),
        include_directories('engine'),
    ]
endif

if enable_systray
    host_inc += include_directories('tray')
endif

if enable_voice
    host_inc += include_directories('audio')
endif

# ── Executable ────────────────────────────────────────────────────────────
typio_exe = executable('typio',
    host_sources, protocols,
    dependencies: host_deps,
    include_directories: host_inc,
    install: true,
    install_dir: bindir,
)
