use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{Read, Write as _};
use std::mem::zeroed;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use winapi::shared::windef::HWND;
use winapi::um::processthreadsapi::GetProcessId;
use winapi::um::shellapi::{ShellExecuteExA, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOA};
use winapi::um::{handleapi::CloseHandle, winbase::CREATE_SUSPENDED, winuser::SW_SHOW};

use tauri::Emitter;
use tauri::Window;

use crate::utilities;

use serde::Deserialize;

// A required game pak the launcher must have on disk, described by the backend
// manifest (GET /api/launcher/paks). `size` is the authoritative byte size — a
// local file whose size doesn't match is treated as missing/outdated and
// redownloaded. Editing the manifest server-side is how paks get updated monthly.
#[derive(Debug, Deserialize)]
pub struct PakEntry {
    pub name: String,
    pub url: String,
    pub size: u64,
    // Optional integrity hash from the backend manifest: sha1 of the LAST
    // min(TAIL_HASH_BYTES, size) bytes of the pak. When present, a size-matching but
    // content-CORRUPT local file (e.g. a pak assembled by the v8.9.0 whole-file-range
    // bug) fails this check and is re-fetched. Absent (older backend / external CDN
    // entry) → size-only check, unchanged behaviour.
    #[serde(default)]
    pub sha: Option<String>,
}

// Must match the backend's TAIL_HASH_BYTES in launcherPaks.ts.
const TAIL_HASH_BYTES: u64 = 8 * 1024 * 1024;

// sha1 of the last min(TAIL_HASH_BYTES, size) bytes of a file, lowercase hex. Cheap
// even for the multi-GB pak (reads only the trailing window). Returns None if the
// file can't be read.
fn tail_sha1(path: &Path, size: u64) -> Option<String> {
    use sha1::{Digest, Sha1};
    let len = TAIL_HASH_BYTES.min(size);
    if len == 0 {
        return None;
    }
    let mut f = fs::File::open(path).ok()?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(size - len)).ok()?;
    let mut hasher = Sha1::new();
    let mut remaining = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let want = (buf.len() as u64).min(remaining) as usize;
        let n = f.read(&mut buf[..want]).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Some(hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect())
}

// A required pak needs (re)downloading if it's missing, the wrong size, or (when the
// manifest supplies an integrity hash) size-valid but content-CORRUPT. When size is 0
// (unknown) an existing file is trusted so we don't refetch large files every launch.
fn pak_needs_download(path: &Path, expected_size: u64, expected_sha: &Option<String>) -> bool {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return true, // missing
    };
    if expected_size != 0 && meta.len() != expected_size {
        return true; // wrong size
    }
    // Size matches. Verify the integrity tail-hash when the manifest provides one: this
    // catches a corrupt-but-right-sized pak and forces ONE clean re-fetch; a correct
    // pak passes and is skipped, so healthy installs never redownload.
    if expected_size != 0 {
        if let Some(sha) = expected_sha {
            if !sha.is_empty() {
                match tail_sha1(path, meta.len()) {
                    // Read the tail and it DIFFERS -> genuinely corrupt, refetch once.
                    Some(local) if !local.eq_ignore_ascii_case(sha) => return true,
                    // Read the tail and it matches -> healthy, skip.
                    Some(_) => {}
                    // Could NOT read the tail (file locked by AV/the running game, transient
                    // IO). The size already matches, so DON'T redownload gigabytes on a hunch —
                    // fail open and trust the size. (Previously this refetched every launch when
                    // the pak happened to be unreadable — the "downloads again every time" bug.)
                    None => {}
                }
            }
        }
    }
    false
}

const CREATE_NO_WINDOW: u32 = 0x08000000;

// The Arc anticheat config (client id + loader url) is fetched from the backend
// at launch time — NOT hardcoded here — so it stays out of the public source and
// can be rotated server-side. This is only a fallback loader url (public, not a
// secret) used if the backend omits one; the client id always comes from backend.
const ARC_EXE_URL_FALLBACK: &str =
    "https://cloud.arc-services.dev/modules/anticheat/Arc.exe";

#[derive(serde::Deserialize)]
struct ArcConfigResp {
    #[serde(rename = "clientId")]
    client_id: String,
    #[serde(rename = "exeUrl")]
    exe_url: Option<String>,
}

// Fetch the Arc client id (and loader url) from the backend. `backend` is the
// host:port the launch was given (e.g. "pulsefn.de:2603"); it may already carry
// a scheme. Fails closed — Arc can't run with a wrong/missing client id anyway.
fn fetch_arc_config(backend: &str) -> Result<(String, String), String> {
    let base = backend.trim().trim_end_matches('/');
    let url = if base.starts_with("http://") || base.starts_with("https://") {
        format!("{}/api/launcher/arc", base)
    } else {
        format!("http://{}/api/launcher/arc", base)
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build http client: {}", e))?;

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("Failed to fetch Arc config: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Arc config request failed: HTTP {}", resp.status()));
    }

    let parsed: ArcConfigResp = resp
        .json()
        .map_err(|e| format!("Failed to parse Arc config: {}", e))?;

    let client_id = parsed.client_id.trim().to_string();
    if client_id.is_empty() {
        return Err("Backend returned an empty Arc client id".to_string());
    }

    let exe_url = parsed
        .exe_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ARC_EXE_URL_FALLBACK.to_string());

    Ok((client_id, exe_url))
}

fn is_hex32(s: &str) -> bool {
    let s = s.trim();
    s.len() == 32 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F'))
}

fn b64url_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(((data.len() + 2) / 3) * 4);
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = (data[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
    } else if rem == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
    }
    out
}

// Per-user token our gameserver's join validation reads as a fallback to extract
// account_id when UniqueId replication bytes are empty (see exloBackend gameserver checks).
// NOTE: this is an UNSIGNED JWT (alg:none) — there is no signing key here. The
// calderaGuid below is a static, non-secret identifier, not a credential.
fn make_caldera_token(account_id: &str) -> String {
    let header = r#"{"alg":"none","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"account_id":"{}","generated":0,"calderaGuid":"3810b863-2a65-4457-9b58-4dab3b482a86","acProvider":"EasyAntiCheat","notes":"","fallback":false}}"#,
        account_id
    );
    format!(
        "{}.{}.x",
        b64url_encode(header.as_bytes()),
        b64url_encode(payload.as_bytes())
    )
}

