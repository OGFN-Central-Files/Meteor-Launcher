use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use reqwest::header::RANGE;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{Emitter, Window};
use unrar::Archive;
use zip::read::ZipArchive;

static CANCEL_TOKEN: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));
// Separate cancel flag for the Build Installer so it can't interfere with a launch.
static INSTALL_CANCEL: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

#[tauri::command]
pub fn cancel_download() {
    let mut token = CANCEL_TOKEN.lock().unwrap();
    *token = true;
}

#[tauri::command]
pub fn cancel_install() {
    INSTALL_CANCEL.store(true, Ordering::Relaxed);
}

// =====================  Build Installer (served like paks)  =====================
// Downloads the 10.40 build as INDIVIDUAL FILES from the backend manifest, each with a
// 6-connection ranged download (the same fast path the paks use), straight to
// <dest>\<relative path>. No giant archive, no extraction, no double-disk — a 60 GB
// build never needs temp space beyond the file being fetched. Resumable: a file already
// present at the right size is skipped. Emits `install-progress`
// {phase, progress, downloadedBytes, totalBytes, bytesPerSec, etaSec, currentFile} — a
// DISTINCT event from launch's `download-progress`. Ends with `install-finished`
// {destDir} or `install-error` {message}.

#[derive(serde::Deserialize)]
pub struct BuildFile {
    pub url: String,
    pub path: String, // build-relative path (forward slashes)
    pub size: u64,
}

fn human_eta(remaining: u64, bps: f64) -> f64 {
    if bps <= 1.0 { return 0.0; }
    (remaining as f64) / bps
}

// Download one byte range [start,end) into its own chunk file, resuming from what's
// already there. Adds transferred bytes to `total_done` (an aggregate counter). Sets
// `no_range` if the server ignores Range (200) or over-serves a 206.
fn dl_range(
    client: &Client,
    url: &str,
    chunk_path: &Path,
    start: u64,
    end: u64,
    total_done: &AtomicU64,
    no_range: &AtomicBool,
) -> Result<(), String> {
    let want = end - start;
    let mut have = fs::metadata(chunk_path).map(|m| m.len()).unwrap_or(0);
    if have > want { let _ = fs::remove_file(chunk_path); have = 0; }
    if have > 0 { total_done.fetch_add(have, Ordering::Relaxed); }

    let mut attempt = 0u32;
    while have < want {
        if INSTALL_CANCEL.load(Ordering::Relaxed) { return Err("cancelled".into()); }
        attempt += 1;
        if attempt > 40 { return Err("Download kept dropping — check your connection.".into()); }
        let from = start + have;
        let to = end - 1;
        let resp = match client.get(url).header(RANGE, format!("bytes={}-{}", from, to)).send() {
            Ok(r) => r,
            Err(_) => { thread::sleep(Duration::from_secs(2)); continue; }
        };
        if resp.status().as_u16() == 200 { no_range.store(true, Ordering::Relaxed); return Ok(()); }
        if !resp.status().is_success() { thread::sleep(Duration::from_secs(2)); continue; }
        if let Some(cl) = resp.content_length() {
            if cl > (want - have) { no_range.store(true, Ordering::Relaxed); return Ok(()); }
        }
        let mut resp = resp;
        let mut file = match OpenOptions::new().create(true).append(true).open(chunk_path) {
            Ok(f) => f, Err(e) => return Err(format!("open chunk: {}", e)),
        };
        let mut buf = vec![0u8; 512 * 1024];
        loop {
            if INSTALL_CANCEL.load(Ordering::Relaxed) { return Err("cancelled".into()); }
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let n = (n as u64).min(want - have) as usize;
                    if n == 0 { break; }
                    if let Err(e) = file.write_all(&buf[..n]) { return Err(format!("write: {}", e)); }
                    have += n as u64;
                    total_done.fetch_add(n as u64, Ordering::Relaxed);
                    if have >= want { break; }
                }
                Err(_) => break,
            }
        }
        let _ = file.flush();
    }
    Ok(())
}

