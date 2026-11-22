use crate::modules::game;
use crate::modules::settings;
use crate::modules::updater;
use std::{
    fs,
    path::{Path, PathBuf},
};
use declarative_discord_rich_presence::{
    activity::{Activity, Assets, Button, Timestamps},
    DeclarativeDiscordIpcClient,
};

mod download;
mod modules;
mod utilities;

#[tauri::command]
async fn check_file_exists(path: &str) -> Result<bool, String> {
    let file_path = std::path::PathBuf::from(path);

    if !file_path.exists() {
        return Ok(false);
    }

    Ok(true)
}

#[tauri::command]
async fn file_exists(file_path: String) -> Result<bool, String> {
    let file_path = Path::new(&file_path);
    Ok(file_path.exists())
}

#[tauri::command]
async fn delete_file(file_path: String) -> Result<bool, String> {
    let path = Path::new(&file_path);

    if !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    if path.is_dir() {
        return Err(format!("Path is a directory, not a file: {}", file_path));
    }

    fs::remove_file(path).map_err(|e| format!("Failed to delete file '{}': {}", file_path, e))?;

    Ok(true)
}

#[tauri::command]
fn get_directory_size(path: String) -> Result<u64, String> {
    fn dir_size(path: &PathBuf) -> Result<u64, String> {
        let mut size = 0;
        if path.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.is_dir() {
                    size += dir_size(&path)?;
                } else {
                    size += fs::metadata(&path).map_err(|e| e.to_string())?.len();
                }
            }
        } else if path.is_file() {
            size += fs::metadata(path).map_err(|e| e.to_string())?.len();
        }
        Ok(size)
    }

    dir_size(&PathBuf::from(path))
}

#[tauri::command]
fn locate_version(file_path: &str) -> Result<Vec<String>, String> {
    let file_contents = fs::read(file_path)
        .map_err(|error| format!("Failed to read file '{}': {}", file_path, error))?;

    const VERSION_SIGNATURE: &[u8] = &[
        0x2b, 0x00, 0x2b, 0x00, 0x46, 0x00, 0x6f, 0x00, 0x72, 0x00, 0x74, 0x00, 0x6e, 0x00, 0x69,
        0x00, 0x74, 0x00, 0x65, 0x00, 0x2b, 0x00,
    ];

    let mut version_strings = Vec::new();
    let signature_len = VERSION_SIGNATURE.len();

    for match_pos in find_pattern_positions(&file_contents, VERSION_SIGNATURE) {
        if let Some(version_text) = extract_version_string(&file_contents, match_pos, signature_len)
        {
            version_strings.push(version_text);
        }
    }

    Ok(version_strings)
}

fn find_pattern_positions(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(
            |(idx, window)| {
                if window == needle {
                    Some(idx)
                } else {
                    None
                }
            },
        )
        .collect()
}

fn extract_version_string(
    buffer: &[u8],
    pattern_start: usize,
    pattern_len: usize,
) -> Option<String> {
    const SEARCH_RANGE: usize = 64;

    let text_start = pattern_start;
    let search_end = (pattern_start + pattern_len + SEARCH_RANGE).min(buffer.len());

    let string_end = locate_string_terminator(&buffer[pattern_start + pattern_len..search_end])?;
    let total_length = pattern_len + string_end;

    if total_length % 2 != 0 {
        return None;
    }

    let utf16_data: Vec<u16> = buffer[text_start..text_start + total_length]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    let decoded_string = String::from_utf16_lossy(&utf16_data);
    Some(decoded_string.trim_matches('\0').trim().to_string())
}

fn locate_string_terminator(data: &[u8]) -> Option<usize> {
    data.chunks_exact(2)
        .position(|chunk| chunk == [0x00, 0x00])
        .map(|pos| pos * 2)
        .or_else(|| Some(data.len().min(64)))
}

#[tauri::command]
fn is_fn_running() -> bool {
    // Check for the game by enumerating process NAMES via a Toolhelp snapshot. This does
    // NOT OpenProcess any individual process. The old sysinfo `System::new_all()` scan
    // opened a full-access handle to EVERY process — including the Arc anti-cheat — which
    // Arc flagged ("launcher.exe opening handle to Arc with PROCESS_ALL_ACCESS",
    // Detection 27) and used to close the launcher. Name-only enumeration opens zero
    // per-process handles, so the anti-cheat has nothing to detect.
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                if name.eq_ignore_ascii_case("FortniteClient-Win64-Shipping.exe") {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

// Discord Rich Presence — shows "Playing Pulse" (+ a Join Discord button) in the user's
// Discord status while the launcher is open. On its own thread: enable() runs the crate's
// reconnect loop; we re-apply the activity every 15s so it survives Discord restarts
// (set_activity is a no-op while disconnected and does NOT persist the desired state).
// Client ID = the Pulse Discord application (its NAME is what shows after "Playing").
const DISCORD_RPC_CLIENT_ID: &str = "1530551727303032883";
fn start_discord_rpc() {
    std::thread::spawn(|| {
        let client = DeclarativeDiscordIpcClient::new(DISCORD_RPC_CLIENT_ID);
        client.enable();
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        loop {
            let _ = client.set_activity(
                Activity::new()
                    .details("Playing Pulse")
                    .state("Chapter 1 · Season X")
                    .assets(Assets::new().large_image("logo").large_text("Pulse"))
                    .timestamps(Timestamps::new().start(start))
                    .buttons(vec![Button::new(
                        "Join Discord".to_string(),
                        "https://discord.gg/pulsefn".to_string(),
                    )]),
            );
            std::thread::sleep(std::time::Duration::from_secs(15));
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    start_discord_rpc();
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            check_file_exists,
            file_exists,
            locate_version,
            get_directory_size,
            delete_file,
            is_fn_running,
            game::launch_game,
            game::close_game,
            game::verify_build_files,
            game::remove_paks,
            updater::download_and_install_update,
            settings::set_always_on_top,
            download::download_build,
            download::cancel_download,
            download::install_build_files,
            download::cancel_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