fn get_remote_file_size(url: &str) -> Result<u64, String> {
    // A HEAD returns only headers, so a small TOTAL timeout is safe here. Without it
    // (the old Client::new(), timeout=None) a black-hole host that accepts the TCP
    // connection but never answers would hang the whole launch on this pre-download
    // size check. On error, should_skip_download falls open to a normal download.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build http client: {}", e))?;
    let response = client
        .head(url)
        .send()
        .map_err(|e| format!("Failed to get file info: {}", e))?;

    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| "Could not determine remote file size".to_string())
}

fn should_skip_download(local_path: &Path, remote_url: &str) -> Result<bool, String> {
    if !local_path.exists() {
        return Ok(false);
    }

    let local_size = fs::metadata(local_path)
        .map_err(|e| format!("Failed to get local file size: {}", e))?
        .len();

    match get_remote_file_size(remote_url) {
        Ok(remote_size) => Ok(local_size == remote_size),
        Err(_) => Ok(false),
    }
}

// Streams a `progress` event for the game-file phase.
fn emit_pak_progress(window: &Window, label: &str, downloaded: u64, total: u64) {
    let pct: i64 = if total > 0 {
        (((downloaded as f64 / total as f64) * 100.0) as i64).min(100)
    } else {
        0
    };
    let _ = window.emit(
        "download-progress",
        serde_json::json!({
            "type": "gamefile",
            "file": label,
            "progress": pct,
            "downloadedMB": downloaded / (1024 * 1024),
            "totalMB": total / (1024 * 1024),
        }),
    );
}

/// Downloads a (multi-GB) game pak RESUMABLY. Writes to `<dest>.part` and, on any
/// connection drop, resumes from the bytes already on disk via an HTTP Range
/// request (`bytes=<have>-`) instead of restarting from zero — so a hiccup near
/// the end costs a few retried megabytes, not the whole 2.8 GB. Only renames the
/// `.part` into place once the full expected size is present, so an interrupted
/// download is never mistaken for a finished one. Flat memory use (256 KB chunks)
/// regardless of file size.
// Parallel multi-connection download: split the file into N byte ranges and fetch
// them concurrently (the backend serves HTTP range / 206). This saturates bandwidth
// far better than one stream, especially for players with high latency to the server
// (e.g. OCE -> EU). Each range downloads to its own .partN file so it resumes
// independently; when all are done they're streamed into place (delete-as-we-go, so
// peak disk stays ~1x). Falls back to the single-stream path for small/unknown sizes
// or if the server ignores Range.
fn download_pak_resumable(
    window: &Window,
    url: &str,
    dest: &Path,
    label: &str,
    expected_size: u64,
) -> Result<(), String> {
    const CHUNKS: u64 = 6;
    const MIN_PARALLEL: u64 = 24 * 1024 * 1024; // below this, one stream is fine
    if expected_size < MIN_PARALLEL {
        return download_pak_serial(window, url, dest, label, expected_size);
    }

    let chunk_size = (expected_size + CHUNKS - 1) / CHUNKS;
    let ranges: Vec<(u64, u64)> = (0..CHUNKS)
        .map(|i| (i * chunk_size, ((i + 1) * chunk_size).min(expected_size)))
        .filter(|(s, e)| e > s)
        .collect();

    let chunk_path = |i: usize| -> PathBuf {
        let mut s = dest.as_os_str().to_os_string();
        s.push(format!(".part{}", i));
        PathBuf::from(s)
    };

    // Seed progress from any chunk bytes already on disk (resume).
    let total_done = Arc::new(AtomicU64::new(0));
    for (i, (s, e)) in ranges.iter().enumerate() {
        let have = fs::metadata(chunk_path(i)).map(|m| m.len()).unwrap_or(0).min(e - s);
        total_done.fetch_add(have, Ordering::Relaxed);
    }
    emit_pak_progress(window, label, total_done.load(Ordering::Relaxed), expected_size);
    let last_emit = Arc::new(AtomicU64::new(total_done.load(Ordering::Relaxed)));

    // Download every range concurrently. A "server ignored Range" signal from any
    // range aborts to the serial path (still correct, just slower).
    let no_range = Arc::new(AtomicU64::new(0));
    let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        let mut handles = Vec::new();
        for (i, (s, e)) in ranges.iter().enumerate() {
            let (i, s, e) = (i, *s, *e);
            let cpath = chunk_path(i);
            let (td, le, nr) = (Arc::clone(&total_done), Arc::clone(&last_emit), Arc::clone(&no_range));
            handles.push(sc.spawn(move || {
                download_range_to_file(window, url, &cpath, s, e, label, expected_size, &td, &le, &nr)
            }));
        }
        for h in handles {
            if let Ok(Err(er)) = h.join() {
                errors.lock().unwrap().push(er);
            }
        }
    });

    if no_range.load(Ordering::Relaxed) > 0 {
        // Server doesn't honour Range — clean up chunk files and do a single stream.
        for i in 0..ranges.len() { let _ = fs::remove_file(chunk_path(i)); }
        return download_pak_serial(window, url, dest, label, expected_size);
    }
    {
        let errs = errors.lock().unwrap();
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }
    }

    // Assemble: stream each chunk into the final .part, deleting as we go (peak ~1x disk).
    let part_path = {
        let mut s = dest.as_os_str().to_os_string();
        s.push(".part");
        PathBuf::from(s)
    };
    {
        let mut out = fs::File::create(&part_path).map_err(|e| format!("assemble create failed: {}", e))?;
        for i in 0..ranges.len() {
            let cp = chunk_path(i);
            let mut cf = fs::File::open(&cp).map_err(|e| format!("assemble open chunk {} failed: {}", i, e))?;
            std::io::copy(&mut cf, &mut out).map_err(|e| format!("assemble copy chunk {} failed: {}", i, e))?;
            drop(cf);
            let _ = fs::remove_file(&cp);
        }
        let _ = out.flush();
    }

    let final_size = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    if final_size != expected_size {
        let _ = fs::remove_file(&part_path);
        return Err(format!(
            "Game file '{}' finished at the wrong size (expected {}, got {}). Please try again.",
            label, expected_size, final_size
        ));
    }
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    fs::rename(&part_path, dest).map_err(|e| format!("Failed to finalize '{}': {}", label, e))?;
    let _ = fs::remove_file(partmeta_path(dest)); // download complete -> sidecar no longer needed
    emit_pak_progress(window, label, final_size, final_size);
    Ok(())
}

