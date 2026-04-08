use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use walkdir::WalkDir;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;

pub const TRANSFER_PORT: u16 = 53318;

// Gönderme/alma buffer boyutu — 8MB: LAN'da throughput'u maksimize eder
const BUF_SIZE: usize = 8 * 1024 * 1024;

// Progress event'lerini en fazla bu sıklıkta gönder (ms)
// Çok sık emit frontend'i yavaşlatır, çok seyrek kullanıcı deneyimini bozar
const PROGRESS_THROTTLE_MS: u64 = 80;

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
    pub static ref PENDING_TRANSFERS: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    pub static ref CANCEL_TOKENS: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> =
        Arc::new(Mutex::new(HashMap::new()));
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
    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                let app_c = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_incoming(socket, app_c).await {
                        println!("TCP Hata: {:?}", e);
                    }
                });
            }
        }
    });
    Ok(())
}

// ─── GELEN TRANSFER ────────────────────────────────────────────────────────
async fn handle_incoming(mut socket: TcpStream, app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let _ = socket.set_nodelay(true);
    let save_dir = dirs::download_dir().unwrap_or_else(|| std::env::current_dir().unwrap());

    let mut active_cancel: Option<Arc<tokio::sync::Notify>> = None;

    // Batch durum takibi
    let mut batch_id       = String::new();
    let mut batch_total    = 0u64;
    let mut batch_files    = 0u32;
    let mut batch_dl       = 0u64;
    let mut batch_label    = String::new();
    let mut batch_last_pct = 0u32;
    let mut last_emit      = Instant::now();
    // Son indirilen dosyanın yolu (tek dosyalı transferlerde "Dosyayı Aç" için)
    let mut last_saved_path: Option<PathBuf> = None;

    loop {
        // 4 byte uzunluk başlığı oku
        let mut len_buf = [0u8; 4];
        let n = socket.read(&mut len_buf).await?;
        if n == 0 { break; }
        if n < 4 { socket.read_exact(&mut len_buf[n..]).await?; }

        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        socket.read_exact(&mut payload).await?;

        let msg: TransferProtocol = serde_json::from_slice(&payload)?;

        match msg {
            // ── Transfer isteği geldi ────────────────────────────────────────
            TransferProtocol::TransferRequest { total_size, total_files, id } => {
                let cancel = register_cancel_token(&id);
                active_cancel = Some(cancel);
                batch_id       = id.clone();
                batch_total    = total_size;
                batch_files    = total_files;
                batch_label    = format!("{} adet içerik", total_files);
                batch_dl       = 0;
                batch_last_pct = 0;
                last_emit      = Instant::now();
                last_saved_path = None;

                let (tx, rx) = tokio::sync::oneshot::channel();
                PENDING_TRANSFERS.lock().await.insert(id.clone(), tx);

                // Pencereyi öne getir
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }

                let _ = app.notification()
                    .builder()
                    .title("VeriShare — Gelen İstek")
                    .body(format!(
                        "{} dosya ({}) göndermek istiyor. Kabul ediyor musunuz?",
                        total_files, format_size(total_size)
                    ))
                    .show();

                let _ = app.emit("transfer-request", serde_json::json!({
                    "id":          id.as_str(),
                    "total_size":  total_size,
                    "total_files": total_files
                }));

                let accepted = rx.await.unwrap_or(false);
                let resp = if accepted { TransferProtocol::TransferAccepted } else { TransferProtocol::TransferDeclined };
                send_msg(&mut socket, &resp).await?;

                if !accepted {
                    if let Some(ref _c) = active_cancel { remove_cancel_token(&id); }
                    return Ok(());
                }

                let _ = app.emit("transfer-initiated", serde_json::json!({
                    "transfer_id": batch_id.as_str(),
                    "text":        batch_label.as_str(),
                    "dir":         "in"
                }));
            }

            TransferProtocol::TransferAccepted | TransferProtocol::TransferDeclined => {}

            // ── Dosya başlığı — içeriği oku ve diske yaz ─────────────────────
            TransferProtocol::FileHeader { rel_path, file_size } => {
                // Güvenli yol: mutlak ya da traversal içeren parçaları at
                let mut save_path = save_dir.clone();
                for part in rel_path.split('/') {
                    let part = part.trim();
                    if part.is_empty() || part == ".." || part == "." { continue; }
                    save_path.push(part);
                }

                if let Some(parent) = save_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                let mut file = tokio::fs::File::create(&save_path).await?;
                let mut buf  = vec![0u8; BUF_SIZE];
                let mut remaining = file_size;
                let mut cancelled = false;

                while remaining > 0 {
                    let to_read = std::cmp::min(remaining, buf.len() as u64) as usize;

                    match &active_cancel {
                        Some(cancel) => {
                            tokio::select! {
                                result = socket.read_exact(&mut buf[..to_read]) => { result?; }
                                _ = cancel.notified() => { cancelled = true; break; }
                            }
                        }
                        None => { socket.read_exact(&mut buf[..to_read]).await?; }
                    }

                    if cancelled { break; }

                    file.write_all(&buf[..to_read]).await?;
                    remaining  -= to_read as u64;
                    batch_dl   += to_read as u64;
                    last_saved_path = Some(save_path.clone());

                    let pct = pct_of(batch_dl, batch_total);
                    let now = Instant::now();
                    let should_emit = pct > batch_last_pct
                        || pct == 100
                        || now.duration_since(last_emit) >= Duration::from_millis(PROGRESS_THROTTLE_MS);

                    if should_emit {
                        batch_last_pct = pct;
                        last_emit = now;
                        let is_done = pct == 100;
                        let path_val = if is_done {
                            // Tek dosya → dosyanın kendisi; çok dosya → indirme klasörü
                            let p = if batch_files == 1 {
                                save_path.to_string_lossy().into_owned()
                            } else {
                                save_dir.to_string_lossy().into_owned()
                            };
                            serde_json::Value::String(p)
                        } else {
                            serde_json::Value::Null
                        };

                        let _ = app.emit("transfer-progress", serde_json::json!({
                            "id":      batch_id.as_str(),
                            "pct":     pct,
                            "text":    batch_label.as_str(),
                            "is_done": is_done,
                            "path":    path_val
                        }));
                    }
                }

                if cancelled {
                    let _ = app.emit("transfer-progress", serde_json::json!({
                        "id":        batch_id.as_str(),
                        "pct":       batch_last_pct,
                        "text":      batch_label.as_str(),
                        "is_done":   false,
                        "cancelled": true
                    }));
                    let _ = tokio::fs::remove_file(&save_path).await;
                    return Ok(());
                }
            }

            // ── Tüm dosyalar bitti ───────────────────────────────────────────
            TransferProtocol::AllDone => {
                if let Some(ref _c) = active_cancel {
                    remove_cancel_token(&batch_id);
                }

                // Eğer is_done=100 eventi henüz gönderilmediyse (küçük dosyalar) burada gönder
                if batch_last_pct < 100 {
                    let path_str = if batch_files == 1 {
                        last_saved_path.as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    } else {
                        save_dir.to_string_lossy().into_owned()
                    };
                    let _ = app.emit("transfer-progress", serde_json::json!({
                        "id":      batch_id.as_str(),
                        "pct":     100,
                        "text":    batch_label.as_str(),
                        "is_done": true,
                        "path":    path_str
                    }));
                }

                let _ = app.notification()
                    .builder()
                    .title("VeriShare")
                    .body(format!("{} indirildi!", batch_label.as_str()))
                    .show();

                break;
            }
        }
    }

    Ok(())
}

