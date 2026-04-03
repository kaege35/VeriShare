use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use walkdir::WalkDir;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;

pub const TRANSFER_PORT: u16 = 53318;

#[derive(Serialize, Deserialize, Debug)]
pub enum TransferProtocol {
    TransferRequest {
        total_size: u64,
        total_files: u32,
        id: String,
    },
    TransferAccepted,
    TransferDeclined,
    FileHeader {
        rel_path: String,
        file_size: u64,
    },
    AllDone,
}

lazy_static::lazy_static! {
    pub static ref PENDING_TRANSFERS: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> = Arc::new(Mutex::new(HashMap::new()));
    pub static ref CANCEL_TOKENS: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub async fn cancel_transfer_by_id(id: String) -> Result<(), String> {
    if let Some(tx) = PENDING_TRANSFERS.lock().await.remove(&id) {
        let _ = tx.send(false);
    }
    if let Some(notify) = CANCEL_TOKENS.lock().await.remove(&id) {
        notify.notify_one();
    }
    Ok(())
}

fn register_cancel_token(id: &str) -> Arc<tokio::sync::Notify> {
    let notify = Arc::new(tokio::sync::Notify::new());
    let rt = tokio::runtime::Handle::current();
    let id_owned = id.to_string();
    let notify_clone = notify.clone();
    rt.spawn(async move {
        CANCEL_TOKENS.lock().await.insert(id_owned, notify_clone);
    });
    notify
}

fn remove_cancel_token(id: &str) {
    let rt = tokio::runtime::Handle::current();
    let id_owned = id.to_string();
    rt.spawn(async move {
        CANCEL_TOKENS.lock().await.remove(&id_owned);
    });
}

pub async fn start_transfer_server(app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], TRANSFER_PORT))).await?;
    let app_clone = app.clone();
    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                let app_c = app_clone.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(socket, app_c).await {
                        println!("TCP Hata: {:?}", e);
                    }
                });
            }
        }
    });
    Ok(())
}

