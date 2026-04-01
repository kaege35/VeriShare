use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

mod discovery;
mod transfer;

#[tauri::command]
async fn start_discovery(app: AppHandle, name: String) -> Result<(), String> {
    println!("Ağ cihaz keşfi başlatılıyor... Kullanıcı adı: {}", name);
    let id = uuid::Uuid::new_v4().to_string();
    let tcp_port = transfer::TRANSFER_PORT;
    match discovery::start_discovery_loop(app, id, name, tcp_port).await {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("Keşif modülü başlatılamadı: {}", e))
    }
}

#[tauri::command]
async fn open_file_dialog(app: AppHandle, peer_ip: String) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog().file().pick_files(move |file_paths| {
        if let Some(paths) = file_paths {
            let pbs: Vec<std::path::PathBuf> = paths.into_iter().map(|p| p.into_path().unwrap()).collect();
            let app_c = app.clone();
            
            tauri::async_runtime::spawn(async move {
                let _ = app_c.emit("transfer-event", format!("{} öğe gönderiliyor...", pbs.len()));
                if let Err(e) = transfer::send_items(&peer_ip, pbs).await {
                    let _ = app_c.emit("transfer-event", format!("Hata: {}", e));
                }
            });
        }
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let quit_i = MenuItem::with_id(app, "quit", "Çıkış", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "EasyShare'i Göster", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("EasyShare")
                .menu(&menu)
                .menu_on_left_click(false)
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

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = transfer::start_transfer_server(handle).await {
                    println!("Transfer server failed: {}", e);
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
        .invoke_handler(tauri::generate_handler![
            start_discovery,
            open_file_dialog
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
