# 'c2x' is the pre-ratification compiler spelling of C23; meson < 1.4
# (e.g. Ubuntu 24.04 LTS) does not know the 'c23' value. Switch to 'c23'
# when the supported meson floor reaches 1.4.
project('typio-linux', 'c',
    version: '0.3.4',
  default_options: ['warning_level=2', 'c_std=c2x'],
  meson_version: '>=1.0.0',
)

cc = meson.get_compiler('c')

add_project_arguments('-D_POSIX_C_SOURCE=200809L', language: 'c')

warn_flags = [
    '-Wformat=2', '-Wconversion', '-Wsign-conversion',
    '-Wshadow', '-Wundef', '-Wunused', '-Wno-unused-parameter',
]
foreach f : warn_flags
    if cc.has_argument(f)
        add_project_arguments(f, language: 'c')
    endif
endforeach

if get_option('enable_asan')
    add_project_arguments('-fsanitize=address', '-fno-omit-frame-pointer', language: 'c')
    add_project_link_arguments('-fsanitize=address', language: 'c')
endif
if get_option('enable_ubsan')
    add_project_arguments('-fsanitize=undefined', '-fno-omit-frame-pointer', language: 'c')
    add_project_link_arguments('-fsanitize=undefined', language: 'c')
endif

# ── Options ───────────────────────────────────────────────────────────────
build_tests = get_option('build_tests')
enable_wayland = get_option('enable_wayland')
enable_systray = get_option('enable_systray')
enable_voice = get_option('enable_voice')

# ── Dependencies ──────────────────────────────────────────────────────────
thread_dep = dependency('threads')
m_dep = cc.find_library('m', required: false)

# The framework library (libtypio + full C ABI headers).
#
# Resolution order:
#   1. pkg-config — the canonical path.  Set PKG_CONFIG_PATH to
#      <libtypio-checkout>/target/release for a local cargo build, or rely
#      on a system install.  See docs/dev/setup.md.
#   2. meson subproject — fall back to `subprojects/libtypio.wrap`, which
#      clones libtypio and builds it via cargo.
# Version floor: 0.4.0 is the first release with the language-first
# registry API (typio_registry_set_engine_languages and the
# language activation/cycling calls, ADR-0031).
libtypio_dep = dependency('libtypio',
    version: '>= 0.4.0',
    fallback: ['libtypio', 'libtypio_dep'],
    required: true,
)

if enable_wayland
    wayland_client_dep = dependency('wayland-client', required: true)
    xkbcommon_dep = dependency('xkbcommon', required: true)
    vulkan_dep = dependency('vulkan', required: true)
    wayland_scanner = find_program('wayland-scanner', required: true)
endif

dbus_dep = dependency('libsystemd', required: enable_systray)
have_libsystemd = dbus_dep.found()

if enable_voice
    pipewire_dep = dependency('libpipewire-0.3', required: true)
endif

# ── flux (GPU canvas for the candidate Panel) ──────────────────────────────
# Prefer a sibling checkout (~/projects/flux), else the subprojects wrap.
# Both the sibling probe and the symlink target are relative to this file's
# location (<typio-meta-repo>/typio-linux/), so two levels up escape the
# meta-repo and reach the parent that holds the sibling flux checkout.
# The symlink target is relative to the symlink itself
# (<root>/subprojects/flux), which adds one more `..` to escape subprojects/.
flux_sibling = meson.current_source_dir() / '..' / '..' / 'flux' / 'meson.build'
flux_subproject_link = meson.current_source_dir() / 'subprojects' / 'flux'
if run_command('test', '-f', flux_sibling, check: false).returncode() == 0
    if run_command('test', '-L', flux_subproject_link, check: false).returncode() != 0
        run_command('ln', '-sf', '../../../flux', flux_subproject_link, check: false)
    endif
endif

flux_dep = dependency('', required: false)
flux_proj = subproject('flux',
    default_options: [
        'canvas=true', 'scene=false', 'compute=true',
        'examples=false', 'tests=false', 'default_library=static',
    ],
    required: false,
)
if flux_proj.found()
    flux_dep = flux_proj.get_variable('flux_dep')
else
    # Version floor: 0.1.0 is the first standalone flux release (canvas +
    # text + effect + compute modules, stable surface, MIT). Earlier 0.0.x
    # was the in-tree flux of the old monorepo and does not carry the
    # headers typio-linux includes (<flux/vulkan.h>, <flux/canvas.h>).
    flux_dep = dependency('flux', version: '>= 0.1.0', required: enable_wayland)
endif

# ── Version & source label ────────────────────────────────────────────────
typio_version = meson.project_version()
git = find_program('git', required: false)
typio_source_label = 'source'
if git.found()
    r = run_command(git, 'describe', '--always', '--dirty', '--tags', check: false)
    if r.returncode() == 0
        typio_source_label = r.stdout().strip()
    endif
endif

# ── Install paths ──────────────────────────────────────────────────────────
prefix = get_option('prefix')
bindir = get_option('bindir')
libdir = get_option('libdir')
datadir = get_option('datadir')
top_inc = include_directories('.')

# ── Build config header ────────────────────────────────────────────────────
conf_data = configuration_data()
conf_data.set('TYPIO_VERSION', typio_version)
conf_data.set('TYPIO_VERSION_MAJOR', typio_version.split('.')[0])
conf_data.set('TYPIO_VERSION_MINOR', typio_version.split('.')[1])
conf_data.set('TYPIO_VERSION_PATCH', typio_version.split('.')[2])
conf_data.set('TYPIO_BUILD_SOURCE_LABEL', typio_source_label)
conf_data.set('TYPIO_INSTALL_PREFIX', prefix)
conf_data.set('TYPIO_ENGINE_DIR', prefix / datadir / 'typio' / 'engines')
conf_data.set('TYPIO_DATA_DIR', prefix / datadir / 'typio')
conf_data.set('TYPIO_INSTALL_ICON_DIR', prefix / datadir / 'icons')
conf_data.set('TYPIO_SOURCE_ICON_DIR', meson.project_source_root() / 'data' / 'icons')
conf_data.set('HAVE_WAYLAND', enable_wayland)
conf_data.set('HAVE_SYSTRAY', enable_systray)
conf_data.set('HAVE_LIBSYSTEMD', have_libsystemd)
conf_data.set('HAVE_VOICE', enable_voice)
conf_data.set('HAVE_FLUX', flux_dep.found())

configure_file(
    input: 'typio_build_config.h.in',
    output: 'typio_build_config.h',
    configuration: conf_data,
)

# ── Subdirectories ─────────────────────────────────────────────────────────
subdir('src')
if build_tests
    subdir('tests')
endif

# ── Data files ─────────────────────────────────────────────────────────────
service_conf = configuration_data()
service_conf.set('TYPIO_HOST_DIR', prefix / bindir)

configure_file(
    input: 'data/typio.service.in',
    output: 'typio.service',
    configuration: service_conf,
    install: true,
    install_dir: prefix / libdir / 'systemd' / 'user',
)

install_subdir('data/icons/hicolor', install_dir: datadir / 'icons')

if import('fs').is_file('data/core.toml.example')
    install_data('data/core.toml.example', install_dir: datadir / 'typio')
endif
if import('fs').is_file('data/platform.toml.example')
    install_data('data/platform.toml.example', install_dir: datadir / 'typio')
endif