async fn handle_connection(mut socket: TcpStream, app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let _ = socket.set_nodelay(true);
    let save_dir = dirs::download_dir().unwrap_or_else(|| std::env::current_dir().unwrap());
    let mut active_cancel: Option<Arc<tokio::sync::Notify>> = None;
    
    let mut batch_transfer_id = String::new();
    let mut batch_total_size = 0u64;
    let mut batch_downloaded = 0u64;
    let mut batch_display_text = String::new();
    let mut batch_last_pct = 0;

    loop {
        let mut len_buf = [0u8; 4];
        let n = socket.read(&mut len_buf).await?;
        if n == 0 { break; }
        if n < 4 { socket.read_exact(&mut len_buf[n..]).await?; }
        
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        socket.read_exact(&mut payload).await?;

        let req: TransferProtocol = serde_json::from_slice(&payload)?;
        
        match req {
            TransferProtocol::TransferRequest { total_size, total_files, id } => {
                let cancel = register_cancel_token(&id);
                active_cancel = Some(cancel);
                batch_transfer_id = id.clone();
                batch_total_size = total_size;
                batch_display_text = format!("{} adet içerik", total_files);

                let (tx, rx) = tokio::sync::oneshot::channel();
                PENDING_TRANSFERS.lock().await.insert(id.clone(), tx);
                
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }

                let _ = app.notification()
                    .builder()
                    .title("VeriShare — Gelen İstek")
                    .body(format!("{} dosya ({}) göndermek istiyor. Kabul ediyor musunuz?", total_files, format_size_rust(total_size)))
                    .show();

                let _ = app.emit("transfer-request", serde_json::json!({
                    "id": id,
                    "total_size": total_size,
                    "total_files": total_files
                }));

                let accepted = rx.await.unwrap_or(false);
                let resp = if accepted { TransferProtocol::TransferAccepted } else { TransferProtocol::TransferDeclined };
                let req_json = serde_json::to_vec(&resp)?;
                socket.write_all(&(req_json.len() as u32).to_be_bytes()).await?;
                socket.write_all(&req_json).await?;
                
                if !accepted {
                    if let Some(ref _c) = active_cancel { remove_cancel_token(&id); }
                    return Ok(());
                } else {
                    let _ = app.emit("transfer-initiated", serde_json::json!({
                        "transfer_id": batch_transfer_id.clone(),
                        "text": batch_display_text.clone(),
                        "dir": "in"
                    }));
                }
            },
            TransferProtocol::TransferAccepted => {},
            TransferProtocol::TransferDeclined => {},
            TransferProtocol::FileHeader { rel_path, file_size } => {
                let mut save_path = save_dir.clone();
                for component in rel_path.split('/') {
                    save_path.push(component);
                }

                if let Some(p) = save_path.parent() {
                    tokio::fs::create_dir_all(p).await?;
                }

                let mut file = tokio::fs::File::create(&save_path).await?;
                let mut buffer = vec![0u8; 4 * 1024 * 1024];
                let mut remaining = file_size;
                let mut cancelled = false;
                
                while remaining > 0 {
                    let to_read = std::cmp::min(remaining, buffer.len() as u64) as usize;
                    
                    if let Some(ref cancel) = active_cancel {
                        tokio::select! {
                            result = socket.read_exact(&mut buffer[..to_read]) => {
                                result?;
                            }
                            _ = cancel.notified() => {
                                cancelled = true;
                                break;
                            }
                        }
                    } else {
                        socket.read_exact(&mut buffer[..to_read]).await?;
                    }
                    
                    if cancelled { break; }
                    
                    file.write_all(&buffer[..to_read]).await?;
                    remaining -= to_read as u64;
                    batch_downloaded += to_read as u64;

                    let pct = if batch_total_size == 0 { 100 } else { ((batch_downloaded as f64 / batch_total_size as f64) * 100.0) as u32 };
                    if pct > batch_last_pct || pct == 100 {
                        batch_last_pct = pct;
                        let _ = app.emit("transfer-progress", serde_json::json!({
                            "id": batch_transfer_id.clone(),
                            "pct": pct,
                            "text": batch_display_text.clone(),
                            "is_done": pct == 100,
                            "path": save_path.to_string_lossy().into_owned()
                        }));
                    }
                }

                if cancelled {
                    let _ = app.emit("transfer-progress", serde_json::json!({
                        "id": batch_transfer_id.clone(),
                        "pct": batch_last_pct,
                        "text": batch_display_text.clone(),
                        "is_done": false,
                        "cancelled": true
                    }));
                    let _ = tokio::fs::remove_file(&save_path).await;
                    return Ok(());
                }

                if batch_last_pct == 100 {
                    let _ = app.notification()
                        .builder()
                        .title("VeriShare")
                        .body("İndirme tamamlandı!")
                        .show();
                }
            },
            TransferProtocol::AllDone => {
                if let Some(ref _c) = active_cancel {
                    remove_cancel_token(&batch_transfer_id);
                }
                break;
            }
        }
    }
    Ok(())
}