// Bytes requested per HTTP range request. Small enough that a per-request timeout can
// safely bound EVERY network operation (so a stalled socket aborts + resumes instead of
// hanging the whole download forever), large enough to keep overhead low.
const DL_WINDOW: u64 = 8 * 1024 * 1024;
// Per-request cap. Only ever fires on a genuinely stalled connection because each request
// asks for at most DL_WINDOW bytes — a working-but-slow link (down to ~70 KB/s) still
// finishes a window inside this budget, so legit slow downloads are NOT killed.
const DL_REQ_TIMEOUT_SECS: u64 = 120;
// Consecutive no-progress attempts before we give up on a span.
const DL_MAX_FAILS: u32 = 50;

// Download one byte range [start, end) into its own chunk file, resuming from the chunk
// file's current length. Fetches in bounded DL_WINDOW-sized requests, each with a total
// timeout — so a connection that stalls mid-stream aborts that request and the loop
// resumes from the bytes already on disk, instead of blocking read() forever (the cause
// of "stuck on downloading paks", especially in the 6-way parallel path where one hung
// chunk froze thread::scope). Sets `no_range` if the server ignores Range (200/over-serve).
fn download_range_to_file(
    window: &Window,
    url: &str,
    chunk_path: &Path,
    start: u64,
    end: u64,
    label: &str,
    expected_size: u64,
    total_done: &AtomicU64,
    last_emit: &AtomicU64,
    no_range: &AtomicU64,
) -> Result<(), String> {
    let want = end - start;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(DL_REQ_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let mut have = fs::metadata(chunk_path).map(|m| m.len()).unwrap_or(0);
    if have > want {
        // Stale/oversized chunk from an earlier run — start this chunk fresh.
        let _ = fs::remove_file(chunk_path);
        have = 0;
    }

    let mut fails: u32 = 0;
    while have < want {
        let from = start + have;
        let win_end = (from + DL_WINDOW).min(end); // exclusive
        let to = win_end - 1; // inclusive
        let expect_win = to - from + 1;
        let before = have;

        let resp = match client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={}-{}", from, to))
            .send()
        {
            Ok(r) => r,
            Err(_) => {
                fails += 1;
                if fails > DL_MAX_FAILS {
                    return Err(format!("Download of '{}' keeps dropping. Check your connection and try again.", label));
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };
        // We asked for a sub-range; the server MUST answer 206. A 200 means it ignored
        // Range -> parallel is impossible, signal a fallback to the serial path.
        if resp.status().as_u16() == 200 {
            no_range.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        if !resp.status().is_success() {
            fails += 1;
            if fails > DL_MAX_FAILS {
                return Err(format!("Download of '{}' failed repeatedly (HTTP {}).", label, resp.status().as_u16()));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        // Integrity guard: the server said 206, but does the body actually match the
        // bytes we asked for? Bun 1.4.0 (pre-fix) answered 206 with a correct
        // Content-Range header yet streamed the WHOLE file — every chunk got the entire
        // pak, so the assembly was N whole-file copies (right total size, wrong content:
        // "installed but skins missing"). If Content-Length exceeds the WINDOW we asked
        // for, the server is over-serving — DON'T trust parallel; fall back to serial.
        if let Some(cl) = resp.content_length() {
            if cl > expect_win {
                no_range.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }
        let mut resp = resp;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(chunk_path)
            .map_err(|e| format!("Failed to open chunk: {}", e))?;
        let mut buf = vec![0u8; 512 * 1024];
        let mut got: u64 = 0; // bytes for THIS window
        loop {
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let n = (n as u64).min(expect_win - got) as usize;
                    if n == 0 { break; }
                    file.write_all(&buf[..n]).map_err(|e| format!("Disk write error on '{}': {}", label, e))?;
                    got += n as u64;
                    have += n as u64;
                    let td = total_done.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
                    let le = last_emit.load(Ordering::Relaxed);
                    if td.saturating_sub(le) >= 8 * 1024 * 1024
                        && last_emit.compare_exchange(le, td, Ordering::Relaxed, Ordering::Relaxed).is_ok()
                    {
                        emit_pak_progress(window, label, td, expected_size);
                    }
                    if got >= expect_win { break; }
                }
                Err(_) => break, // timeout / drop — resume this window from the new `have`
            }
        }
        let _ = file.flush();

        if have > before {
            fails = 0; // progress made — reset the failure streak (long downloads never time out overall)
        } else {
            fails += 1;
            if fails > DL_MAX_FAILS {
                return Err(format!("Download of '{}' stalled. Check your connection and try again.", label));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    Ok(())
}

fn part_path_for(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn partmeta_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".partmeta");
    PathBuf::from(s)
}

// Partial files (.part / .partN) are keyed only on the pak FILENAME, and resume trusts
// only their byte LENGTH. So if a pak is REPLACED in place (same filename, new content —
// the normal monthly update) while a stale partial from an interrupted download lingers,
// resume would splice new-version bytes onto old-version bytes -> a right-SIZED but
// content-CORRUPT pak that the 8 MB tail-hash can't always catch (silent, unrepairable).
// Guard: stamp a sidecar with the target's (size, sha); before downloading, if a leftover
// partial doesn't match the target we're about to fetch, wipe every .part/.partN and start
// clean. A matching sidecar means the partials provably belong to THIS target -> safe resume.
fn ensure_partials_match_target(dest: &Path, expected_size: u64, expected_sha: &Option<String>) {
    let meta = partmeta_path(dest);
    let want = format!("{}:{}", expected_size, expected_sha.as_deref().unwrap_or(""));
    let matches = fs::read_to_string(&meta).map(|s| s.trim() == want).unwrap_or(false);
    if matches {
        return; // leftover partials belong to exactly this target -> safe to resume
    }
    let _ = fs::remove_file(part_path_for(dest));
    for i in 0..16usize {
        let mut s = dest.as_os_str().to_os_string();
        s.push(format!(".part{}", i));
        let _ = fs::remove_file(PathBuf::from(s));
    }
    let _ = fs::write(&meta, want);
}

// Verify a completed `.part` is the right size, then atomically move it into place.
fn finalize_part(part_path: &Path, dest: &Path, label: &str, expected_size: u64) -> Result<u64, String> {
    let final_size = fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
    if expected_size != 0 && final_size != expected_size {
        let _ = fs::remove_file(part_path);
        return Err(format!(
            "Game file '{}' finished at the wrong size (expected {}, got {}). Please try again.",
            label, expected_size, final_size
        ));
    }
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    fs::rename(part_path, dest).map_err(|e| format!("Failed to finalize '{}': {}", label, e))?;
    let _ = fs::remove_file(partmeta_path(dest)); // download complete -> sidecar no longer needed
    Ok(final_size)
}

// Last-resort whole-file download: only used when the size is UNKNOWN or the server
// ignores Range (neither happens with the Pulse backend, which supports range). No range
// = no resume, so each attempt restarts the `.part` from zero. Rarely taken.
fn download_wholefile(
    window: &Window,
    url: &str,
    dest: &Path,
    label: &str,
    expected_size: u64,
) -> Result<(), String> {
    let part_path = part_path_for(dest);
    // No range here = no windowing to bound a stalled read(), so a black-hole socket
    // would hang forever. reqwest 0.11 blocking has no per-read timeout, so cap the whole
    // request with a size-scaled TOTAL budget (~30 KB/s floor, clamped 10 min .. 1 h): a
    // stall aborts the attempt and the loop retries. This path is a rare fallback (the
    // self-hosted backend supports Range, so the big pak never reaches it).
    let budget_secs = (expected_size / 30_000).clamp(600, 3600);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(budget_secs))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    const MAX_ATTEMPTS: u32 = 20;
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if attempt > MAX_ATTEMPTS {
            return Err(format!("Download of '{}' keeps dropping. Check your connection and try again.", label));
        }
        let resp = match client.get(url).send() {
            Ok(r) => r,
            Err(_) => { std::thread::sleep(std::time::Duration::from_secs(3)); continue; }
        };
        if !resp.status().is_success() {
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }
        let total = if expected_size != 0 { expected_size } else { resp.content_length().unwrap_or(0) };
        let mut resp = resp;
        // No range => cannot resume => always start the .part fresh.
        let mut file = fs::File::create(&part_path).map_err(|e| format!("Failed to create file: {}", e))?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut downloaded: u64 = 0;
        let mut last_emit: u64 = 0;
        emit_pak_progress(window, label, 0, total);
        loop {
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    file.write_all(&buf[..n]).map_err(|e| format!("Disk write error on '{}': {}", label, e))?;
                    downloaded += n as u64;
                    if downloaded - last_emit >= EMIT_EVERY || (total > 0 && downloaded >= total) {
                        last_emit = downloaded;
                        emit_pak_progress(window, label, downloaded, total);
                    }
                }
                Err(_) => break, // dropped — retry from scratch
            }
        }
        let _ = file.flush();

        let have = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if expected_size != 0 {
            if have >= expected_size { break; }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        } else {
            break; // unknown size: clean EOF is the only completion signal
        }
    }
    let final_size = finalize_part(&part_path, dest, label, expected_size)?;
    emit_pak_progress(window, label, final_size, final_size);
    Ok(())
}

// Single-connection pak download. When the size is known (the normal case) it fetches in
// bounded DL_WINDOW range requests, each with a per-request timeout, so a stalled socket
// aborts + resumes instead of blocking read() forever. Used for small paks (<24 MB) and as
// the fallback when parallel detects the server isn't honouring Range.
fn download_pak_serial(
    window: &Window,
    url: &str,
    dest: &Path,
    label: &str,
    expected_size: u64,
) -> Result<(), String> {
    // Unknown size can't be windowed (no end offset) — stream the whole file.
    if expected_size == 0 {
        return download_wholefile(window, url, dest, label, expected_size);
    }

    let part_path = part_path_for(dest);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(DL_REQ_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let mut downloaded = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    if downloaded > expected_size {
        // Stale/oversized .part from an earlier run — start fresh.
        let _ = fs::remove_file(&part_path);
        downloaded = 0;
    }
    emit_pak_progress(window, label, downloaded, expected_size);

    let mut fails: u32 = 0;
    let mut last_emit = downloaded;
    while downloaded < expected_size {
        let from = downloaded;
        let win_end = (from + DL_WINDOW).min(expected_size); // exclusive
        let to = win_end - 1; // inclusive
        let expect_win = to - from + 1;
        let before = downloaded;

        let resp = match client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={}-{}", from, to))
            .send()
        {
            Ok(r) => r,
            Err(_) => {
                fails += 1;
                if fails > DL_MAX_FAILS {
                    return Err(format!("Download of '{}' keeps dropping. Check your connection and try again.", label));
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };
        // Server ignored Range (200) or over-served a 206 → it doesn't do ranges
        // correctly; restart cleanly via the whole-file path (immune to range bugs).
        let over = resp.content_length().map_or(false, |cl| cl > expect_win);
        if resp.status().as_u16() == 200 || over {
            drop(resp);
            let _ = fs::remove_file(&part_path);
            return download_wholefile(window, url, dest, label, expected_size);
        }
        if !resp.status().is_success() {
            fails += 1;
            if fails > DL_MAX_FAILS {
                return Err(format!("Download of '{}' failed repeatedly (HTTP {}).", label, resp.status().as_u16()));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }

        let mut resp = resp;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
            .map_err(|e| format!("Failed to open file: {}", e))?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut got: u64 = 0;
        loop {
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let n = (n as u64).min(expect_win - got) as usize;
                    if n == 0 { break; }
                    file.write_all(&buf[..n]).map_err(|e| format!("Disk write error on '{}': {}", label, e))?;
                    got += n as u64;
                    downloaded += n as u64;
                    if downloaded - last_emit >= 8 * 1024 * 1024 || downloaded >= expected_size {
                        last_emit = downloaded;
                        emit_pak_progress(window, label, downloaded, expected_size);
                    }
                    if got >= expect_win { break; }
                }
                Err(_) => break, // timeout / drop — resume this window from the new `downloaded`
            }
        }
        let _ = file.flush();

        if downloaded > before {
            fails = 0;
        } else {
            fails += 1;
            if fails > DL_MAX_FAILS {
                return Err(format!("Download of '{}' stalled. Check your connection and try again.", label));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }

    let final_size = finalize_part(&part_path, dest, label, expected_size)?;
    emit_pak_progress(window, label, final_size, final_size);
    Ok(())
}

/// Derives the local filename a custom-pak/extra-DLL link would be saved as —
/// shared between download and cleanup so "Verify Files" removes exactly the
/// files launch would (re)create, not anything the player added themselves.
fn derive_link_filename(link: &str, idx: usize, fallback_prefix: &str, fallback_ext: &str) -> String {
    let clean_url = link.split('?').next().unwrap_or(link);
    match clean_url.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.replace('%', "_"),
        _ => format!("{}_{}.{}", fallback_prefix, idx, fallback_ext),
    }
}

/// Ensures every file the launcher itself manages (redirect DLL, Arc loader +
/// config, custom paks) is present and correctly sized, downloading only what's
/// missing or mismatched. Shared by launch_game and verify_build_files so both
/// "launch" and "repair" go through the exact same setup path.
fn ensure_launcher_managed_files(
    window: &Window,
    game_game_directory: &Path,
    build_root: &Path,
    paks_dir: &Path,
    redirect_link: &str,
    backend: &str,
    use_custom_paks: bool,
    custom_paks_links: &[String],
    paks_manifest: &[PakEntry],
) -> Result<PathBuf, String> {
    let game_dll_path = utilities::handle_game_dll_path(game_game_directory);

    if game_dll_path.exists() {
        utilities::remove_game_dll_sync(game_game_directory)
            .map_err(|err| format!("Failed to remove game DLL: {}", err))?;
    }

    match should_skip_download(&game_dll_path, redirect_link) {
        Ok(true) => {
            let _ = window.emit(
                "download-progress",
                serde_json::json!({
                    "type": "main_dll",
                    "file": game_dll_path.file_name().unwrap_or_default().to_string_lossy(),
                    "progress": 100,
                    "stage": "skipped"
                }),
            );
        }
        _ => {
            utilities::download_file(redirect_link, &game_dll_path)
                .map_err(|err| format!("Failed to download game DLL: {}", err))?;
        }
    }

    if !game_dll_path.exists() {
        return Err("Failed to find Redirect".to_string());
    }

    // Arc anticheat: re-fetch the loader fresh and point it at the real shipping
    // exe via Config.json. Arc.exe (not FortniteClient directly) is what actually
    // gets launched, so it can wrap the game with anticheat.
    let arc_dir = build_root.join("Arc");
    if !arc_dir.exists() {
        std::fs::create_dir_all(&arc_dir)
            .map_err(|e| format!("Failed to create Arc dir: {}", e))?;
    }
    let arc_exe_path = arc_dir.join("Arc.exe");
    let arc_config_path = arc_dir.join("Config.json");

    if arc_exe_path.exists() {
        let _ = std::fs::remove_file(&arc_exe_path);
    }
    if arc_config_path.exists() {
        let _ = std::fs::remove_file(&arc_config_path);
    }

    // Pull the Arc client id + loader url from the backend (kept out of source).
    let (arc_client_id, arc_exe_url) = fetch_arc_config(backend)?;

    utilities::download_file(&arc_exe_url, &arc_exe_path)
        .map_err(|e| format!("Failed to download Arc: {}", e))?;

    let arc_config = format!(
        "{{\n    \"ClientID\": \"{}\",\n    \"Executable\": \"FortniteGame\\\\Binaries\\\\Win64\\\\FortniteClient-Win64-Shipping.exe\"\n}}",
        arc_client_id
    );
    std::fs::write(&arc_config_path, arc_config)
        .map_err(|e| format!("Failed to write Arc config: {}", e))?;

    if !arc_exe_path.exists() {
        return Err("Failed to find Arc (game bootstrapper)".to_string());
    }

    std::fs::create_dir_all(paks_dir).map_err(|e| format!("Failed to create Paks dir: {}", e))?;

    if use_custom_paks && !custom_paks_links.is_empty() {
        for (idx, link) in custom_paks_links.iter().enumerate() {
            let link = link.trim();
            if link.is_empty() {
                continue;
            }

            let filename = derive_link_filename(link, idx, "CustomPak", "pak");
            let save_path = paks_dir.join(&filename);

            match should_skip_download(&save_path, link) {
                Ok(true) => {
                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "pak",
                            "file": filename,
                            "progress": 100,
                            "stage": "skipped"
                        }),
                    );
                }
                _ => {
                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "pak",
                            "file": filename,
                            "progress": 0
                        }),
                    );

                    utilities::download_file(link, &save_path)
                        .map_err(|err| format!("Failed to download custom pak '{}': {}", link, err))?;

                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "pak",
                            "file": filename,
                            "progress": 100
                        }),
                    );
                }
            }
        }
    }

    // Required game paks from the backend manifest. Downloaded for everyone; a
    // file is fetched only when missing or the wrong size (authoritative size
    // from the manifest). Emitted as "gamefile" so the UI marks the phase
    // non-cancelable. When a pak is updated server-side (new url + size), the
    // size mismatch makes every launcher redownload it on the next launch.
    for pak in paks_manifest {
        let filename = Path::new(&pak.name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if filename.is_empty() || pak.url.trim().is_empty() {
            continue;
        }

        let save_path = paks_dir.join(filename);

        if !pak_needs_download(&save_path, pak.size, &pak.sha) {
            let _ = window.emit(
                "download-progress",
                serde_json::json!({
                    "type": "gamefile",
                    "file": filename,
                    "progress": 100,
                    "stage": "skipped"
                }),
            );
            continue;
        }

        let _ = window.emit(
            "download-progress",
            serde_json::json!({
                "type": "gamefile",
                "file": filename,
                "progress": 0
            }),
        );

        // Discard any leftover partials that belong to a DIFFERENT version of this pak
        // (replaced-in-place update) before resuming, so we never splice old bytes onto
        // new. Matching partials are kept for a normal resume.
        ensure_partials_match_target(&save_path, pak.size, &pak.sha);

        // Resumable + retrying: only renames into place once the full expected
        // size is on disk, so a drop near the end resumes instead of restarting,
        // and an interrupted download is never mistaken for a finished one.
        download_pak_resumable(window, pak.url.trim(), &save_path, filename, pak.size)
            .map_err(|err| format!("Failed to download game file '{}': {}", filename, err))?;
    }

    Ok(arc_exe_path)
}

