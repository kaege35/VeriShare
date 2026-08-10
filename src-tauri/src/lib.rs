use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

mod crypto;
mod discovery;
mod settings;
mod transfer;

#[tauri::command]
async fn start_discovery(app: AppHandle, name: String) -> Result<String, String> {
    println!("Ağ cihaz keşfi başlatılıyor... Kullanıcı adı: {}", name);
    let id = uuid::Uuid::new_v4().to_string();
    let tcp_port = transfer::TRANSFER_PORT;

    // Self ID'yi sakla
    discovery::set_self_id(id.clone()).await;

    match discovery::start_discovery_loop(app, id.clone(), name, tcp_port).await {
        Ok(_) => Ok(id), // Self ID'yi frontend'e döndür
        Err(e) => Err(format!("Keşif modülü başlatılamadı: {}", e))
    }
}

#[tauri::command]
async fn scan_network() -> Result<(), String> {
    discovery::force_announce().await;
    Ok(())
}

// Kurumsal Wi-Fi'de multicast/broadcast tamamen engellenmişse (AP client
// isolation), kullanıcı karşı tarafın IP adresini elle girip doğrudan bir
// keşif paketi gönderebilir. Bkz. discovery::probe_peer_ip.
#[tauri::command]
async fn add_peer_by_ip(ip: String) -> Result<(), String> {
    discovery::probe_peer_ip(ip).await
}

#[derive(serde::Serialize)]
struct SettingsPayload {
    download_dir: String,
}

#[tauri::command]
async fn get_settings() -> Result<SettingsPayload, String> {
    let dir = settings::current_download_dir().await;
    Ok(SettingsPayload { download_dir: dir.to_string_lossy().into_owned() })
}

#[tauri::command]
async fn set_download_dir(path: String) -> Result<(), String> {
    settings::set_download_dir(std::path::PathBuf::from(path)).await
}

#[tauri::command]
async fn send_paths_directly(app: AppHandle, peer_ip: String, paths: Vec<String>) -> Result<(), String> {
    let pbs: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let app_c = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = transfer::send_items(&peer_ip, pbs, app_c.clone()).await {
            let msg = format!("{}", e);
            if msg.contains("İPTAL_EDİLDİ") {
                let _ = app_c.emit("transfer-event", "Transfer iptal edildi.".to_string());
            } else {
                let _ = app_c.emit("transfer-event", format!("Hata: {}", e));
            }
        }
    });
    Ok(())
}

#[tauri::command]
async fn respond_to_transfer(id: String, accept: bool) -> Result<(), String> {
    if let Some(tx) = transfer::PENDING_TRANSFERS.lock().await.remove(&id) {
        let _ = tx.send(accept);
    }
    Ok(())
}

#[tauri::command]
async fn cancel_transfer(id: String) -> Result<(), String> {
    transfer::cancel_transfer_by_id(id).await
}

#[tauri::command]
async fn get_wifi_ssid() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        // macOS Sonoma (14) ve sonrasında Apple, "Konum Servisleri" izni
        // olmayan uygulamalara Wi-Fi adını okutmayı kısıtlıyor.
        // system_profiler 10 saniye boyunca sistemi dondurduğu için onu
        // KULLANMIYORUZ. Bunun yerine `networksetup -getairportnetwork`
        // deniyoruz — çoğu sistemde konum izni istemeden anında yanıt verir;
        // izin/donanım kısıtlaması varsa yine de zarifçe hataya düşüp
        // frontend'de sessizce yok sayılır (fetchWifiSSID zaten try/catch'li).
        let output = tokio::task::spawn_blocking(|| {
            std::process::Command::new("networksetup")
                .args(["-getairportnetwork", "en0"])
                .output()
        }).await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        let text = String::from_utf8_lossy(&output.stdout).to_string();
        // Beklenen çıktı: "Current Wi-Fi Network: AğAdı"
        if let Some((_, name)) = text.trim().rsplit_once(": ") {
            let name = name.trim();
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
        Err("Apple Gizlilik Koruması ya da Wi-Fi kapalı".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        // Windows'ta işlem arka planda asenkron çalışacak, böylece donma olmayacak.
        let output = tokio::task::spawn_blocking(|| {
            std::process::Command::new("netsh")
                .args(["wlan", "show", "interfaces"])
                .output()
        }).await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        let text = String::from_utf8_lossy(&output.stdout).to_string();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("SSID") && !trimmed.starts_with("BSSID") {
                if let Some(ssid) = trimmed.split(": ").nth(1) {
                    return Ok(ssid.trim().to_string());
                }
            }
        }
        Err("WiFi ağı bulunamadı".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("Bu platform desteklenmiyor".to_string())
    }
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    if let Some(update) = updater.check().await.map_err(|e| e.to_string())? {
        update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
        // Güncelleme bittikten sonra uygulamayı otomatik yeniden başlat
        app.restart();
    }
    Ok(())
}

#[tauri::command]
fn show_in_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    { std::process::Command::new("explorer").args(["/select,", &path]).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").args(["-R", &path]).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

#[tauri::command]
fn open_file(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    { std::process::Command::new("explorer").arg(&path).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(&path).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single instance — ilk plugin olarak kayıtlı olmalı
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // İkinci girişimde mevcut pencereyi öne getir
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .invoke_handler(tauri::generate_handler![
            start_discovery, 
            send_paths_directly,
            respond_to_transfer,
            cancel_transfer,
            install_update,
            get_wifi_ssid,
            scan_network,
            add_peer_by_ip,
            get_settings,
            set_download_dir,
            open_file,
            show_in_folder
        ])
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let quit_i = MenuItem::with_id(app, "quit", "Çıkış", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "VeriShare'i Göster", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("VeriShare")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app: &AppHandle, event| match event.id.as_ref() {
                    "quit" => { std::process::exit(0); }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray: &tauri::tray::TrayIcon, event| match event {
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            // Kayıtlı ayarları (ör. özel indirme klasörü) yükle, sonra TCP
            // Transfer Sunucusunu başlat — sıralama önemli, sunucu save_dir'i
            // her bağlantıda ayarlardan okuyor.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                settings::load().await;
                if let Err(e) = transfer::start_transfer_server(handle).await {
                    println!("Transfer server başlatılamadı: {}", e);
                }
            });

            // Açılışta arka planda anında otomatik güncelleme kontrolü
            let updater_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Gecikmeyi kaldırdık (hemen sorar)
                use tauri_plugin_updater::UpdaterExt;
                match updater_handle.updater() {
                    Ok(updater) => {
                        match updater.check().await {
                            Ok(Some(update)) => {
                                let version = update.version.clone();
                                let _ = updater_handle.emit("update-available", version);
                            }
                            Ok(None) => { println!("Uygulama güncel."); }
                            Err(e) => { println!("Güncelleme kontrol hatası: {}", e); }
                        }
                    }
                    Err(e) => { println!("Updater başlatılamadı: {}", e); }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                let _ = window.hide();
                api.prevent_close();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
