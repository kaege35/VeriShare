use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use walkdir::WalkDir;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;

use crate::crypto::{self, SecureStream};
use crate::settings;

pub const TRANSFER_PORT: u16 = 53318;

// Progress event'lerini en fazla bu sıklıkta gönder (ms)
// Çok sık emit frontend'i yavaşlatır, çok seyrek kullanıcı deneyimini bozar
const PROGRESS_THROTTLE_MS: u64 = 80;

// Gelen transfer isteğine kullanıcı yanıt vermezse bu süre sonunda otomatik
// reddediliyor — eskiden süresiz beklendiği için gönderen taraf sonsuza kadar
// asılı kalabiliyordu.
const REQUEST_TIMEOUT_SECS: u64 = 60;

#[derive(Serialize, Deserialize, Debug)]
pub enum TransferProtocol {
    TransferRequest {
        total_size: u64,
        total_files: u32,
        id: String,
    },
    TransferAccepted,
    TransferDeclined,
    /// Her dosyadan önce gönderilir — alıcıya "bu dosyadan zaten bir parçan
    /// var mı?" diye sorar (kaldığı yerden devam / isim çakışması kontrolü).
    FileOffsetRequest {
        rel_path: String,
        file_size: u64,
    },
    /// offset=0: dosya sıfırdan gönderilecek (yeni ya da yeniden adlandırıldı).
    /// offset>0: alıcıda bu kadar byte zaten var, gönderen oradan devam eder.
    FileOffsetResponse {
        offset: u64,
    },
    /// Dosyanın TAMAMININ (resume ile atlanan kısım dahil) SHA-256 özeti —
    /// bütünlük doğrulaması için.
    FileChecksum {
        checksum: String,
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

// NOT: Bu fonksiyonlar eskiden `tokio::runtime::Handle::current().spawn(...)`
// ile map güncellemesini arka planda (await edilmeden) yapıyordu. Bu bir yarış
// durumuna yol açıyordu: `register_cancel_token` çağrıldıktan hemen sonra
// kullanıcı transferi iptal ederse, `cancel_transfer_by_id` içindeki
// `CANCEL_TOKENS.lock().await.remove(&id)` henüz eklenmemiş bir anahtarı
// arayıp bulamayabiliyor ve iptal sessizce kayboluyordu. Artık doğrudan
// `.await` ile senkron sırayla yazılıyor — çağıran taraf zaten async fn
// içinde olduğu için ek bir maliyeti yok.
async fn register_cancel_token(id: &str) -> Arc<tokio::sync::Notify> {
    let notify = Arc::new(tokio::sync::Notify::new());
    CANCEL_TOKENS.lock().await.insert(id.to_string(), notify.clone());
    notify
}

async fn remove_cancel_token(id: &str) {
    CANCEL_TOKENS.lock().await.remove(id);
}

pub async fn start_transfer_server(app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], TRANSFER_PORT))).await?;
    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                let app_c = app.clone();
                tokio::spawn(async move {
                    // Hata durumunda son bilinen transfer id'sini frontend'e
                    // "başarısız/kesildi" olarak bildirebilmek için paylaşımlı
                    // bir tutucu kullanıyoruz — eskiden hatalar sadece konsola
                    // basılıyordu ve arayüzdeki log satırı sonsuza dek
                    // "%xx" durumunda asılı kalıyordu.
                    let last_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

                    // NOT: `Box<dyn std::error::Error>` (`e`) Send DEĞİL.
                    // Önceki denemede `e.to_string()`'i bir `if let Err(e) =`
                    // bloğunun İÇİNDE, sonrasında başka bir `.await` olacak
                    // şekilde çağırmak yetmedi — async state machine, `e`
                    // bağlamının (drop sırası nedeniyle) blok sonuna kadar
                    // "canlı" sayıldığını düşünüyor, NLL bunu kısaltmıyor.
                    // Kesin çözüm: `e`'yi yalnızca `match` kolunun İÇİNDE,
                    // hiçbir `.await` içermeyen dar bir ifadede kullanıp
                    // hemen `String`'e indirgemek — böylece `e`'nin
                    // ömrü match kolunun kendisiyle birlikte biter ve
                    // sonraki `.await`'lere hiç sızmaz.
                    let err_msg: Option<String> =
                        match handle_incoming(socket, app_c.clone(), last_id.clone()).await {
                            Ok(()) => None,
                            Err(e) => Some(e.to_string()),
                        };

                    if let Some(err_msg) = err_msg {
                        println!("TCP Hata: {}", err_msg);
                        let last_id_val = last_id.lock().await.clone();
                        if let Some(id) = last_id_val {
                            let _ = app_c.emit("transfer-progress", serde_json::json!({
                                "id": id,
                                "pct": 0,
                                "text": "",
                                "is_done": false,
                                "error": err_msg
                            }));
                        }
                    }
                });
            }
        }
    });
    Ok(())
}

