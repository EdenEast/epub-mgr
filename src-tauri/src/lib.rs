use std::{path::PathBuf, thread};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub mod cli;
pub mod copier;
pub mod enrichment;
pub mod epub_metadata;
pub mod epub_writer;
pub mod normalize;
pub mod output_path;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EpubFoundPayload {
    scan_id: String,
    path: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanFinishedPayload {
    scan_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanErrorPayload {
    scan_id: String,
    message: String,
}

#[tauri::command]
fn scan_epubs(app: AppHandle, path: String, scan_id: String) -> Result<(), String> {
    let root = PathBuf::from(path);

    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    // Return immediately to Tauri so the UI thread remains responsive. The jwalk
    // iterator does the directory traversal in parallel worker threads, and this
    // outer thread streams matching files back to the frontend as events.
    thread::spawn(move || {
        for entry in jwalk::WalkDir::new(root).follow_links(false) {
            match entry {
                Ok(entry) => {
                    if !entry.file_type().is_file() || !normalize::is_epub(&entry.path()) {
                        continue;
                    }

                    let payload = EpubFoundPayload {
                        scan_id: scan_id.clone(),
                        path: entry.path().display().to_string(),
                    };

                    if app.emit("epub-found", payload).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = app.emit(
                        "scan-error",
                        ScanErrorPayload {
                            scan_id: scan_id.clone(),
                            message: error.to_string(),
                        },
                    );
                }
            }
        }

        let _ = app.emit("scan-finished", ScanFinishedPayload { scan_id });
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![scan_epubs])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