fn build_root_and_dirs(file_path: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let game_path = PathBuf::from(file_path);
    let game_game_directory = game_path
        .parent()
        .expect("Failed to get parent directory")
        .parent()
        .expect("Failed to get FortniteGame directory")
        .to_path_buf();
    let build_root = game_game_directory
        .parent()
        .expect("Failed to get FortniteGame directory")
        .parent()
        .expect("Failed to get build root directory")
        .to_path_buf();
    let paks_dir = build_root.join("FortniteGame").join("Content").join("Paks");
    (game_path, game_game_directory, build_root, paks_dir)
}

// Delete named pak files from a build's Paks folder. Used when an optional toggle
// (Mobile / Bubble Builds) is turned OFF, so the mod is actually removed rather than
// left behind. `filenames` are basenames only (the caller passes manifest file names);
// anything with a path separator or ".." is skipped so we can never escape the folder.
#[tauri::command]
pub fn remove_paks(file_path: String, filenames: Vec<String>) -> Result<Vec<String>, String> {
    let paks_dir = match PathBuf::from(&file_path)
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
    {
        Some(root) => root.join("FortniteGame").join("Content").join("Paks"),
        None => return Err("could not resolve Paks folder from game path".into()),
    };

    let mut removed = Vec::new();
    for name in filenames {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            continue;
        }
        let p = paks_dir.join(&name);
        if p.exists() && fs::remove_file(&p).is_ok() {
            removed.push(name);
        }
    }
    Ok(removed)
}

