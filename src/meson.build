# ── Typio Wayland host ───────────────────────────────────────────

# ── Wayland protocol generation ───────────────────────────────────────────
protocols = []
if enable_wayland
    protocol_xmls = {
        'input-method-unstable-v2': meson.project_source_root() / 'protocols' / 'input-method-unstable-v2.xml',
        'virtual-keyboard-unstable-v1': meson.project_source_root() / 'protocols' / 'virtual-keyboard-unstable-v1.xml',
        'fractional-scale-v1': meson.project_source_root() / 'protocols' / 'fractional-scale-v1.xml',
        'viewporter': meson.project_source_root() / 'protocols' / 'viewporter.xml',
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
    endforeach
endif

# ── Source files ──────────────────────────────────────────────────────────
host_sources = files(
    'main.c',
    'app.c',
    'plugin_loader.c',
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
        'bridge/boundary.c',
        'bridge/candidate_guard.c',
        'frontend/frontend_aux.c',
        'engine/identity.c',
        'input/arbiter.c',
        'input/debug.c',
        'input/tracker.c',
        'input/router.c',
        'input/repeat.c',
        'engine/lifecycle.c',
        'engine/lifecycle_policy.c',
        'input/modifiers.c',
        'frontend/event_loop.c',
        'frontend/panel_scheduler.c',
        'frontend/panel_coordinator.c',
        'frontend/runtime_config.c',
        'frontend/watchdog.c',
        'input/xkb.c',
        'frontend/aux_handler.c',
        'frontend/aux_adapters.c',
        'ui/state.c',
        'engine/reconcile.c',
        'engine/reconciler.c',
        'engine/backoff.c',
        'input/repeat_guard.c',
        'engine/resume.c',
        'input/chords.c',
        'engine/startup.c',
        'input/bridge.c',
        'frontend/frontend.c',
        'frontend/indicator.c',
        'ui/preedit.c',
        'frontend/input_method.c',
        'frontend/keyboard.c',
        'frontend/shortcut.c',
        'frontend/trace.c',
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
        'voice/pw_capture.c',
    )
endif

# ── Dependencies ──────────────────────────────────────────────────────────
host_deps = [libtypio_dep, dl_dep, vulkan_dep]

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

if have_libdbus
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
        include_directories('frontend'),
        include_directories('input'),
        include_directories('ui'),
        include_directories('ui/panel'),
        include_directories('engine'),
        include_directories('bridge'),
        include_directories('platform'),
    ]
endif

if enable_systray
    host_inc += include_directories('tray')
endif

if enable_voice
    host_inc += include_directories('voice')
endif

# ── Executable ────────────────────────────────────────────────────────────
typio_exe = executable('typio',
    host_sources, protocols,
    dependencies: host_deps,
    include_directories: host_inc,
    install: true,
    install_dir: bindir,
)
