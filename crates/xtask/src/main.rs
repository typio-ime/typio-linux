//! Cargo xtask helpers for typio-linux.
//!
//! Run with `cargo xtask <command>`.
//!
//! Commands:
//!   install          Install the daemon binary, systemd user service,
//!                    icons, and example configs.
//!   install --dry-run
//!   uninstall        Remove installed files.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "xtask", about = "Typio build helper tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install typio system-wide or to a custom prefix.
    Install {
        /// Install prefix (default: /usr/local).
        #[arg(long, default_value = "/usr/local")]
        prefix: PathBuf,
        /// Stage files under DESTDIR while preserving the runtime prefix.
        #[arg(long)]
        destdir: Option<PathBuf>,
        /// Do not write anything; just print what would happen.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove installed typio files from a prefix.
    Uninstall {
        /// Install prefix (default: /usr/local).
        #[arg(long, default_value = "/usr/local")]
        prefix: PathBuf,
        /// Do not remove anything; just print what would happen.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Install {
            prefix,
            destdir,
            dry_run,
        } => install(&prefix, destdir.as_deref(), dry_run),
        Command::Uninstall { prefix, dry_run } => uninstall(&prefix, dry_run),
    }
}

struct InstallPlan {
    copies: Vec<(PathBuf, PathBuf)>, // (source, dest)
    dirs: Vec<PathBuf>,
}

fn plan_install(prefix: &Path) -> Result<InstallPlan> {
    let project_root = project_root();
    let bindir = prefix.join("bin");
    let libdir = prefix.join("lib");
    let datadir = prefix.join("share");
    let systemd_user_dir = libdir.join("systemd/user");
    let icons_dst = datadir.join("icons");
    let data_dst = datadir.join("typio");

    let mut copies = Vec::new();
    let mut dirs = vec![
        bindir.clone(),
        systemd_user_dir.clone(),
        icons_dst.clone(),
        data_dst.clone(),
    ];

    // Binary.
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "release".to_string());
    let binary_src = project_root.join("target").join(profile).join("typio");
    copies.push((binary_src, bindir.join("typio")));

    // Systemd service file is rendered separately in install().
    dirs.push(systemd_user_dir.clone());

    // Icons.
    let icons_src = project_root.join("data/icons/hicolor");
    if icons_src.is_dir() {
        for entry in WalkDir::new(&icons_src).min_depth(1) {
            let entry = entry?;
            let src = entry.path();
            if src.is_file() {
                let rel = src.strip_prefix(&icons_src)?;
                let dst = icons_dst.join(rel);
                copies.push((src.to_path_buf(), dst));
            }
        }
    }

    // Example configs.
    for name in ["core.toml.example", "platform.toml.example"] {
        let src = project_root.join("data").join(name);
        if src.is_file() {
            copies.push((src, data_dst.join(name)));
        }
    }

    // We need to write the rendered service file separately.
    Ok(InstallPlan { copies, dirs })
}

fn install(prefix: &Path, destdir: Option<&Path>, dry_run: bool) -> Result<()> {
    let plan = plan_install(prefix)?;

    // Render service file content separately because it is not a direct copy.
    let project_root = project_root();
    let service_template = project_root.join("data/typio.service.in");
    let service_text = fs::read_to_string(&service_template)?;
    let host_dir = prefix.join("bin").to_string_lossy().into_owned();
    let service_rendered = service_text.replace("@TYPIO_HOST_DIR@", &host_dir);
    let service_dst = prefix.join("lib/systemd/user/typio.service");

    // Create directories.
    for dir in &plan.dirs {
        let staged = staged_path(destdir, dir);
        if dry_run {
            println!("mkdir -p {}", staged.display());
        } else {
            fs::create_dir_all(&staged)
                .with_context(|| format!("creating dir {}", staged.display()))?;
        }
    }

    // Copy files.
    for (src, dst) in &plan.copies {
        if src.file_name() == Some(std::ffi::OsStr::new("typio.service.in")) {
            // Handled separately below.
            continue;
        }
        let staged = staged_path(destdir, dst);
        if dry_run {
            println!("cp {} {}", src.display(), staged.display());
        } else {
            if let Some(parent) = staged.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating dir {}", parent.display()))?;
            }
            fs::copy(src, &staged)
                .with_context(|| format!("copying {} to {}", src.display(), staged.display()))?;
        }
    }

    let staged_service_dst = staged_path(destdir, &service_dst);
    if dry_run {
        println!(
            "write {} -> {}",
            service_template.display(),
            staged_service_dst.display()
        );
    } else {
        fs::create_dir_all(staged_service_dst.parent().unwrap())?;
        fs::write(&staged_service_dst, service_rendered)
            .with_context(|| format!("writing {}", staged_service_dst.display()))?;
    }

    if dry_run {
        println!("Dry run complete. No files were written.");
    } else {
        match destdir {
            Some(root) => println!(
                "Staged typio under {} with runtime prefix {}",
                root.display(),
                prefix.display()
            ),
            None => println!("Installed typio to {}", prefix.display()),
        }
    }
    Ok(())
}

fn staged_path(destdir: Option<&Path>, path: &Path) -> PathBuf {
    match destdir {
        Some(root) => {
            let rel = path.strip_prefix("/").unwrap_or(path);
            root.join(rel)
        }
        None => path.to_path_buf(),
    }
}

fn uninstall(prefix: &Path, dry_run: bool) -> Result<()> {
    let plan = plan_install(prefix)?;
    let mut removed: Vec<PathBuf> = Vec::new();

    // Remove copied files.
    for (_, dst) in &plan.copies {
        if dst.file_name() == Some(std::ffi::OsStr::new("typio.service")) {
            // Service file is rendered, remove the actual destination.
            let service_dst = prefix.join("lib/systemd/user/typio.service");
            if service_dst.exists() {
                if dry_run {
                    println!("rm {}", service_dst.display());
                } else {
                    fs::remove_file(&service_dst)?;
                }
                removed.push(service_dst);
            }
            continue;
        }
        if dst.exists() {
            if dry_run {
                println!("rm {}", dst.display());
            } else {
                fs::remove_file(dst)?;
            }
            removed.push(dst.clone());
        }
    }

    // Remove empty icon/theme directories under prefix/share/icons/hicolor.
    let icons_dst = prefix.join("share/icons/hicolor");
    if icons_dst.exists() {
        if dry_run {
            println!("rm -rf {}", icons_dst.display());
        } else {
            fs::remove_dir_all(&icons_dst)?;
        }
        removed.push(icons_dst);
    }

    // Remove data dir.
    let data_dst = prefix.join("share/typio");
    if data_dst.exists() {
        if dry_run {
            println!("rm -rf {}", data_dst.display());
        } else {
            fs::remove_dir_all(&data_dst)?;
        }
        removed.push(data_dst);
    }

    if dry_run {
        println!("Dry run complete. No files were removed.");
    } else {
        println!("Uninstalled typio from {}", prefix.display());
    }
    Ok(())
}

fn project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}