/// Var olan hedef dosya için kaldığı yerden devam etme kararı.
/// - Hedefte hiç dosya yoksa: sıfırdan, aynı isim.
/// - Hedefte istenenden KÜÇÜK bir dosya varsa: yarım kalmış transfer kabul
///   edilir, o boyuttan devam edilir (bütünlük son checksum ile doğrulanır).
/// - Hedefte istenen boyuta eşit/daha büyük bir dosya varsa: muhtemelen
///   farklı bir dosya — üzerine yazmak yerine "(1)", "(2)" ekiyle yeni isim
///   üretilir, hiçbir zaman sessizce üzerine yazılmaz.
async fn decide_resume(save_path: &Path, incoming_size: u64) -> (PathBuf, u64) {
    match tokio::fs::metadata(save_path).await {
        Ok(meta) if meta.len() < incoming_size => (save_path.to_path_buf(), meta.len()),
        Ok(_) => (unique_path(save_path).await, 0),
        Err(_) => (save_path.to_path_buf(), 0),
    }
}

async fn unique_path(path: &Path) -> PathBuf {
    if tokio::fs::metadata(path).await.is_err() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
    let parent = path.parent().unwrap_or_else(|| Path::new(""));

    for i in 1..10_000u32 {
        let candidate_name = match &ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        let candidate = parent.join(candidate_name);
        if tokio::fs::metadata(&candidate).await.is_err() {
            return candidate;
        }
    }
    // Pratikte imkansız ama sonsuz döngüye girmeyelim — orijinal isme geri dön.
    path.to_path_buf()
}

/// Bir dosyanın ilk `len` byte'ını okuyup SHA-256 hazırlar — resume
/// durumunda hem gönderen (kaynak dosyanın atlanan kısmı) hem alıcı (diskte
/// zaten var olan kısım) checksum'ı dosyanın TAMAMI üzerinden tutarlı
/// hesaplayabilsin diye.
async fn hash_prefix(path: &Path, len: u64) -> std::io::Result<Sha256> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut remaining = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let to_read = std::cmp::min(remaining, buf.len() as u64) as usize;
        let n = f.read(&mut buf[..to_read]).await?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(hasher)
}

