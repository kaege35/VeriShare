// ─── KALICI AYARLAR ─────────────────────────────────────────────────────
//
// Neden: İndirme klasörü eskiden hep sabit `dirs::download_dir()` idi,
// kullanıcı değiştiremiyordu. Artık `%APPDATA%/verishare/settings.json`
// (macOS/Linux'ta karşılığı `dirs::config_dir()`) içine kaydediliyor ve
// `DOWNLOAD_DIR` global'i tüm gelen transferlerde canlı olarak okunuyor —
// uygulama yeniden başlatılmadan değişiklik anında etkili olur.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub download_dir: PathBuf,
}

lazy_static::lazy_static! {
    pub static ref DOWNLOAD_DIR: Arc<Mutex<PathBuf>> = Arc::new(Mutex::new(default_download_dir()));
}

fn default_download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn settings_path() -> Option<PathBuf> {
    let mut dir = dirs::config_dir()?;
    dir.push("verishare");
    Some(dir.join("settings.json"))
}

/// Uygulama açılışında bir kez çağrılır — diskteki ayar varsa DOWNLOAD_DIR'a yükler.
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
            }
        }
    }
}

pub async fn current_download_dir() -> PathBuf {
    DOWNLOAD_DIR.lock().await.clone()
}

pub async fn set_download_dir(new_dir: PathBuf) -> Result<(), String> {
    if !new_dir.is_dir() {
        return Err("Seçilen yol bir klasör değil.".to_string());
    }

    *DOWNLOAD_DIR.lock().await = new_dir.clone();

    let path = settings_path().ok_or_else(|| "Ayar dosyası konumu bulunamadı.".to_string())?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let settings = AppSettings { download_dir: new_dir };
    let json = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, json).await.map_err(|e| e.to_string())?;
    Ok(())
}