#[tauri::command]
pub async fn launch_game(
    window: Window,
    file_path: String,
    email: String,
    password: String,
    redirect_link: String,
    backend: String,
    inject_extra_dlls: bool,
    extra_dll_links: Vec<String>,
    use_custom_paks: bool,
    custom_paks_links: Vec<String>,
    extra_dll_options: Vec<std::collections::HashMap<String, String>>,
    paks_manifest: Vec<PakEntry>,
    account_id: String,
    arc_token: String,
    edit_on_release: bool,
    disable_pre_edits: bool,
    reset_on_release: bool,
) -> Result<bool, String> {
    // Heavy, blocking work (multi-GB pak downloads, DLL injection, process
    // spawning + sleeps) must NOT run on the main thread or the window freezes
    // and Windows shows "Not Responding". Run it on a blocking worker so the UI
    // stays responsive and download-progress events keep flowing.
    tauri::async_runtime::spawn_blocking(move || {
        launch_game_impl(
            window,
            file_path,
            email,
            password,
            redirect_link,
            backend,
            inject_extra_dlls,
            extra_dll_links,
            use_custom_paks,
            custom_paks_links,
            extra_dll_options,
            paks_manifest,
            account_id,
            arc_token,
            edit_on_release,
            disable_pre_edits,
            reset_on_release,
        )
    })
    .await
    .map_err(|e| format!("Launch task failed: {}", e))?
}

