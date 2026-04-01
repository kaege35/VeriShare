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

pub async fn start_discovery_loop(app: AppHandle, id: String, name: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(Mutex::new(DiscoveryState {
        id: id.clone(),
        name: name.clone(),
        port,
        peers: std::collections::HashMap::new(),
    }));

    let addr = SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT));
    
    // soketi tokio ile bağlayıp yapılandırıyoruz
    let socket = UdpSocket::bind(addr).await?;
    
    // Multi-cast grubuna katılıyoruz, böylece ağdaki diğer cihazları duyabiliriz
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
        loop {
            let info = {
                let s = state_clone.lock().await;
                PeerInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    port: s.port,
                    ip: None,
                }
            };
            
            // Kendi bilgimizi multicast ile ağa ilan edelim
            if let Ok(json) = serde_json::to_string(&info) {
                let dest = SocketAddr::from((MULTICAST_ADDR, DISCOVERY_PORT));
                let _ = send_socket.send_to(json.as_bytes(), dest).await;
            }
            
            // Süresi dolmuş (offline) cihazları listeden temizleyip arayüze güncel listeyi yollayalım
            {
                let mut s = state_clone.lock().await;
                s.peers.retain(|_, (_, last_seen)| last_seen.elapsed() < Duration::from_secs(10));
                
                let mut peer_list = Vec::new();
                for (_, (info, _)) in s.peers.iter() {
                    peer_list.push(info.clone());
                }
                
                // Kendimizi de listeye ekliyoruz
                peer_list.push(PeerInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    port: s.port,
                    ip: None, // kendi ip'mize ihtiyacımız yok
                });
                
                let _ = app_clone.emit("peers-updated", peer_list);
            }
            
            // 3 saniyede bir yayın yapacağız
            tokio::time::sleep(Duration::from_secs(3)).await;
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
                        
                        // Kendi gönderdiğimiz paketleri yok sayalım
                        if peer.id != s.id {
                            s.peers.insert(peer.id.clone(), (peer, std::time::Instant::now()));
                        }
                    }
                }
            }
        }
    });

    Ok(())
}