// Single-stream fallback for one file (used when the server ignores Range).
fn serial_to_file(client: &Client, url: &str, target: &Path, global_done: &AtomicU64) -> Result<(), String> {
    if let Some(parent) = target.parent() { let _ = fs::create_dir_all(parent); }
    let mut part = target.as_os_str().to_os_string(); part.push(".part"); let part = PathBuf::from(part);
    let mut resp = client.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }
    let mut file = File::create(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 512 * 1024];
    loop {
        if INSTALL_CANCEL.load(Ordering::Relaxed) { return Err("cancelled".into()); }
        match resp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => { file.write_all(&buf[..n]).map_err(|e| e.to_string())?; global_done.fetch_add(n as u64, Ordering::Relaxed); }
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = file.flush();
    if target.exists() { let _ = fs::remove_file(target); }
    fs::rename(&part, target).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

// Download one whole file (parallel-chunked) to `target`, adding transferred bytes to
// the shared `global_done`. Skips a file already on disk at the correct size.
fn download_one_file(
    client: &Client,
    url: &str,
    target: &Path,
    total: u64,
    global_done: &Arc<AtomicU64>,
) -> Result<(), String> {
    if let Ok(m) = fs::metadata(target) {
        if total > 0 && m.len() == total { global_done.fetch_add(total, Ordering::Relaxed); return Ok(()); }
        if total == 0 && m.len() == 0 { return Ok(()); } // empty file already present
    }
    if let Some(parent) = target.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }

    // Zero-byte files (placeholder.txt etc.) can't be range-downloaded — a `bytes=0-0`
    // request against a 0-byte file returns 416. Just create the empty file.
    if total == 0 {
        File::create(target).map_err(|e| format!("create empty {}: {}", target.display(), e))?;
        return Ok(());
    }

    let mut part = target.as_os_str().to_os_string(); part.push(".part"); let part = PathBuf::from(part);
    let chunk_path = |i: usize| -> PathBuf {
        let mut s = part.as_os_str().to_os_string(); s.push(format!(".{}", i)); PathBuf::from(s)
    };
    const CHUNKS: u64 = 6;
    let ranges: Vec<(u64, u64)> = if total >= 16 * 1024 * 1024 {
        let cs = (total + CHUNKS - 1) / CHUNKS;
        (0..CHUNKS).map(|i| (i * cs, ((i + 1) * cs).min(total))).filter(|(s, e)| e > s).collect()
    } else {
        vec![(0, total)]
    };

    let no_range = Arc::new(AtomicBool::new(false));
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    thread::scope(|sc| {
        let mut handles = Vec::new();
        for (i, (s, e)) in ranges.iter().enumerate() {
            let (s, e) = (*s, *e);
            let cp = chunk_path(i);
            let (gd, nr) = (Arc::clone(global_done), Arc::clone(&no_range));
            let cl = client.clone();
            let url = url.to_string();
            handles.push(sc.spawn(move || dl_range(&cl, &url, &cp, s, e, &gd, &nr)));
        }
        for h in handles { if let Ok(Err(er)) = h.join() { errors.lock().unwrap().push(er); } }
    });

    if INSTALL_CANCEL.load(Ordering::Relaxed) {
        for i in 0..ranges.len() { let _ = fs::remove_file(chunk_path(i)); }
        return Err("cancelled".into());
    }
    if no_range.load(Ordering::Relaxed) {
        for i in 0..ranges.len() { let _ = fs::remove_file(chunk_path(i)); }
        return serial_to_file(client, url, target, global_done);
    }
    { let errs = errors.lock().unwrap(); if !errs.is_empty() { return Err(errs.join("; ")); } }

    let mut out = File::create(&part).map_err(|e| format!("assemble create: {}", e))?;
    for i in 0..ranges.len() {
        let cp = chunk_path(i);
        let mut cf = File::open(&cp).map_err(|e| format!("assemble open {}: {}", i, e))?;
        std::io::copy(&mut cf, &mut out).map_err(|e| format!("assemble copy {}: {}", i, e))?;
        drop(cf);
        let _ = fs::remove_file(&cp);
    }
    let _ = out.flush();
    let got = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if total > 0 && got != total {
        let _ = fs::remove_file(&part);
        return Err(format!("wrong size (expected {}, got {})", total, got));
    }
    if target.exists() { let _ = fs::remove_file(target); }
    fs::rename(&part, target).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn install_build_files(window: Window, files: Vec<BuildFile>, dest_dir: String, total_size: u64) -> Result<(), String> {
    thread::spawn(move || {
        INSTALL_CANCEL.store(false, Ordering::Relaxed);
        let dest = PathBuf::from(&dest_dir);
        if let Err(e) = fs::create_dir_all(&dest) {
            let _ = window.emit("install-error", format!("Can't create folder: {}", e));
            return;
        }
        let client = match Client::builder().connect_timeout(Duration::from_secs(30)).build() {
            Ok(c) => c, Err(e) => { let _ = window.emit("install-error", e.to_string()); return; }
        };

        let total = if total_size > 0 { total_size } else { files.iter().map(|f| f.size).sum() };
        let global_done = Arc::new(AtomicU64::new(0));
        let current = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));

        // One global progress/speed/ETA emitter for the whole install.
        let (ewin, egd, ecur, estop) = (window.clone(), Arc::clone(&global_done), Arc::clone(&current), Arc::clone(&stop));
        let start_t = Instant::now();
        let emitter = thread::spawn(move || {
            let mut last = 0u64;
            let mut last_t = Instant::now();
            while !estop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(400));
                let done = egd.load(Ordering::Relaxed).min(total);
                let now = Instant::now();
                let dt = now.duration_since(last_t).as_secs_f64().max(0.001);
                let bps = ((done.saturating_sub(last)) as f64) / dt;
                last = done; last_t = now;
                let progress = if total > 0 { (done as f64 / total as f64) * 100.0 } else { 0.0 };
                let cur = ecur.lock().map(|s| s.clone()).unwrap_or_default();
                let _ = ewin.emit("install-progress", serde_json::json!({
                    "phase": "download", "progress": progress, "downloadedBytes": done, "totalBytes": total,
                    "bytesPerSec": bps, "etaSec": human_eta(total.saturating_sub(done), bps),
                    "elapsedSec": start_t.elapsed().as_secs_f64(), "currentFile": cur,
                }));
            }
        });

        let mut failed: Option<String> = None;
        for f in &files {
            if INSTALL_CANCEL.load(Ordering::Relaxed) { failed = Some("cancelled".into()); break; }
            let rel = f.path.replace('/', "\\");
            let target = dest.join(&rel);
            if let Ok(mut c) = current.lock() { *c = f.path.clone(); }
            if let Err(e) = download_one_file(&client, &f.url, &target, f.size, &global_done) {
                failed = Some(if e == "cancelled" { e } else { format!("{}: {}", f.path, e) });
                break;
            }
        }

        stop.store(true, Ordering::Relaxed);
        let _ = emitter.join();

        if let Some(e) = failed {
            if e == "cancelled" { let _ = window.emit("install-cancelled", "cancelled"); }
            else { let _ = window.emit("install-error", e); }
            return;
        }
        let _ = window.emit("install-progress", serde_json::json!({ "phase": "download", "progress": 100.0, "totalBytes": total, "downloadedBytes": total }));
        let _ = window.emit("install-finished", serde_json::json!({ "destDir": dest_dir }));
    });
    Ok(())
}

