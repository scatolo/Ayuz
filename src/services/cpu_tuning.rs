// Ayuz - Unofficial Control Center for Asus Laptops
// Copyright (C) 2026 Guido Philipp
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see https://www.gnu.org/licenses/.

//! Intel P-State `max_perf_pct` tuning service.
//!
//! Intentionally scopes active CPU tuning to `/sys/devices/system/cpu/intel_pstate/max_perf_pct`
//! (leaving `EPP`, `no_turbo`, and `min_perf_pct` managed by `tuned-ppd`), using write access
//! granted via `systemd-tmpfiles` for the `input` group so AC ↔ Battery profile switches
//! never trigger `pkexec` password prompts.

use std::io::Write;
use std::path::Path;

use rust_i18n::t;

use crate::sys_paths::{
    SYS_INTEL_MAX_PERF_PCT, clamp_perf_pct, is_path_writable, parse_perf_pct,
};

/// Recommended `max_perf_pct` presets in the `60%–100%` range so P-cores stay above base clock.
pub const RECOMMENDED_MAX_PERF_PRESETS: [u8; 6] = [60, 65, 75, 80, 90, 100];

/// Result of attempting to apply `intel_pstate/max_perf_pct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMaxPerfOutcome {
    /// Target percentage applied (or already active in sysfs).
    Applied(u8),
    /// No ceiling configured (`None`) and no reset required.
    SkippedUnconfigured,
    /// `intel_pstate/max_perf_pct` does not exist on this CPU (e.g. AMD).
    Unsupported,
    /// `intel_pstate/max_perf_pct` exists but is not writable by the current user.
    PermissionDenied,
}

/// Builds sorted, deduplicated `Option<u8>` choices for the CPU Max Performance dropdown:
/// `[None, Some(60), Some(65), Some(75), Some(80), Some(90), Some(100)]`,
/// inserting a custom clamped value from `saved_pct` if not already present.
pub fn build_max_perf_presets(saved_pct: Option<u8>) -> Vec<Option<u8>> {
    let mut values = RECOMMENDED_MAX_PERF_PRESETS.to_vec();
    if let Some(raw) = saved_pct {
        let clamped = clamp_perf_pct(raw);
        if !values.contains(&clamped) {
            values.push(clamped);
        }
    }
    values.sort_unstable();
    values.dedup();

    let mut out = Vec::with_capacity(1 + values.len());
    out.push(None);
    out.extend(values.into_iter().map(Some));
    out
}

/// Formats a localized dropdown label for an `Option<u8>` CPU Max Performance preset.
pub fn format_max_perf_preset_label(preset: Option<u8>) -> String {
    match preset {
        None => t!("fan_cpu_max_perf_auto").to_string(),
        Some(65) => t!("fan_cpu_max_perf_preset_battery", pct = "65").to_string(),
        Some(80) => t!("fan_cpu_max_perf_preset_balanced", pct = "80").to_string(),
        Some(100) => t!("fan_cpu_max_perf_preset_full", pct = "100").to_string(),
        Some(pct) => format!("{pct}%"),
    }
}

/// Resolves the concrete `max_perf_pct` value to write to sysfs, if any.
///
/// - `Some(pct)` always clamps to `30..=100` via [`clamp_perf_pct`].
/// - `None` (System Default) resolves to `Some(100)` when switching away from a previously
///   capped profile (`prev_configured.is_some()`) or when `current_sysfs` is below `100`,
///   and returns `None` (no-op) otherwise.
pub fn resolve_target_max_perf_pct(
    configured: Option<u8>,
    prev_configured: Option<u8>,
    current_sysfs: Option<u8>,
) -> Option<u8> {
    match configured {
        Some(pct) => Some(clamp_perf_pct(pct)),
        None => {
            if prev_configured.is_some() || current_sysfs.is_some_and(|v| v < 100) {
                Some(100)
            } else {
                None
            }
        }
    }
}

/// Applies `configured` to the live `/sys/devices/system/cpu/intel_pstate/max_perf_pct` path.
pub async fn apply_max_perf_pct(
    configured: Option<u8>,
    prev_configured: Option<u8>,
) -> Result<ApplyMaxPerfOutcome, String> {
    apply_max_perf_pct_at(
        Path::new(SYS_INTEL_MAX_PERF_PCT),
        configured,
        prev_configured,
    )
    .await
}

/// Path-parameterized implementation of [`apply_max_perf_pct`] for headless unit testing.
pub async fn apply_max_perf_pct_at(
    path: &Path,
    configured: Option<u8>,
    prev_configured: Option<u8>,
) -> Result<ApplyMaxPerfOutcome, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        apply_max_perf_pct_sync(&path, configured, prev_configured)
    })
    .await
    .map_err(|e| t!("error_spawn_blocking", error = e.to_string()).to_string())?
}

fn write_perf_pct(writer: &mut impl Write, target: u8) -> std::io::Result<()> {
    // Sysfs attributes invoke the kernel store() callback once per write() syscall.
    // Using write_all with a pre-formatted buffer avoids writeln! splitting "{target}"
    // and "\n" into two syscalls (the trailing "\n" write would fail with -EINVAL).
    let payload = format!("{target}\n");
    writer.write_all(payload.as_bytes())
}

