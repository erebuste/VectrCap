//! Self-update from GitHub Releases.
//!
//! A background thread checks `releases/latest` shortly after start and then daily
//! (release builds only; a manual check always works). Installing downloads
//! `VectrCap.exe`, verifies it against the release's `VectrCap.exe.sha256`, stages it
//! beside the running exe as `VectrCap.exe.new` and asks the app to quit. After the app
//! has fully exited (queue drained, single-instance lock released), `finish_restart`
//! swaps the files and starts the new exe. Nothing installs without a click.
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST: &str = "https://api.github.com/repos/erebuste/VectrCap/releases/latest";
const EXE_ASSET: &str = "VectrCap.exe";
const SHA_ASSET: &str = "VectrCap.exe.sha256";
const MAX_EXE: usize = 128 << 20;
const MAX_JSON: usize = 1 << 20;
const FIRST_CHECK: Duration = Duration::from_secs(10);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    exe_url: String,
    sha_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    /// The repository has no public release (yet).
    NoReleases,
    UpToDate,
    Available(Release),
    Downloading(Release),
    Restarting(Release),
    Failed(String),
}

type Hook = Arc<dyn Fn() + Send + Sync>;
enum Job {
    Check,
    Install,
}
struct Updater {
    status: Mutex<Status>,
    jobs: mpsc::Sender<Job>,
    restart: AtomicBool,
    wake: Mutex<Option<Hook>>,
    quit: Mutex<Option<Hook>>,
}
static UPDATER: OnceLock<Updater> = OnceLock::new();

/// Starts the update thread. Call once, in the primary instance only.
pub fn start() {
    let (jobs, rx) = mpsc::channel();
    let updater = Updater {
        status: Mutex::default(),
        jobs,
        restart: AtomicBool::new(false),
        wake: Mutex::default(),
        quit: Mutex::default(),
    };
    if UPDATER.set(updater).is_ok() {
        let _ = std::thread::Builder::new()
            .name("updater".into())
            .spawn(move || worker(rx));
    }
}

/// `wake` repaints the UI on a status change; `quit` exits the app once an update is staged.
pub fn connect(wake: Hook, quit: Hook) {
    if let Some(updater) = UPDATER.get() {
        *lock(&updater.wake) = Some(wake);
        *lock(&updater.quit) = Some(quit);
    }
}

pub fn status() -> Status {
    UPDATER
        .get()
        .map(|u| lock(&u.status).clone())
        .unwrap_or_default()
}

/// The newer version on offer, if any.
pub fn offered_version() -> Option<String> {
    match status() {
        Status::Available(release) => Some(release.version),
        _ => None,
    }
}

pub fn check() {
    send(Job::Check);
}

/// Downloads the offered version, then quits so `finish_restart` can swap it in.
pub fn install() {
    send(Job::Install);
}

/// Called after the app has exited: swaps in a staged update and starts it.
pub fn finish_restart() {
    let Some(updater) = UPDATER.get() else { return };
    if !updater.restart.load(Ordering::Acquire) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // On a failed swap the old exe is back in place; restarting it is still right.
    let _ = swap(&exe);
    let _ = std::process::Command::new(&exe).spawn();
}

fn send(job: Job) {
    if let Some(updater) = UPDATER.get() {
        let _ = updater.jobs.send(job);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_status(updater: &Updater, status: Status) {
    *lock(&updater.status) = status;
    if let Some(wake) = lock(&updater.wake).clone() {
        wake();
    }
}

fn worker(rx: mpsc::Receiver<Job>) {
    let Some(updater) = UPDATER.get() else { return };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let (Ok(runtime), Ok(client)) = (runtime, http()) else {
        set_status(updater, Status::Failed("Updates are unavailable.".into()));
        return;
    };
    let automatic = !cfg!(debug_assertions);
    let mut cleaned = false;
    let mut wait = FIRST_CHECK;
    loop {
        let job = match rx.recv_timeout(wait) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                wait = DAY;
                if !cleaned {
                    // The previous version's exe may still be closing; by now it is gone.
                    cleaned = true;
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = fs::remove_file(sibling(&exe, ".old"));
                        if !matches!(*lock(&updater.status), Status::Restarting(_)) {
                            let _ = fs::remove_file(sibling(&exe, ".new"));
                        }
                    }
                }
                if !automatic {
                    continue;
                }
                Job::Check
            }
        };
        let current = lock(&updater.status).clone();
        match job {
            Job::Check => {
                if matches!(current, Status::Downloading(_) | Status::Restarting(_)) {
                    continue;
                }
                set_status(updater, Status::Checking);
                let status = match runtime.block_on(latest(&client)) {
                    Ok(None) => Status::NoReleases,
                    Ok(Some(release)) if is_newer(&release.version, CURRENT) => {
                        Status::Available(release)
                    }
                    Ok(Some(_)) => Status::UpToDate,
                    Err(message) => Status::Failed(message),
                };
                set_status(updater, status);
            }
            Job::Install => {
                let Status::Available(release) = current else {
                    continue;
                };
                set_status(updater, Status::Downloading(release.clone()));
                match runtime
                    .block_on(download(&client, &release))
                    .and_then(|bytes| stage(&bytes))
                {
                    Ok(()) => {
                        set_status(updater, Status::Restarting(release));
                        updater.restart.store(true, Ordering::Release);
                        if let Some(quit) = lock(&updater.quit).clone() {
                            quit();
                        }
                    }
                    Err(message) => set_status(updater, Status::Failed(message)),
                }
            }
        }
    }
}