// ==============================================================================

#[tauri::command]
pub fn download_build(window: Window, url: String, folder_path: String) -> Result<(), String> {
    thread::spawn(move || {
        let client = Client::builder()
            .timeout(None)
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        if let Err(e) = fs::create_dir_all(&folder_path) {
            let _ = window.emit("download-error", e.to_string());
            return;
        }

        let filename = url
            .split('/')
            .last()
            .unwrap_or("file.dat")
            .replace('%', "_");
        let file_path: PathBuf = Path::new(&folder_path).join(&filename);

        let existing_size = if file_path.exists() {
            fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let mut request = client.get(&url);
        if existing_size > 0 {
            request = request.header(RANGE, format!("bytes={}-", existing_size));
        }

        let mut response = match request.send() {
            Ok(r) => r,
            Err(e) => {
                let _ = window.emit("download-error", e.to_string());
                return;
            }
        };

        if !response.status().is_success() && response.status().as_u16() != 206 {
            let _ = window.emit(
                "download-error",
                format!("HTTP error: {}", response.status()),
            );
            return;
        }

        let total_size = if response.status().as_u16() == 206 {
            existing_size + response.content_length().unwrap_or(0)
        } else {
            response.content_length().unwrap_or(0)
        };

        let mut downloaded: u64 = existing_size;

        let mut file = if existing_size > 0 && response.status().as_u16() == 206 {
            match OpenOptions::new().append(true).open(&file_path) {
                Ok(f) => f,
                Err(e) => {
                    let _ = window.emit("download-error", e.to_string());
                    return;
                }
            }
        } else {
            match File::create(&file_path) {
                Ok(f) => f,
                Err(e) => {
                    let _ = window.emit("download-error", e.to_string());
                    return;
                }
            }
        };

        let mut buffer = [0u8; 8192];
        *CANCEL_TOKEN.lock().unwrap() = false;

        loop {
            if *CANCEL_TOKEN.lock().unwrap() {
                let _ = window.emit("download-cancelled", "cancelled");
                return;
            }

            let bytes_read = match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = window.emit("download-error", e.to_string());
                    return;
                }
            };

            if let Err(e) = file.write(&buffer[..bytes_read]) {
                let _ = window.emit("download-error", e.to_string());
                return;
            }

            downloaded += bytes_read as u64;
            let progress = if total_size > 0 {
                (downloaded as f64 / total_size as f64) * 100.0
            } else {
                0.0
            };

            let _ = window.emit(
                "download-progress",
                serde_json::json!({
                    "downloadedGB": downloaded as f64 / 1024.0 / 1024.0 / 1024.0,
                    "progress": progress,
                    "currentFile": filename,
                    "totalBytes": total_size
                }),
            );
        }

        drop(file);

        if *CANCEL_TOKEN.lock().unwrap() {
            return;
        }

        if filename.ends_with(".zip") {
            if let Ok(file) = File::open(&file_path) {
                if let Ok(mut zip) = ZipArchive::new(file) {
                    let total_files = zip.len();
                    for i in 0..total_files {
                        if *CANCEL_TOKEN.lock().unwrap() {
                            return;
                        }
                        let mut f = zip.by_index(i).unwrap();
                        let outpath = Path::new(&folder_path).join(f.mangled_name());
                        if let Some(parent) = outpath.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        let mut outfile = File::create(&outpath).unwrap();
                        std::io::copy(&mut f, &mut outfile).unwrap();
                        let _ = window.emit(
                            "extract-progress",
                            serde_json::json!({
                                "currentFile": f.name(),
                                "index": i + 1,
                                "total": total_files
                            }),
                        );
                    }
                }
            }
            let _ = fs::remove_file(&file_path);
        } else if filename.ends_with(".rar") {
            let file_count = match Archive::new(&file_path).open_for_listing() {
                Ok(archive) => archive.count(),
                Err(_) => 0,
            };

            match Archive::new(&file_path).open_for_processing() {
                Ok(mut archive) => {
                    let mut extracted = 0;
                    loop {
                        if *CANCEL_TOKEN.lock().unwrap() {
                            return;
                        }

                        match archive.read_header() {
                            Ok(Some(header)) => {
                                let entry = header.entry();
                                let filename_str = entry.filename.to_string_lossy().to_string();

                                let _ = window.emit(
                                    "extract-progress",
                                    serde_json::json!({
                                        "currentFile": filename_str,
                                        "index": extracted + 1,
                                        "total": file_count
                                    }),
                                );

                                archive = match header.extract_with_base(&folder_path) {
                                    Ok(a) => a,
                                    Err(e) => {
                                        let _ = window.emit(
                                            "download-error",
                                            format!("Extract error: {}", e),
                                        );
                                        break;
                                    }
                                };
                                extracted += 1;
                            }
                            Ok(None) => break,
                            Err(e) => {
                                let _ = window
                                    .emit("download-error", format!("Read header error: {}", e));
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = window.emit("download-error", format!("RAR open error: {}", e));
                }
            }

            let _ = fs::remove_file(&file_path);
        }

        *CANCEL_TOKEN.lock().unwrap() = false;
        let _ = window.emit(
            "download-finished",
            serde_json::json!({
                "filePath": folder_path,
                "totalBytes": total_size
            }),
        );
    });

    Ok(())
}