// ─── GİDEN TRANSFER ────────────────────────────────────────────────────────
pub async fn send_items(peer_ip: &str, paths: Vec<PathBuf>, app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(format!("{}:{}", peer_ip, TRANSFER_PORT)).await?;
    let _ = socket.set_nodelay(true);

    // Tüm dosyaları tara ve toplam boyutu hesapla
    let mut all_files: Vec<(String, PathBuf)> = Vec::new();
    let mut total_size = 0u64;

    for p in paths {
        if p.is_file() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Ok(m) = tokio::fs::metadata(&p).await { total_size += m.len(); }
            all_files.push((name, p));
        } else if p.is_dir() {
            let base = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            for entry in WalkDir::new(&p).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&p) {
                        let mut full = PathBuf::from(&base);
                        full.push(rel);
                        let rel_str = full.to_string_lossy().replace('\\', "/");
                        if let Ok(m) = entry.metadata() { total_size += m.len(); }
                        all_files.push((rel_str, entry.path().to_path_buf()));
                    }
                }
            }
        }
    }

    let transfer_id = uuid::Uuid::new_v4().to_string();
    let cancel = register_cancel_token(&transfer_id);

    // display_name tüm fonksiyon boyunca referansla kullanılacak — move etme
    let display_name: String = if all_files.len() == 1 {
        all_files.first().map(|x| x.0.clone()).unwrap_or_else(|| "Bilinmeyen".into())
    } else {
        format!("{} dosya/klasör", all_files.len())
    };

    let _ = app.emit("transfer-initiated", serde_json::json!({
        "transfer_id": transfer_id.as_str(),
        "text":        display_name.as_str(),
        "dir":         "out"
    }));

    // İstek gönder
    send_msg(&mut socket, &TransferProtocol::TransferRequest {
        total_size,
        total_files: all_files.len() as u32,
        id: transfer_id.clone(),
    }).await?;

    // Yanıt bekle (iptal edilebilir)
    let mut len_buf = [0u8; 4];
    tokio::select! {
        result = socket.read_exact(&mut len_buf) => { result?; }
        _ = cancel.notified() => {
            remove_cancel_token(&transfer_id);
            return Err("İPTAL_EDİLDİ".into());
        }
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    socket.read_exact(&mut payload).await?;

    match serde_json::from_slice::<TransferProtocol>(&payload)? {
        TransferProtocol::TransferAccepted => {}
        TransferProtocol::TransferDeclined => {
            remove_cancel_token(&transfer_id);
            return Err("ERİŞİM_REDDEDİLDİ".into());
        }
        _ => {
            remove_cancel_token(&transfer_id);
            return Err("Bilinmeyen yanıt.".into());
        }
    }

    let _ = app.emit("transfer-id-assigned", serde_json::json!({ "transfer_id": transfer_id.as_str() }));

    // Dosyaları gönder
    let mut uploaded  = 0u64;
    let mut last_pct  = 0u32;
    let mut last_emit = Instant::now();
    let mut buf       = vec![0u8; BUF_SIZE];

    for (rel_path, abs_path) in &all_files {
        let size = tokio::fs::metadata(abs_path).await?.len();

        send_msg(&mut socket, &TransferProtocol::FileHeader {
            rel_path: rel_path.clone(),
            file_size: size,
        }).await?;

        let mut file = tokio::fs::File::open(abs_path).await?;

        loop {
            let n;
            tokio::select! {
                result = file.read(&mut buf) => { n = result?; }
                _ = cancel.notified() => {
                    remove_cancel_token(&transfer_id);
                    let _ = app.emit("transfer-out-progress", serde_json::json!({
                        "id":        transfer_id.as_str(),
                        "pct":       last_pct,
                        "text":      display_name.as_str(),
                        "is_done":   false,
                        "cancelled": true
                    }));
                    return Err("İPTAL_EDİLDİ".into());
                }
            }
            if n == 0 { break; }

            socket.write_all(&buf[..n]).await?;
            uploaded += n as u64;

            let pct = pct_of(uploaded, total_size);
            let now = Instant::now();
            let should_emit = pct > last_pct
                || pct == 100
                || now.duration_since(last_emit) >= Duration::from_millis(PROGRESS_THROTTLE_MS);

            if should_emit {
                last_pct  = pct;
                last_emit = now;
                let _ = app.emit("transfer-out-progress", serde_json::json!({
                    "id":      transfer_id.as_str(),
                    "pct":     pct,
                    "text":    display_name.as_str(),
                    "is_done": pct == 100
                }));
            }
        }
    }

    // Bitiş sinyali
    send_msg(&mut socket, &TransferProtocol::AllDone).await?;
    remove_cancel_token(&transfer_id);
    Ok(())
}

// ─── YARDIMCI FONKSİYONLAR ─────────────────────────────────────────────────

/// Framing: 4 byte big-endian uzunluk + JSON payload
async fn send_msg(socket: &mut TcpStream, msg: &TransferProtocol) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(msg)?;
    socket.write_all(&(json.len() as u32).to_be_bytes()).await?;
    socket.write_all(&json).await?;
    Ok(())
}

#[inline]
fn pct_of(done: u64, total: u64) -> u32 {
    if total == 0 { return 100; }
    ((done as f64 / total as f64) * 100.0) as u32
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 { return format!("{} B", bytes); }
    if bytes < 1024 * 1024 { return format!("{:.1} KB", bytes as f64 / 1024.0); }
    if bytes < 1024 * 1024 * 1024 { return format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)); }
    format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