fn http() -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .referer(false)
        // Release assets redirect to GitHub's download host; https_only covers every hop.
        .redirect(reqwest::redirect::Policy::limited(5))
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("VectrCap/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Updates are unavailable.".to_string())
}

async fn latest(client: &reqwest::Client) -> Result<Option<Release>, String> {
    let response = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Could not reach GitHub.".to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = read(response, MAX_JSON).await?;
    parse_release(&body).map(Some)
}

async fn download(client: &reqwest::Client, release: &Release) -> Result<Vec<u8>, String> {
    let get = |url: &str| client.get(url).send();
    let sha = get(&release.sha_url)
        .await
        .map_err(|_| "Could not download the update.".to_string())?;
    let expected =
        parse_sha256(&read(sha, 1024).await?).ok_or("The update's checksum is unreadable.")?;
    let exe = get(&release.exe_url)
        .await
        .map_err(|_| "Could not download the update.".to_string())?;
    let bytes = read(exe, MAX_EXE).await?;
    verify(&bytes, &expected)?;
    Ok(bytes)
}

async fn read(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}.", response.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The download was interrupted.".to_string())?
    {
        if body.len() + chunk.len() > max {
            return Err("The download is larger than expected.".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_release(json: &[u8]) -> Result<Release, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        tag_name: String,
        assets: Vec<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    let raw: Raw =
        serde_json::from_slice(json).map_err(|_| "GitHub's answer is unreadable.".to_string())?;
    let version = raw.tag_name.trim_start_matches('v').to_string();
    if parse_version(&version).is_none() {
        return Err(format!(
            "The latest release has an unknown version ({}).",
            raw.tag_name
        ));
    }
    let url = |name: &str| {
        raw.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
            .ok_or_else(|| format!("The latest release has no {name}."))
    };
    Ok(Release {
        exe_url: url(EXE_ASSET)?,
        sha_url: url(SHA_ASSET)?,
        version,
    })
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    let parsed = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(parsed)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// `sha256sum` / `Get-FileHash` style: the first token is the hex digest.
fn parse_sha256(text: &[u8]) -> Option<[u8; 32]> {
    let hex = std::str::from_utf8(text).ok()?.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(digest)
}

fn verify(bytes: &[u8], expected: &[u8; 32]) -> Result<(), String> {
    if Sha256::digest(bytes).as_slice() != expected {
        return Err("The download did not match its checksum.".into());
    }
    if !bytes.starts_with(b"MZ") {
        return Err("The download is not a Windows program.".into());
    }
    Ok(())
}

fn stage(bytes: &[u8]) -> Result<(), String> {
    let failed = |_| "Could not save the update next to VectrCap.exe.".to_string();
    let exe = std::env::current_exe().map_err(failed)?;
    let part = sibling(&exe, ".new.part");
    fs::write(&part, bytes).map_err(failed)?;
    fs::rename(&part, sibling(&exe, ".new")).map_err(failed)
}

/// `VectrCap.exe` + `.new` -> `VectrCap.exe.new`.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// Windows lets a running exe be renamed but not overwritten: move it aside, then
/// move the staged one in. Puts the old exe back if the second step fails.
fn swap(exe: &Path) -> io::Result<()> {
    let new = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    if !new.exists() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old)?;
    if let Err(error) = fs::rename(&new, exe) {
        let _ = fs::rename(&old, exe);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("0.2", "0.1.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
    }

    #[test]
    fn a_release_needs_both_assets() {
        let json = br#"{"tag_name":"v0.2.0","assets":[
            {"name":"VectrCap.exe","browser_download_url":"https://example.test/a.exe"},
            {"name":"VectrCap.exe.sha256","browser_download_url":"https://example.test/a.sha256"}]}"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.exe_url, "https://example.test/a.exe");
        let missing = br#"{"tag_name":"v0.2.0","assets":[]}"#;
        assert!(parse_release(missing).is_err());
        assert!(parse_release(br#"{"tag_name":"latest","assets":[]}"#).is_err());
    }

    #[test]
    fn the_download_must_match_its_checksum() {
        let exe = b"MZ fake program";
        let hex: String = Sha256::digest(exe)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let expected = parse_sha256(format!("{hex}  VectrCap.exe\n").as_bytes()).unwrap();
        assert!(verify(exe, &expected).is_ok());
        assert!(verify(b"MZ tampered", &expected).is_err());
        assert!(parse_sha256(b"abc").is_none());
    }

    #[test]
    fn swap_moves_the_staged_exe_in_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("VectrCap.exe");
        fs::write(&exe, "old").unwrap();
        assert!(swap(&exe).is_err(), "nothing staged");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        fs::write(sibling(&exe, ".new"), "new").unwrap();
        swap(&exe).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(fs::read_to_string(sibling(&exe, ".old")).unwrap(), "old");
        assert!(!sibling(&exe, ".new").exists());
    }
}
