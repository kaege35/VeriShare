use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter};

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const DISCOVERY_PORT: u16 = 53317;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub ip: Option<String>,
}

pub struct DiscoveryState {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub peers: std::collections::HashMap<String, (PeerInfo, std::time::Instant)>,
}

// Global self ID — frontend'e dönebilmek için
lazy_static::lazy_static! {
    static ref SELF_ID: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    static ref FORCE_ANNOUNCE: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    static ref DISCOVERY_RUNNING: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
}

pub async fn set_self_id(id: String) {
    *SELF_ID.lock().await = Some(id);
}

pub async fn force_announce() {
    FORCE_ANNOUNCE.notify_one();
}

async fn send_announce(socket: &UdpSocket, info: &PeerInfo) {
    if let Ok(json) = serde_json::to_string(info) {
        let dest = SocketAddr::from((MULTICAST_ADDR, DISCOVERY_PORT));
        let _ = socket.send_to(json.as_bytes(), dest).await;
    }
}

pub async fn start_discovery_loop(app: AppHandle, id: String, name: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Zaten çalışıyorsa tekrar başlatma
    {
        let mut running = DISCOVERY_RUNNING.lock().await;
        if *running {
            println!("Discovery zaten çalışıyor, yeniden başlatılmıyor.");
            // Sadece force announce yap
            FORCE_ANNOUNCE.notify_one();
            return Ok(());
        }
        *running = true;
    }

    let state = Arc::new(Mutex::new(DiscoveryState {
        id: id.clone(),
        name: name.clone(),
        port,
        peers: std::collections::HashMap::new(),
    }));

    let addr = SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT));
    
    let socket = UdpSocket::bind(addr).await?;
    
    // Multicast ayarları
    if let Err(e) = socket.join_multicast_v4(MULTICAST_ADDR, Ipv4Addr::new(0, 0, 0, 0)) {
        println!("Multicast join error (ignoring if loopback): {:?}", e);
    }
    
    let socket = Arc::new(socket);
    
    let send_socket = socket.clone();
    let recv_socket = socket.clone();
    
    // Yayınlama (Broadcast) döngüsü
    let state_clone = state.clone();
    let app_clone = app.clone();
    
    tokio::spawn(async move {
        let my_info = PeerInfo {
            id: id.clone(),
            name: name.clone(),
            port,
            ip: None,
        };

        // ─── BAŞLANGIÇ BURST: 5 hızlı yayın (500ms aralıkla) ───
        // Bu sayede karşı cihaz zaten açıksa anında keşfedilir
        for i in 0..5 {
            send_announce(&send_socket, &my_info).await;
            println!("Burst announce {}/5 gönderildi", i + 1);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        loop {
            // Normal periyodik yayın
            send_announce(&send_socket, &my_info).await;
            
            // Süresi dolmuş (offline) cihazları temizle ve güncelle
            {
                let mut s = state_clone.lock().await;
                // 8 saniye boyunca sinyal gelmeyen cihazları kaldır
                s.peers.retain(|_, (_, last_seen)| last_seen.elapsed() < Duration::from_secs(8));
                
                let mut peer_list = Vec::new();
                for (_, (info, _)) in s.peers.iter() {
                    peer_list.push(info.clone());
                }
                
                // Kendimizi de listeye ekle
                peer_list.push(PeerInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    port: s.port,
                    ip: None,
                });
                
                let _ = app_clone.emit("peers-updated", peer_list);
            }
            
            // 2 saniye ya da force announce bekle (eskiden 3 saniyeydi)
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = FORCE_ANNOUNCE.notified() => {
                    println!("Force network scan tetiklendi.");
                    // Ek olarak hızlı 5x announce gönder
                    for _ in 0..5 {
                        send_announce(&send_socket, &my_info).await;
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                }
            }
        }
    });
    
    // Dinleme (Receive) döngüsü
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            if let Ok((len, addr)) = recv_socket.recv_from(&mut buf).await {
                if let Ok(msg) = std::str::from_utf8(&buf[..len]) {
                    if let Ok(mut peer) = serde_json::from_str::<PeerInfo>(msg) {
                        peer.ip = Some(addr.ip().to_string());
                        let mut s = state.lock().await;
                        
                        // Kendi gönderdiğimiz paketleri yok say
                        if peer.id != s.id {
                            let is_new = !s.peers.contains_key(&peer.id);
                            s.peers.insert(peer.id.clone(), (peer, std::time::Instant::now()));
                            
                            // Yeni cihaz keşfedildiğinde anında UI güncelle
                            if is_new {
                                let mut peer_list = Vec::new();
                                for (_, (info, _)) in s.peers.iter() {
                                    peer_list.push(info.clone());
                                }
                                peer_list.push(PeerInfo {
                                    id: s.id.clone(),
                                    name: s.name.clone(),
                                    port: s.port,
                                    ip: None,
                                });
                                // Lock'u bırakmadan önce emit et
                                drop(s);
                                // Yeni peer anında listeye eklendi — frontend'e bildir
                                // (sonraki periyodik yayında da gönderilecek ama anlık olsun)
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(())
}
