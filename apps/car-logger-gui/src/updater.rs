//! Non-blocking, fail-open updater for public GitHub Releases.
//!
//! Release discovery and safe executable replacement are delegated to `self_update`.
//! A release-owned SHA-256 sidecar is additionally required before an archive is staged.

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

const OWNER: &str = "Ryokugyoku";
const REPOSITORY: &str = "CarLogger";
const BINARY: &str = if cfg!(windows) {
    "car-logger-gui.exe"
} else {
    "car-logger-gui"
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Downloading(u8),
    Verifying,
    WaitingForSafeExit,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct UpdateEvent {
    pub phase: UpdatePhase,
    pub target_version: Option<String>,
    pub notes: Option<String>,
    pub manual: bool,
}

#[derive(Clone, Debug)]
struct Candidate {
    version: String,
    notes: Option<String>,
    archive_url: String,
    checksum_url: String,
    archive_name: String,
}

#[derive(Debug)]
pub struct StagedUpdate {
    pub version: String,
    directory: PathBuf,
    executable: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestorableUiState {
    pub page: String,
    pub width: i32,
    pub height: i32,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn spawn_check(manual: bool, sender: mpsc::Sender<UpdateEvent>) {
    if check_in_progress().swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        send(&sender, UpdatePhase::Checking, None, None, manual);
        match discover() {
            Ok(Some(candidate)) => match stage(&candidate, &sender, manual) {
                Ok(staged) => {
                    let _ = staged_slot().lock().map(|mut value| *value = Some(staged));
                    send(
                        &sender,
                        UpdatePhase::WaitingForSafeExit,
                        Some(candidate.version),
                        candidate.notes,
                        manual,
                    );
                }
                Err(error) => send(
                    &sender,
                    UpdatePhase::Failed(error),
                    Some(candidate.version),
                    candidate.notes,
                    manual,
                ),
            },
            Ok(None) => send(&sender, UpdatePhase::Idle, None, None, manual),
            Err(error) => send(&sender, UpdatePhase::Failed(error), None, None, manual),
        }
        check_in_progress().store(false, Ordering::Release);
    });
}

pub fn take_staged() -> Option<StagedUpdate> {
    staged_slot().lock().ok()?.take()
}

pub fn apply_and_restart(staged: StagedUpdate) -> Result<(), String> {
    self_update::self_replace::self_replace(&staged.executable)
        .map_err(|error| format!("更新の適用に失敗しました: {error}"))?;
    Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .env("CAR_LOGGER_UPDATED_FROM", current_version())
        .env("CAR_LOGGER_UPDATED_TO", &staged.version)
        .spawn()
        .map_err(|error| format!("更新後の再起動に失敗しました: {error}"))?;
    // Keep the extracted directory alive until self_replace has closed its handles.
    let _ = fs::remove_dir_all(staged.directory);
    Ok(())
}

pub fn save_ui_state(path: &Path, state: &RestorableUiState) -> Result<(), String> {
    let parent = path.parent().ok_or("状態保存先が不正です")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())
}

pub fn load_ui_state(path: &Path) -> Option<RestorableUiState> {
    let bytes = fs::read(path).ok()?;
    let state = serde_json::from_slice(&bytes).ok();
    let _ = fs::remove_file(path);
    state
}

fn staged_slot() -> &'static Mutex<Option<StagedUpdate>> {
    static SLOT: std::sync::OnceLock<Mutex<Option<StagedUpdate>>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn check_in_progress() -> &'static AtomicBool {
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    &ACTIVE
}