fn apply_max_perf_pct_sync(
    path: &Path,
    configured: Option<u8>,
    prev_configured: Option<u8>,
) -> Result<ApplyMaxPerfOutcome, String> {
    if !path.exists() {
        return Ok(ApplyMaxPerfOutcome::Unsupported);
    }

    let writable = is_path_writable(path);
    if configured.is_none() && !writable {
        // Keep unconfigured profiles a silent no-op when sysfs write permission is not set up.
        return Ok(ApplyMaxPerfOutcome::SkippedUnconfigured);
    }

    let current_sysfs = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| parse_perf_pct(&raw));

    let Some(target) = resolve_target_max_perf_pct(configured, prev_configured, current_sysfs)
    else {
        return Ok(ApplyMaxPerfOutcome::SkippedUnconfigured);
    };

    if !writable {
        return Ok(ApplyMaxPerfOutcome::PermissionDenied);
    }

    if current_sysfs == Some(target) {
        return Ok(ApplyMaxPerfOutcome::Applied(target));
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| t!("error_cpu_max_perf_write", error = e.to_string()).to_string())?;
    write_perf_pct(&mut file, target)
        .map_err(|e| t!("error_cpu_max_perf_write", error = e.to_string()).to_string())?;

    Ok(ApplyMaxPerfOutcome::Applied(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempFile {
        dir: PathBuf,
        file: PathBuf,
    }

    impl TempFile {
        fn new(prefix: &str, initial: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "ayuz_cpu_tune_{}_{}_{}",
                prefix,
                std::process::id(),
                id
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("max_perf_pct");
            std::fs::write(&file, initial).unwrap();
            Self { dir, file }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn test_build_max_perf_presets_recommended_and_custom_clamped() {
        assert_eq!(
            build_max_perf_presets(None),
            vec![
                None,
                Some(60),
                Some(65),
                Some(75),
                Some(80),
                Some(90),
                Some(100)
            ]
        );
        assert_eq!(
            build_max_perf_presets(Some(80)),
            vec![
                None,
                Some(60),
                Some(65),
                Some(75),
                Some(80),
                Some(90),
                Some(100)
            ]
        );
        assert_eq!(
            build_max_perf_presets(Some(70)),
            vec![
                None,
                Some(60),
                Some(65),
                Some(70),
                Some(75),
                Some(80),
                Some(90),
                Some(100)
            ]
        );
        // Unsafe low value below 30 is clamped to 30
        assert_eq!(
            build_max_perf_presets(Some(8)),
            vec![
                None,
                Some(30),
                Some(60),
                Some(65),
                Some(75),
                Some(80),
                Some(90),
                Some(100)
            ]
        );
    }

    #[test]
    fn test_resolve_target_max_perf_pct() {
        assert_eq!(resolve_target_max_perf_pct(None, None, Some(100)), None);
        assert_eq!(resolve_target_max_perf_pct(None, None, None), None);
        assert_eq!(
            resolve_target_max_perf_pct(None, Some(65), Some(65)),
            Some(100)
        );
        assert_eq!(
            resolve_target_max_perf_pct(None, None, Some(80)),
            Some(100)
        );
        assert_eq!(
            resolve_target_max_perf_pct(Some(65), None, Some(100)),
            Some(65)
        );
        assert_eq!(
            resolve_target_max_perf_pct(Some(8), None, Some(100)),
            Some(30)
        );
        assert_eq!(
            resolve_target_max_perf_pct(Some(120), None, Some(80)),
            Some(100)
        );
    }

    #[tokio::test]
    async fn test_apply_max_perf_pct_at_missing_is_unsupported() {
        let missing = std::env::temp_dir().join("ayuz_nonexistent_max_perf_pct_12345");
        let outcome = apply_max_perf_pct_at(&missing, Some(65), None)
            .await
            .expect("should not error");
        assert_eq!(outcome, ApplyMaxPerfOutcome::Unsupported);
    }

    #[test]
    fn test_write_perf_pct_uses_single_write_call() {
        struct RecordingWriter {
            writes: Vec<Vec<u8>>,
        }

        impl Write for RecordingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.writes.push(buf.to_vec());
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut writer = RecordingWriter { writes: Vec::new() };
        write_perf_pct(&mut writer, 65).expect("single write should succeed");
        assert_eq!(writer.writes, vec![b"65\n".to_vec()]);
    }

    #[tokio::test]
    async fn test_apply_max_perf_pct_at_applies_clamps_and_restores() {
        let mock = TempFile::new("apply", "100\n");

        // Unconfigured on 100% -> SkippedUnconfigured
        let res = apply_max_perf_pct_at(&mock.file, None, None)
            .await
            .unwrap();
        assert_eq!(res, ApplyMaxPerfOutcome::SkippedUnconfigured);
        assert_eq!(
            parse_perf_pct(&std::fs::read_to_string(&mock.file).unwrap()),
            Some(100)
        );

        // Apply 65%
        let res = apply_max_perf_pct_at(&mock.file, Some(65), None)
            .await
            .unwrap();
        assert_eq!(res, ApplyMaxPerfOutcome::Applied(65));
        assert_eq!(
            parse_perf_pct(&std::fs::read_to_string(&mock.file).unwrap()),
            Some(65)
        );

        // Clamp unsafe 10% -> 30%
        let res = apply_max_perf_pct_at(&mock.file, Some(10), Some(65))
            .await
            .unwrap();
        assert_eq!(res, ApplyMaxPerfOutcome::Applied(30));
        assert_eq!(
            parse_perf_pct(&std::fs::read_to_string(&mock.file).unwrap()),
            Some(30)
        );

        // Switch back to None (System Default) -> restores 100%
        let res = apply_max_perf_pct_at(&mock.file, None, Some(30))
            .await
            .unwrap();
        assert_eq!(res, ApplyMaxPerfOutcome::Applied(100));
        assert_eq!(
            parse_perf_pct(&std::fs::read_to_string(&mock.file).unwrap()),
            Some(100)
        );
    }
}