fn launch_game_impl(
    window: Window,
    file_path: String,
    email: String,
    password: String,
    redirect_link: String,
    backend: String,
    inject_extra_dlls: bool,
    extra_dll_links: Vec<String>,
    use_custom_paks: bool,
    custom_paks_links: Vec<String>,
    extra_dll_options: Vec<std::collections::HashMap<String, String>>,
    paks_manifest: Vec<PakEntry>,
    account_id: String,
    arc_token: String,
    edit_on_release: bool,
    disable_pre_edits: bool,
    reset_on_release: bool,
) -> Result<bool, String> {
    let _ = utilities::kill_all_procs();

    // Force a FRESH hotfix pull on every launch. Fortnite caches the cloud/system hotfix
    // (DefaultGame.ini — which drives the mode tiles / playlists) in PersistentDownloadDir
    // and will NOT re-download it on a normal relaunch, so backend changes (e.g. enabling
    // the Duos Arena playlist) never reach the client — it keeps showing the stale modes.
    // Deleting this cache before launch makes the client re-fetch the current config from
    // the backend every time. It only holds downloaded cloud content, so it's safe to wipe
    // (the game re-creates + re-downloads it); user settings/keybinds live elsewhere and are
    // untouched.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let cloud = std::path::Path::new(&local)
            .join("FortniteGame")
            .join("Saved")
            .join("PersistentDownloadDir");
        if cloud.exists() {
            match std::fs::remove_dir_all(&cloud) {
                Ok(_) => println!("[hotfix] cleared cloud cache: {}", cloud.display()),
                Err(e) => println!("[hotfix] could not clear cloud cache ({}): {}", cloud.display(), e),
            }
        }
    }

    let (_game_path, game_game_directory, build_root, paks_dir) = build_root_and_dirs(&file_path);
    let game_game_directory = game_game_directory.as_path();
    let build_root = build_root.as_path();

    let cwd = game_game_directory.join("Win64");
    let fn_launcher = cwd.join("FortniteLauncher.exe");
    let eac = cwd.join("FortniteClient-Win64-Shipping_BE.exe");

    let arc_exe_path = ensure_launcher_managed_files(
        &window,
        game_game_directory,
        build_root,
        &paks_dir,
        &redirect_link,
        &backend,
        use_custom_paks,
        &custom_paks_links,
        &paks_manifest,
    )?;

    let effective_account_id = {
        let v = account_id.trim();
        if is_hex32(v) {
            v.to_string()
        } else if is_hex32(email.trim()) {
            email.trim().to_string()
        } else {
            String::new()
        }
    };

    if effective_account_id.is_empty() {
        return Err("Missing accountId for launch (required for Arc/caldera).".to_string());
    }

    let caldera = make_caldera_token(&effective_account_id);

    let mut combined_args = vec![
        "-epicapp=Fortnite",
        "-epicenv=Prod",
        "-epiclocale=en-us",
        "-epicportal",
        "-nobe",
        "-fromfl=eac",
        "-nocodeguards",
        "-nouac",
        // Public, community-shared Fortnite client launch flag (not a Pulse
        // credential — it authenticates nothing against our backend).
        "-fltoken=3db3ba5dcbd2e16703f3978d",
        "-caldera=",
        "-skippatchcheck",
        "-AUTH_TYPE=exchangecode",
        "-useallavailablecores",
        "-steamimportavailable",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect::<Vec<String>>();

    if let Some(pos) = combined_args.iter().position(|a| a == "-caldera=") {
        combined_args[pos] = format!("-caldera={}", caldera);
    }

    combined_args.insert(combined_args.len() - 3, format!("-AUTH_LOGIN={}", email));
    combined_args.insert(
        combined_args.len() - 3,
        format!("-AUTH_PASSWORD={}", password),
    );

    if redirect_link == "https://ttttttttttttttttttttttttttttttttttt.vercel.app/Tellurium.dll" {
        combined_args.push(format!("-backend={}", backend));
    }

    // Build-editor behaviour toggles — direct Fortnite launch args, no DLL involved.
    if edit_on_release {
        combined_args.push("-eor".to_string());
    }
    if disable_pre_edits {
        combined_args.push("-dpe".to_string());
    }
    if reset_on_release {
        combined_args.push("-ror".to_string());
    }

    // Only the old "session" credential type used -t. Under the Exchange code
    // credential type there's no client token, so skip it when empty.
    if !arc_token.trim().is_empty() {
        combined_args.push(format!("-t={}", arc_token));
    }

    let combined_args_os: Vec<OsString> = combined_args
        .iter()
        .map(|arg| OsString::from(arg))
        .collect();

    let combined_args_str: String = combined_args_os
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    let hwnd: HWND = std::ptr::null_mut();

    // Launch via Arc (the anticheat bootstrapper), which loads the game.
    let exe_str = arc_exe_path.to_str().ok_or("Invalid path to executable")?;
    let exe_cstring = CString::new(exe_str).map_err(|e| format!("CString error: {}", e))?;

    let args_cstring =
        CString::new(combined_args_str).map_err(|e| format!("CString error: {}", e))?;

    let lp_file = exe_cstring;
    let lp_params = args_cstring;
    let lp_verb = CString::new("runas").unwrap();

    let mut sei: SHELLEXECUTEINFOA = unsafe { zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOA>() as u32;
    // Only request a handle to the launched process when we actually need its PID for DLL
    // injection. In the normal Arc build we inject nothing — and asking for a handle to the
    // Arc process trips Arc's anti-tamper (Detection 27: "launcher.exe opening a
    // PROCESS_ALL_ACCESS handle to Arc"), which makes Arc close the launcher. When Arc is off
    // + we do inject, exe_str is the game exe, so the handle is to the game, not Arc.
    sei.fMask = if inject_extra_dlls { SEE_MASK_NOCLOSEPROCESS } else { 0 };
    sei.hwnd = hwnd;
    sei.lpVerb = lp_verb.as_ptr();
    sei.lpFile = lp_file.as_ptr();
    sei.lpParameters = lp_params.as_ptr();
    sei.nShow = SW_SHOW;

    let success = unsafe { ShellExecuteExA(&mut sei) };
    if success == 0 {
        return Err("ShellExecuteExA failed".to_string());
    }

    let pid: u32 = if inject_extra_dlls {
        if sei.hProcess.is_null() {
            return Err("Failed to get process handle".to_string());
        }
        let p = unsafe { GetProcessId(sei.hProcess) };
        unsafe { CloseHandle(sei.hProcess) };
        p
    } else {
        // No handle requested (fMask=0) -> nothing to close, no anti-tamper detection.
        0
    };

    std::thread::sleep(std::time::Duration::from_secs(5));

    let _eac_proc = Command::new(&eac)
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .args(
            combined_args_os
                .iter()
                .map(|arg| arg as &OsStr)
                .collect::<Vec<&OsStr>>(),
        )
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start EAC: {}", e));

    let _launcher_proc = Command::new(&fn_launcher)
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .args(
            combined_args_os
                .iter()
                .map(|arg| arg as &OsStr)
                .collect::<Vec<&OsStr>>(),
        )
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start Launcher: {}", e));

    std::thread::sleep(std::time::Duration::from_secs(5));

    if inject_extra_dlls {
        for (idx, link) in extra_dll_links.iter().enumerate() {
            if link.trim().is_empty() {
                continue;
            }

            let filename = derive_link_filename(link, idx, "ExtraDll", "dll");
            let save_path = game_game_directory.join("Win64").join(&filename);

            match should_skip_download(&save_path, link) {
                Ok(true) => {
                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "dll",
                            "file": filename,
                            "progress": 100,
                            "stage": "skipped"
                        }),
                    );
                }
                _ => {
                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "dll",
                            "file": filename,
                            "progress": 0
                        }),
                    );

                    if let Err(err) = utilities::download_file(link, &save_path) {
                        return Err(format!("Failed to download extra DLL '{}': {}", link, err));
                    }

                    let _ = window.emit(
                        "download-progress",
                        serde_json::json!({
                            "type": "dll",
                            "file": filename,
                            "progress": 100
                        }),
                    );
                }
            }

            // NON-FATAL: an extra DLL (e.g. the Pulse overlay) failing to inject must
            // NEVER block the game from launching. If injection fails we log it and move
            // on — the player still gets into the game, just without the overlay. (The
            // overlay itself is SEH-guarded so a bad offset can't crash the game either.)
            if let Some(path_str) = save_path.to_str() {
                if let Err(e) = utilities::inject_dll(pid, path_str) {
                    eprintln!("[extra-dll] inject of '{}' failed (non-fatal): {}", save_path.display(), e);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(300));
        }

        for option in extra_dll_options.iter() {
            for (_key, link) in option.iter() {
                if link.trim().is_empty() {
                    continue;
                }

                let clean_link = link.split('?').next().unwrap_or(link);

                let filename = match clean_link.rsplit('/').next() {
                    Some(name) if !name.is_empty() => name.replace('%', "_"),
                    _ => "OptionDll.dll".to_string(),
                };

                let save_path = game_game_directory.join("Win64").join(&filename);

                match should_skip_download(&save_path, link) {
                    Ok(true) => {
                        let _ = window.emit(
                            "download-progress",
                            serde_json::json!({
                                "type": "dll",
                                "file": filename,
                                "progress": 100,
                                "stage": "skipped"
                            }),
                        );
                    }
                    _ => {
                        let _ = window.emit(
                            "download-progress",
                            serde_json::json!({
                                "type": "dll",
                                "file": filename,
                                "progress": 0
                            }),
                        );

                        if let Err(err) = utilities::download_file(link, &save_path) {
                            return Err(format!(
                                "Failed to download option DLL '{}': {}",
                                link, err
                            ));
                        }

                        let _ = window.emit(
                            "download-progress",
                            serde_json::json!({
                                "type": "dll",
                                "file": filename,
                                "progress": 100
                            }),
                        );
                    }
                }

                // NON-FATAL (same rationale as the extra_dll_links loop above): a failed
                // option-DLL inject logs and continues instead of blocking the launch.
                if let Some(path_str) = save_path.to_str() {
                    if let Err(e) = utilities::inject_dll(pid, path_str) {
                        eprintln!("[option-dll] inject of '{}' failed (non-fatal): {}", save_path.display(), e);
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }

    Ok(true)
}

/// Repairs a build: removes every file the launcher itself manages (redirect
/// DLL, Arc loader, configured custom paks/extra DLLs) and re-applies them
/// fresh via the same setup path launch_game uses. Never touches the player's
/// own Fortnite files or any pak it didn't put there itself.
#[tauri::command]
pub async fn verify_build_files(
    window: Window,
    file_path: String,
    redirect_link: String,
    backend: String,
    use_custom_paks: bool,
    custom_paks_links: Vec<String>,
    inject_extra_dlls: bool,
    extra_dll_links: Vec<String>,
    paks_manifest: Vec<PakEntry>,
) -> Result<bool, String> {
    // Same as launch: verify re-downloads managed files (incl. the big paks), so
    // run it off the main thread to keep the UI responsive.
    tauri::async_runtime::spawn_blocking(move || {
        verify_build_files_impl(
            window,
            file_path,
            redirect_link,
            backend,
            use_custom_paks,
            custom_paks_links,
            inject_extra_dlls,
            extra_dll_links,
            paks_manifest,
        )
    })
    .await
    .map_err(|e| format!("Verify task failed: {}", e))?
}

fn verify_build_files_impl(
    window: Window,
    file_path: String,
    redirect_link: String,
    backend: String,
    use_custom_paks: bool,
    custom_paks_links: Vec<String>,
    inject_extra_dlls: bool,
    extra_dll_links: Vec<String>,
    paks_manifest: Vec<PakEntry>,
) -> Result<bool, String> {
    let (_game_path, game_game_directory, build_root, paks_dir) = build_root_and_dirs(&file_path);
    let game_game_directory = game_game_directory.as_path();
    let build_root = build_root.as_path();

    let _ = window.emit(
        "verify-build",
        serde_json::json!({ "file": "Redirect DLL", "status": "checking" }),
    );

    let game_dll_path = utilities::handle_game_dll_path(game_game_directory);
    if game_dll_path.exists() {
        utilities::remove_game_dll_sync(game_game_directory)
            .map_err(|e| format!("Failed to remove game DLL: {}", e))?;
        let _ = window.emit(
            "verify-build",
            serde_json::json!({ "file": "Redirect DLL", "status": "deleted" }),
        );
    }

    let arc_dir = build_root.join("Arc");
    if arc_dir.exists() {
        std::fs::remove_dir_all(&arc_dir)
            .map_err(|e| format!("Failed to remove Arc folder: {}", e))?;
        let _ = window.emit(
            "verify-build",
            serde_json::json!({ "file": "Arc", "status": "deleted" }),
        );
    }

    if use_custom_paks {
        for (idx, link) in custom_paks_links.iter().enumerate() {
            let link = link.trim();
            if link.is_empty() {
                continue;
            }
            let filename = derive_link_filename(link, idx, "CustomPak", "pak");
            let path = paks_dir.join(&filename);
            if path.exists() {
                let _ = fs::remove_file(&path);
                let _ = window.emit(
                    "verify-build",
                    serde_json::json!({ "file": filename, "status": "deleted" }),
                );
            }
        }
    }

    if inject_extra_dlls {
        for (idx, link) in extra_dll_links.iter().enumerate() {
            let link = link.trim();
            if link.is_empty() {
                continue;
            }
            let filename = derive_link_filename(link, idx, "ExtraDll", "dll");
            let path = game_game_directory.join("Win64").join(&filename);
            if path.exists() {
                let _ = fs::remove_file(&path);
                let _ = window.emit(
                    "verify-build",
                    serde_json::json!({ "file": filename, "status": "deleted" }),
                );
            }
        }
    }

    // Manifest game paks are NOT force-deleted here. Verify used to remove them so
    // ensure_* re-downloaded them fresh — but that forced a full re-download of EVERY
    // pak on every verify, including the ~3.3 GB pakchunkPulseWindowsClient.pak, which
    // made verify slow and FAIL on any interruption ("Failed to verify build files").
    // ensure_launcher_managed_files already repairs paks correctly: pak_needs_download()
    // re-downloads a pak only when it's missing, the wrong size, or its tail-sha1 doesn't
    // match the manifest — so a corrupt/outdated pak is still repaired, while a good pak
    // is left in place. Only the redirect DLL + Arc (small, above) are wiped + refreshed.
    // (To force a full pak wipe, delete the build's Paks folder manually.)

    ensure_launcher_managed_files(
        &window,
        game_game_directory,
        build_root,
        &paks_dir,
        &redirect_link,
        &backend,
        use_custom_paks,
        &custom_paks_links,
        &paks_manifest,
    )?;

    let _ = window.emit(
        "verify-build",
        serde_json::json!({ "file": "All files", "status": "done" }),
    );

    Ok(true)
}

#[tauri::command]
pub fn close_game() -> Result<(), String> {
    let _ = utilities::kill_all_procs();
    Ok(())
}