fn discover() -> Result<Option<Candidate>, String> {
    if staged_slot().lock().is_ok_and(|value| value.is_some()) {
        return Ok(None);
    }
    let current = Version::parse(current_version()).map_err(|e| e.to_string())?;
    let release = if is_preview(&current) {
        self_update::backends::github::ReleaseList::configure()
            .repo_owner(OWNER)
            .repo_name(REPOSITORY)
            .build()
            .map_err(|e| friendly_error(&e.to_string()))?
            .fetch()
            .map_err(|e| friendly_error(&e.to_string()))?
            .into_iter()
            .filter_map(|release| {
                let version = Version::parse(release.version.trim_start_matches('v')).ok()?;
                channel_accepts(&current, &version).then_some((version, release))
            })
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, release)| release)
    } else {
        let updater = self_update::backends::github::Update::configure()
            .repo_owner(OWNER)
            .repo_name(REPOSITORY)
            .bin_name(BINARY)
            .current_version(current_version())
            .no_confirm(true)
            .show_output(false)
            .build()
            .map_err(|e| friendly_error(&e.to_string()))?;
        Some(
            updater
                .get_latest_release()
                .map_err(|e| friendly_error(&e.to_string()))?,
        )
    };
    let Some(release) = release else {
        return Ok(None);
    };
    let latest = Version::parse(release.version.trim_start_matches('v'))
        .map_err(|_| "リリースのバージョン情報が不正です".to_string())?;
    if !channel_accepts(&current, &latest) {
        return Ok(None);
    }
    let marker = platform_marker();
    let archive = release
        .assets
        .iter()
        .find(|asset| asset.name.contains(marker) && asset.name.ends_with(".zip"))
        .ok_or_else(|| format!("このOS/CPU向けの更新がありません ({marker})"))?;
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == format!("{}.sha256", archive.name))
        .ok_or_else(|| "更新のSHA-256ファイルがありません".to_string())?;
    if !archive.download_url.starts_with("https://github.com/")
        || !checksum.download_url.starts_with("https://github.com/")
    {
        return Err("信頼されていない更新URLを拒否しました".into());
    }
    Ok(Some(Candidate {
        version: latest.to_string(),
        notes: release.body,
        archive_url: archive.download_url.clone(),
        checksum_url: checksum.download_url.clone(),
        archive_name: archive.name.clone(),
    }))
}

fn is_preview(version: &Version) -> bool {
    version.pre.as_str().split('.').next() == Some("preview")
}

fn channel_accepts(current: &Version, candidate: &Version) -> bool {
    candidate > current
        && if is_preview(current) {
            is_preview(candidate)
        } else {
            candidate.pre.is_empty()
        }
}

fn stage(
    candidate: &Candidate,
    sender: &mpsc::Sender<UpdateEvent>,
    manual: bool,
) -> Result<StagedUpdate, String> {
    let directory = std::env::temp_dir().join(format!("apex-trace-update-{}", candidate.version));
    if directory.exists() {
        fs::remove_dir_all(&directory).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&directory).map_err(|e| format!("更新領域を作成できません: {e}"))?;
    let archive = directory.join(&candidate.archive_name);
    let mut output = ProgressWriter::new(
        File::create(&archive).map_err(|e| e.to_string())?,
        sender.clone(),
        candidate,
        manual,
    );
    self_update::Download::from_url(&candidate.archive_url)
        .download_to(&mut output)
        .map_err(|e| friendly_error(&e.to_string()))?;
    send(
        sender,
        UpdatePhase::Verifying,
        Some(candidate.version.clone()),
        candidate.notes.clone(),
        manual,
    );
    let checksum_file = directory.join("expected.sha256");
    self_update::Download::from_url(&candidate.checksum_url)
        .download_to(File::create(&checksum_file).map_err(|e| e.to_string())?)
        .map_err(|e| friendly_error(&e.to_string()))?;
    verify_sha256(
        &archive,
        &fs::read_to_string(checksum_file).map_err(|e| e.to_string())?,
    )?;
    self_update::Extract::from_source(&archive)
        .archive(self_update::ArchiveKind::Zip)
        .extract_file(&directory, Path::new(BINARY))
        .map_err(|e| format!("更新ファイルを展開できません: {e}"))?;
    let executable = directory.join(BINARY);
    Ok(StagedUpdate {
        version: candidate.version.clone(),
        directory,
        executable,
    })
}

