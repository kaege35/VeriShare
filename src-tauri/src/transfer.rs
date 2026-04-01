use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

pub const TRANSFER_PORT: u16 = 53318;

#[derive(Serialize, Deserialize, Debug)]
pub enum TransferProtocol {
    FileHeader {
        rel_path: String,
        file_size: u64,
    },
    AllDone,
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
    let save_dir = dirs::download_dir().unwrap_or_else(|| std::env::current_dir().unwrap());

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
            TransferProtocol::FileHeader { rel_path, file_size } => {
                let mut save_path = save_dir.clone();
                for component in rel_path.split('/') {
                    save_path.push(component);
                }

                if let Some(p) = save_path.parent() {
                    tokio::fs::create_dir_all(p).await?;
                }

                let _ = app.emit("transfer-event", format!("{} alınıyor...", rel_path));

                let mut file = tokio::fs::File::create(&save_path).await?;
                let mut buffer = [0u8; 1024 * 128]; // 128KB Chunk
                let mut remaining = file_size;
                
                while remaining > 0 {
                    let to_read = std::cmp::min(remaining, buffer.len() as u64) as usize;
                    socket.read_exact(&mut buffer[..to_read]).await?;
                    file.write_all(&buffer[..to_read]).await?;
                    remaining -= to_read as u64;
                }
            },
            TransferProtocol::AllDone => {
                let _ = app.emit("transfer-event", "Tüm transfer başarıyla tamamlandı!".to_string());
                break;
            }
        }
    }
    Ok(())
}

pub async fn send_items(peer_ip: &str, paths: Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(format!("{}:{}", peer_ip, TRANSFER_PORT)).await?;

    let mut all_files = Vec::new();
    for p in paths {
        if p.is_file() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            all_files.push((name, p));
        } else if p.is_dir() {
            let base_name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            for entry in WalkDir::new(&p).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&p) {
                        let mut full_path = PathBuf::from(&base_name);
                        full_path.push(rel);
                        let rel_str = full_path.to_string_lossy().to_string().replace("\\", "/");
                        all_files.push((rel_str, entry.path().to_path_buf()));
                    }
                }
            }
        }
    }

    for (rel_path, abs_path) in all_files {
        let size = tokio::fs::metadata(&abs_path).await?.len();
        let req = TransferProtocol::FileHeader { rel_path, file_size: size };
        
        let req_json = serde_json::to_vec(&req)?;
        socket.write_all(&(req_json.len() as u32).to_be_bytes()).await?;
        socket.write_all(&req_json).await?;
        
        let mut file = tokio::fs::File::open(&abs_path).await?;
        let mut buffer = [0u8; 1024 * 128];
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 { break; }
            socket.write_all(&buffer[..n]).await?;
        }
    }

    let end_json = serde_json::to_vec(&TransferProtocol::AllDone)?;
    socket.write_all(&(end_json.len() as u32).to_be_bytes()).await?;
    socket.write_all(&end_json).await?;

    Ok(())
}