pub async fn send_items(peer_ip: &str, paths: Vec<PathBuf>, app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(format!("{}:{}", peer_ip, TRANSFER_PORT)).await?;
    let _ = socket.set_nodelay(true);

    let mut all_files = Vec::new();
    let mut total_size = 0u64;
    for p in paths {
        if p.is_file() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Ok(m) = tokio::fs::metadata(&p).await { total_size += m.len(); }
            all_files.push((name, p));
        } else if p.is_dir() {
            let base_name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            for entry in WalkDir::new(&p).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&p) {
                        let mut full_path = PathBuf::from(&base_name);
                        full_path.push(rel);
                        let rel_str = full_path.to_string_lossy().to_string().replace("\\", "/");
                        if let Ok(m) = entry.metadata() { total_size += m.len(); }
                        all_files.push((rel_str, entry.path().to_path_buf()));
                    }
                }
            }
        }
    }

    let transfer_id = uuid::Uuid::new_v4().to_string();
    let cancel = register_cancel_token(&transfer_id);

    let display_name = if all_files.len() == 1 {
        all_files.first().map(|x| x.0.clone()).unwrap_or_else(|| "Bilinmeyen".to_string())
    } else {
        format!("{} dosya/klasör", all_files.len())
    };

    let _ = app.emit("transfer-initiated", serde_json::json!({
        "transfer_id": transfer_id.clone(),
        "text": display_name.clone(),
        "dir": "out"
    }));

    let req = TransferProtocol::TransferRequest {
        total_size,
        total_files: all_files.len() as u32,
        id: transfer_id.clone(),
    };
    let req_json = serde_json::to_vec(&req)?;
    socket.write_all(&(req_json.len() as u32).to_be_bytes()).await?;
    socket.write_all(&req_json).await?;
    
    let mut len_buf = [0u8; 4];
    tokio::select! {
        result = socket.read_exact(&mut len_buf) => {
            result?;
        }
        _ = cancel.notified() => {
            remove_cancel_token(&transfer_id);
            return Err("İPTAL_EDİLDİ".into());
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    socket.read_exact(&mut payload).await?;
    let resp: TransferProtocol = serde_json::from_slice(&payload)?;
    
    match resp {
        TransferProtocol::TransferAccepted => {},
        TransferProtocol::TransferDeclined => {
            remove_cancel_token(&transfer_id);
            return Err("ERİŞİM_REDDEDİLDİ".into());
        },
        _ => {
            remove_cancel_token(&transfer_id);
            return Err("Bilinmeyen yanıt.".into());
        },
    }

    let _ = app.emit("transfer-id-assigned", serde_json::json!({
        "transfer_id": transfer_id.clone()
    }));

    let mut uploaded_total = 0u64;
    let mut last_pct = 0;

    for (rel_path, abs_path) in all_files {
        let size = tokio::fs::metadata(&abs_path).await?.len();
        
        let req = TransferProtocol::FileHeader { rel_path: rel_path.clone(), file_size: size };
        let req_json = serde_json::to_vec(&req)?;
        socket.write_all(&(req_json.len() as u32).to_be_bytes()).await?;
        socket.write_all(&req_json).await?;
        
        let mut file = tokio::fs::File::open(&abs_path).await?;
        let mut buffer = vec![0u8; 4 * 1024 * 1024];

        loop {
            let n;
            tokio::select! {
                result = file.read(&mut buffer) => {
                    n = result?;
                }
                _ = cancel.notified() => {
                    remove_cancel_token(&transfer_id);
                    let _ = app.emit("transfer-out-progress", serde_json::json!({
                        "id": transfer_id.clone(),
                        "pct": last_pct,
                        "text": display_name.clone(),
                        "is_done": false,
                        "cancelled": true
                    }));
                    return Err("İPTAL_EDİLDİ".into());
                }
            }
            if n == 0 { break; }
            socket.write_all(&buffer[..n]).await?;
            uploaded_total += n as u64;

            let pct = if total_size == 0 { 100 } else { ((uploaded_total as f64 / total_size as f64) * 100.0) as u32 };
            if pct > last_pct || pct == 100 {
                last_pct = pct;
                let _ = app.emit("transfer-out-progress", serde_json::json!({
                    "id": transfer_id.clone(),
                    "pct": pct,
                    "text": display_name.clone(),
                    "is_done": pct == 100
                }));
            }
        }
    }

    let end_json = serde_json::to_vec(&TransferProtocol::AllDone)?;
    socket.write_all(&(end_json.len() as u32).to_be_bytes()).await?;
    socket.write_all(&end_json).await?;

    remove_cancel_token(&transfer_id);
    Ok(())
}

fn format_size_rust(bytes: u64) -> String {
    if bytes < 1024 { return format!("{} B", bytes); }
    if bytes < 1024 * 1024 { return format!("{:.1} KB", bytes as f64 / 1024.0); }
    if bytes < 1024 * 1024 * 1024 { return format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)); }
    format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
