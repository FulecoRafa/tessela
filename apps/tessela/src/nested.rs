//! `--nested`: render into a headless sway instead of the desktop.
//!
//! The project runs on a compositor of its own — no physical screen, nothing
//! drawn over the desktop's bar — while modules still read the real session
//! (Hyprland, the tray, the portal). `tessela devtools shot` captures from it.
//! Useful when the screen is off or locked, and for previews in general.

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;

pub struct Nested {
    child: Child,
    /// `WAYLAND_DISPLAY` of the nested compositor.
    pub display: String,
}

/// Wayland sockets created at or after `since`. By time, not by name: a
/// compositor killed earlier leaves its socket behind, and the next one
/// reuses the name.
fn sockets_since(runtime: &Path, since: std::time::SystemTime) -> Vec<String> {
    std::fs::read_dir(runtime)
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t >= since)
                })
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
                .collect()
        })
        .unwrap_or_default()
}

/// `spec`: `WIDTHxHEIGHT@SCALE`, e.g. `1920x1080@1.25`.
pub fn start(dir: &Path, spec: &str) -> anyhow::Result<Nested> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "--nested needs Linux: it runs a headless sway"
    );
    let (mode, scale) = spec.split_once('@').unwrap_or((spec, "1"));
    anyhow::ensure!(
        mode.split_once('x')
            .is_some_and(|(w, h)| w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok())
            && scale.parse::<f32>().is_ok(),
        "--nested takes WIDTHxHEIGHT@SCALE, e.g. 1920x1080@1.25"
    );
    let config = dir.join("nested-sway.conf");
    std::fs::write(
        &config,
        format!(
            "output HEADLESS-1 mode {mode} scale {scale} bg #3b3355 solid_color\ndefault_border none\n"
        ),
    )?;
    let runtime = std::path::PathBuf::from(
        std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
    );
    // A little slack: file times may be coarser than the clock.
    let since = std::time::SystemTime::now() - Duration::from_millis(50);
    let mut cmd = Command::new("sway");
    cmd.arg("-c")
        .arg(&config)
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("nested-sway.log"))?);
    if Path::new("/dev/dri/renderD128").exists() {
        cmd.env("WLR_RENDER_DRM_DEVICE", "/dev/dri/renderD128");
    }
    // SAFETY: prctl is async-signal-safe; nothing else runs before exec.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .context("starting sway (is it installed? it is in the dev shell)")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(display) = sockets_since(&runtime, since).into_iter().next() {
            log::info!(
                "nested: sway on {display} ({mode} @ {scale}); log in {}",
                dir.join("nested-sway.log").display()
            );
            return Ok(Nested { child, display });
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "sway did not create a Wayland socket; see nested-sway.log"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Logical size of a `WIDTHxHEIGHT@SCALE` spec.
pub fn logical_size(spec: &str) -> Option<[f64; 2]> {
    let (mode, scale) = spec.split_once('@').unwrap_or((spec, "1"));
    let (w, h) = mode.split_once('x')?;
    let s: f64 = scale.parse().ok()?;
    Some([
        (w.parse::<f64>().ok()? / s).round(),
        (h.parse::<f64>().ok()? / s).round(),
    ])
}
