// ─── KALICI AYARLAR ─────────────────────────────────────────────────────
//
// `%APPDATA%/verishare/settings.json` (macOS/Linux'ta `dirs::config_dir()`).
// İçerik: indirme klasörü + "Manuel IP ile Bağlan" ile sabitlenen cihaz
// adresleri. Yeni alanlar `#[serde(default)]` — eski sürümün yazdığı (yalnızca
// download_dir içeren) dosya sorunsuz okunur.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    #[serde(default = "default_download_dir")]
    pub download_dir: PathBuf,
    /// Kullanıcının elle eklediği cihaz IP'leri — keşif her tick'te bunlara
    /// doğrudan unicast gönderir, uygulama yeniden açılınca da hatırlanır.
    #[serde(default)]
    pub pinned_ips: Vec<String>,
}

lazy_static::lazy_static! {
    pub static ref DOWNLOAD_DIR: Arc<Mutex<PathBuf>> = Arc::new(Mutex::new(default_download_dir()));
    static ref PINNED_IPS: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
}

fn default_download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn settings_path() -> Option<PathBuf> {
    let mut dir = dirs::config_dir()?;
    dir.push("verishare");
    Some(dir.join("settings.json"))
}

/// Diskteki ayarları belleğe yükler (idempotent — birden çok kez çağrılabilir).
pub async fn load() {
    if let Some(path) = settings_path() {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if let Ok(settings) = serde_json::from_slice::<AppSettings>(&bytes) {
                if settings.download_dir.exists() {
                    *DOWNLOAD_DIR.lock().await = settings.download_dir;
                } else {
                    println!(
                        "Kayıtlı indirme klasörü artık mevcut değil, varsayılana dönülüyor: {:?}",
                        settings.download_dir
                    );
                }
                *PINNED_IPS.lock().await = settings.pinned_ips;
            }
        }
    }
}

async fn persist() -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "Ayar dosyası konumu bulunamadı.".to_string())?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let download_dir = DOWNLOAD_DIR.lock().await.clone();
    let pinned_ips = PINNED_IPS.lock().await.clone();
    let settings = AppSettings { download_dir, pinned_ips };
    let json = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, json).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn current_download_dir() -> PathBuf {
    DOWNLOAD_DIR.lock().await.clone()
}

pub async fn set_download_dir(new_dir: PathBuf) -> Result<(), String> {
    if !new_dir.is_dir() {
        return Err("Seçilen yol bir klasör değil.".to_string());
    }
    *DOWNLOAD_DIR.lock().await = new_dir;
    persist().await
}

pub async fn pinned_ips() -> Vec<String> {
    PINNED_IPS.lock().await.clone()
}

pub async fn add_pinned_ip(ip: String) {
    {
        let mut list = PINNED_IPS.lock().await;
        if !list.contains(&ip) {
            list.push(ip);
        }
    }
    if let Err(e) = persist().await {
        println!("Sabit IP kaydedilemedi: {}", e);
    }
}

pub async fn clear_pinned_ips() {
    PINNED_IPS.lock().await.clear();
    if let Err(e) = persist().await {
        println!("Sabit IP listesi temizlenemedi: {}", e);
    }
}