struct ProgressWriter {
    file: File,
    written: u64,
    sender: mpsc::Sender<UpdateEvent>,
    version: String,
    notes: Option<String>,
    manual: bool,
}
impl ProgressWriter {
    fn new(file: File, sender: mpsc::Sender<UpdateEvent>, c: &Candidate, manual: bool) -> Self {
        Self {
            file,
            written: 0,
            sender,
            version: c.version.clone(),
            notes: c.notes.clone(),
            manual,
        }
    }
}
impl Write for ProgressWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.file.write(buf)?;
        self.written += n as u64;
        let approximate = ((self.written / (1024 * 1024)).min(99)) as u8;
        send(
            &self.sender,
            UpdatePhase::Downloading(approximate),
            Some(self.version.clone()),
            self.notes.clone(),
            self.manual,
        );
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn verify_sha256(path: &Path, sidecar: &str) -> Result<(), String> {
    let expected = sidecar
        .split_whitespace()
        .next()
        .ok_or("SHA-256ファイルが不正です")?;
    if expected.len() != 64 || !expected.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("SHA-256ファイルが不正です".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("更新ファイルのSHA-256が一致しません。適用を拒否しました".into());
    }
    Ok(())
}

fn platform_marker() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64",
        ("macos", "x86_64") => "macos-x64",
        ("macos", "aarch64") => "macos-arm64",
        ("linux", "x86_64") => "linux-x64",
        _ => "unsupported-platform",
    }
}
fn friendly_error(error: &str) -> String {
    if error.contains("429") || error.to_ascii_lowercase().contains("rate limit") {
        "GitHubの更新確認回数制限に達しました。後で再試行します".into()
    } else {
        format!("更新サービスへ接続できません: {error}")
    }
}
fn send(
    sender: &mpsc::Sender<UpdateEvent>,
    phase: UpdatePhase,
    target_version: Option<String>,
    notes: Option<String>,
    manual: bool,
) {
    let _ = sender.send(UpdateEvent {
        phase,
        target_version,
        notes,
        manual,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_mapping_is_supported_here() {
        assert_ne!(platform_marker(), "unsupported-platform");
    }
    #[test]
    fn semver_does_not_treat_old_as_update() {
        assert!(Version::parse("1.2.3").unwrap() > Version::parse("1.2.2").unwrap());
    }
    #[test]
    fn preview_build_accepts_only_newer_preview_versions() {
        let current = Version::parse("0.1.1-preview.42").unwrap();
        assert!(channel_accepts(
            &current,
            &Version::parse("0.1.1-preview.43").unwrap()
        ));
        assert!(!channel_accepts(
            &current,
            &Version::parse("0.1.1").unwrap()
        ));
        assert!(!channel_accepts(
            &current,
            &Version::parse("0.1.1-beta.99").unwrap()
        ));
    }
    #[test]
    fn stable_build_accepts_only_newer_stable_versions() {
        let current = Version::parse("0.1.1").unwrap();
        assert!(channel_accepts(&current, &Version::parse("0.1.2").unwrap()));
        assert!(!channel_accepts(
            &current,
            &Version::parse("0.1.2-preview.1").unwrap()
        ));
    }
    #[test]
    fn rejects_tampered_archive() {
        let p = std::env::temp_dir().join("apex-trace-hash-test");
        fs::write(&p, b"changed").unwrap();
        assert!(verify_sha256(&p, &format!("{}  x", "0".repeat(64))).is_err());
        let _ = fs::remove_file(p);
    }
    #[test]
    fn ui_state_round_trip_and_corruption_are_safe() {
        let p = std::env::temp_dir().join("apex-trace-state-test.json");
        let value = RestorableUiState {
            page: "settings".into(),
            width: 900,
            height: 700,
        };
        save_ui_state(&p, &value).unwrap();
        assert_eq!(load_ui_state(&p), Some(value));
        fs::write(&p, b"broken").unwrap();
        assert_eq!(load_ui_state(&p), None);
    }
}