fn hex_digest(hasher: Sha256) -> String {
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

// ─── GELEN TRANSFER ────────────────────────────────────────────────────────
async fn handle_incoming(
    socket: TcpStream,
    app: AppHandle,
    last_id: Arc<Mutex<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = socket.set_nodelay(true);
    let mut secure = SecureStream::handshake_responder(socket).await?;
    let save_dir = settings::current_download_dir().await;

    let mut active_cancel: Option<Arc<tokio::sync::Notify>> = None;

    // Batch durum takibi
    let mut batch_id       = String::new();
    let mut batch_total    = 0u64;
    let mut batch_files    = 0u32;
    let mut batch_dl       = 0u64;
    let mut batch_label    = String::new();
    let mut batch_last_pct = 0u32;
    let mut last_emit      = Instant::now();
    let mut last_emit_bytes = 0u64;
    // Son indirilen dosyanın yolu (tek dosyalı transferlerde "Dosyayı Aç" için)
    let mut last_saved_path: Option<PathBuf> = None;
    let mut had_integrity_warning = false;

    loop {
        let frame = match secure.read_frame().await? {
            Some(f) => f,
            None => break, // bağlantı düzgün kapandı
        };
        let msg: TransferProtocol = serde_json::from_slice(&frame)?;

        match msg {
            // ── Transfer isteği geldi ────────────────────────────────────────
            TransferProtocol::TransferRequest { total_size, total_files, id } => {
                let cancel = register_cancel_token(&id).await;
                active_cancel = Some(cancel);
                batch_id       = id.clone();
                *last_id.lock().await = Some(id.clone());
                batch_total    = total_size;
                batch_files    = total_files;
                batch_label    = format!("{} adet içerik", total_files);
                batch_dl       = 0;
                batch_last_pct = 0;
                last_emit      = Instant::now();
                last_emit_bytes = 0;
                last_saved_path = None;
                had_integrity_warning = false;

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

                // Kullanıcı REQUEST_TIMEOUT_SECS içinde yanıt vermezse otomatik
                // reddet — eskiden burası süresiz beklerdi.
                let accepted = match tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), rx).await {
                    Ok(Ok(v)) => v,
                    _ => {
                        PENDING_TRANSFERS.lock().await.remove(&id);
                        let _ = app.emit("transfer-request-expired", serde_json::json!({ "id": id.as_str() }));
                        false
                    }
                };

                let resp = if accepted { TransferProtocol::TransferAccepted } else { TransferProtocol::TransferDeclined };
                send_msg(&mut secure, &resp).await?;

                if !accepted {
                    remove_cancel_token(&id).await;
                    return Ok(());
                }

                let _ = app.emit("transfer-initiated", serde_json::json!({
                    "transfer_id": batch_id.as_str(),
                    "text":        batch_label.as_str(),
                    "dir":         "in"
                }));
            }

            TransferProtocol::TransferAccepted | TransferProtocol::TransferDeclined => {}

            // ── Dosya için offset pazarlığı — resume / üzerine yazmama ───────
            TransferProtocol::FileOffsetRequest { rel_path, file_size } => {
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

                let (final_path, offset) = decide_resume(&save_path, file_size).await;

                let mut hasher = Sha256::new();
                if offset > 0 {
                    hasher = hash_prefix(&final_path, offset).await.unwrap_or_else(|_| Sha256::new());
                    batch_dl += offset;
                }

                send_msg(&mut secure, &TransferProtocol::FileOffsetResponse { offset }).await?;

                let mut file = if offset > 0 {
                    let mut f = tokio::fs::OpenOptions::new().write(true).open(&final_path).await?;
                    f.seek(std::io::SeekFrom::Start(offset)).await?;
                    f
                } else {
                    tokio::fs::File::create(&final_path).await?
                };

                let mut remaining = file_size.saturating_sub(offset);
                let mut cancelled = false;

                while remaining > 0 {
                    let chunk = match &active_cancel {
                        Some(cancel) => {
                            tokio::select! {
                                result = secure.read_frame() => result?,
                                _ = cancel.notified() => { cancelled = true; None }
                            }
                        }
                        None => secure.read_frame().await?,
                    };

                    if cancelled { break; }
                    let Some(chunk) = chunk else { break; }; // bağlantı beklenmedik kapandı

                    file.write_all(&chunk).await?;
                    hasher.update(&chunk);
                    let n = chunk.len() as u64;
                    remaining  = remaining.saturating_sub(n);
                    batch_dl   += n;
                    last_saved_path = Some(final_path.clone());

                    let pct = pct_of(batch_dl, batch_total);
                    let now = Instant::now();
                    let should_emit = pct > batch_last_pct
                        || pct == 100
                        || now.duration_since(last_emit) >= Duration::from_millis(PROGRESS_THROTTLE_MS);

                    if should_emit {
                        let speed = speed_bps(batch_dl.saturating_sub(last_emit_bytes), now.duration_since(last_emit));
                        batch_last_pct = pct;
                        last_emit = now;
                        last_emit_bytes = batch_dl;
                        let is_done = pct == 100;
                        let path_val = if is_done {
                            // Tek dosya → dosyanın kendisi; çok dosya → indirme klasörü
                            let p = if batch_files == 1 {
                                final_path.to_string_lossy().into_owned()
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
                            "path":    path_val,
                            "speed":   format_speed(speed)
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
                    // NOT: kısmi dosya BİLEREK silinmiyor — aynı içerik tekrar
                    // gönderilirse kaldığı yerden devam edebilsin diye. Eskiden
                    // burada dosya siliniyordu, resume özelliğiyle çelişiyordu.
                    return Ok(());
                }

                // Dosya tamamlandı — bütünlük doğrulaması için checksum bekle.
                if let Some(cf) = secure.read_frame().await? {
                    if let Ok(TransferProtocol::FileChecksum { checksum }) = serde_json::from_slice(&cf) {
                        let computed = hex_digest(hasher);
                        if computed != checksum {
                            had_integrity_warning = true;
                            println!(
                                "Bütünlük uyuşmazlığı: {} (beklenen {}, hesaplanan {})",
                                rel_path, checksum, computed
                            );
                            let _ = app.emit("transfer-integrity-warning", serde_json::json!({
                                "id": batch_id.as_str(),
                                "file": rel_path
                            }));
                        }
                    }
                }
            }

            TransferProtocol::FileChecksum { .. } => {
                // Normal akışta FileOffsetRequest kolunun sonunda zaten
                // tüketiliyor; buraya düşerse protokol dışı bir durumdur.
            }

            TransferProtocol::FileOffsetResponse { .. } => {
                // Bu mesajı yalnızca GÖNDEREN taraf (send_items) bekler ve
                // tüketir. Alıcı tarafında (bu fonksiyon) hiçbir zaman ana
                // döngüye düşmemeli — yine de enum'u eksiksiz eşleştirmek
                // için burada yok sayıyoruz.
            }

            // ── Tüm dosyalar bitti ───────────────────────────────────────────
            TransferProtocol::AllDone => {
                if !batch_id.is_empty() {
                    remove_cancel_token(&batch_id).await;
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

                let body = if had_integrity_warning {
                    format!("{} indirildi (bazı dosyalarda bütünlük uyarısı var)", batch_label.as_str())
                } else {
                    format!("{} indirildi!", batch_label.as_str())
                };
                let _ = app.notification()
                    .builder()
                    .title("VeriShare")
                    .body(body)
                    .show();

                break;
            }
        }
    }

    Ok(())
}

// ─── GİDEN TRANSFER ────────────────────────────────────────────────────────
pub async fn send_items(peer_ip: &str, paths: Vec<PathBuf>, app: AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let stream = TcpStream::connect(format!("{}:{}", peer_ip, TRANSFER_PORT)).await?;
    let _ = stream.set_nodelay(true);
    let mut secure = SecureStream::handshake_initiator(stream).await?;

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
    let cancel = register_cancel_token(&transfer_id).await;

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
    send_msg(&mut secure, &TransferProtocol::TransferRequest {
        total_size,
        total_files: all_files.len() as u32,
        id: transfer_id.clone(),
    }).await?;

    // Yanıt bekle (iptal edilebilir)
    let resp = tokio::select! {
        result = secure.read_frame() => result?,
        _ = cancel.notified() => {
            remove_cancel_token(&transfer_id).await;
            return Err("İPTAL_EDİLDİ".into());
        }
    };
    let resp = resp.ok_or("Bağlantı beklenmedik şekilde kapandı.")?;

    match serde_json::from_slice::<TransferProtocol>(&resp)? {
        TransferProtocol::TransferAccepted => {}
        TransferProtocol::TransferDeclined => {
            remove_cancel_token(&transfer_id).await;
            return Err("ERİŞİM_REDDEDİLDİ".into());
        }
        _ => {
            remove_cancel_token(&transfer_id).await;
            return Err("Bilinmeyen yanıt.".into());
        }
    }

    let _ = app.emit("transfer-id-assigned", serde_json::json!({ "transfer_id": transfer_id.as_str() }));

    // Dosyaları gönder
    let mut uploaded  = 0u64;
    let mut last_pct  = 0u32;
    let mut last_emit = Instant::now();
    let mut last_emit_bytes = 0u64;
    let mut buf       = vec![0u8; crypto::CHUNK_SIZE];

    for (rel_path, abs_path) in &all_files {
        let size = tokio::fs::metadata(abs_path).await?.len();

        send_msg(&mut secure, &TransferProtocol::FileOffsetRequest {
            rel_path: rel_path.clone(),
            file_size: size,
        }).await?;

        let offset_resp = secure.read_frame().await?.ok_or("Bağlantı kapandı.")?;
        let offset = match serde_json::from_slice::<TransferProtocol>(&offset_resp)? {
            TransferProtocol::FileOffsetResponse { offset } => offset.min(size),
            _ => 0,
        };

        let mut hasher = Sha256::new();
        if offset > 0 {
            hasher = hash_prefix(abs_path, offset).await.unwrap_or_else(|_| Sha256::new());
            uploaded += offset;
        }

        let mut file = tokio::fs::File::open(abs_path).await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }

        loop {
            let n;
            tokio::select! {
                result = file.read(&mut buf) => { n = result?; }
                _ = cancel.notified() => {
                    remove_cancel_token(&transfer_id).await;
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

            secure.write_frame(&buf[..n]).await?;
            hasher.update(&buf[..n]);
            uploaded += n as u64;

            let pct = pct_of(uploaded, total_size);
            let now = Instant::now();
            let should_emit = pct > last_pct
                || pct == 100
                || now.duration_since(last_emit) >= Duration::from_millis(PROGRESS_THROTTLE_MS);

            if should_emit {
                let speed = speed_bps(uploaded.saturating_sub(last_emit_bytes), now.duration_since(last_emit));
                last_pct  = pct;
                last_emit = now;
                last_emit_bytes = uploaded;
                let _ = app.emit("transfer-out-progress", serde_json::json!({
                    "id":      transfer_id.as_str(),
                    "pct":     pct,
                    "text":    display_name.as_str(),
                    "is_done": pct == 100,
                    "speed":   format_speed(speed)
                }));
            }
        }

        send_msg(&mut secure, &TransferProtocol::FileChecksum { checksum: hex_digest(hasher) }).await?;
    }

    // Bitiş sinyali
    send_msg(&mut secure, &TransferProtocol::AllDone).await?;
    remove_cancel_token(&transfer_id).await;
    Ok(())
}

// ─── YARDIMCI FONKSİYONLAR ─────────────────────────────────────────────────

async fn send_msg(secure: &mut SecureStream, msg: &TransferProtocol) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(msg)?;
    secure.write_frame(&json).await?;
    Ok(())
}

#[inline]
fn pct_of(done: u64, total: u64) -> u32 {
    if total == 0 { return 100; }
    ((done as f64 / total as f64) * 100.0) as u32
}

fn speed_bps(bytes_since: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0001 { return 0.0; }
    bytes_since as f64 / secs
}

fn format_speed(bps: f64) -> String {
    if bps < 1024.0 { return format!("{:.0} B/s", bps); }
    if bps < 1024.0 * 1024.0 { return format!("{:.1} KB/s", bps / 1024.0); }
    if bps < 1024.0 * 1024.0 * 1024.0 { return format!("{:.1} MB/s", bps / (1024.0 * 1024.0)); }
    format!("{:.2} GB/s", bps / (1024.0 * 1024.0 * 1024.0))
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 { return format!("{} B", bytes); }
    if bytes < 1024 * 1024 { return format!("{:.1} KB", bytes as f64 / 1024.0); }
    if bytes < 1024 * 1024 * 1024 { return format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0)); }
    format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}
