use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter};

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const DISCOVERY_PORT: u16 = 53317;
// Kurumsal ağlarda multicast (IGMP snooping/querier eksikliği yüzünden)
// filtrelenebiliyor. Aynı anda düz UDP broadcast da göndererek, multicast
// çalışmasa bile keşfin çalışma ihtimalini artırıyoruz. Tam "client
// isolation" (AP'nin cihazlar arası TÜM trafiği engellemesi) durumunda hiçbir
// yöntem işe yaramaz — bu durumda "Manuel IP ile Bağlan" tek çözümdür.
const BROADCAST_ADDR: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

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

// Global self ID / self info / soket referansı — frontend'e dönebilmek ve
// manuel IP ile probe atabilmek için.
lazy_static::lazy_static! {
    static ref SELF_ID: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    static ref SELF_INFO: Arc<Mutex<Option<PeerInfo>>> = Arc::new(Mutex::new(None));
    static ref ANNOUNCE_SOCKET: Arc<Mutex<Option<Arc<UdpSocket>>>> = Arc::new(Mutex::new(None));
    static ref FORCE_ANNOUNCE: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    static ref DISCOVERY_RUNNING: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
}

pub async fn set_self_id(id: String) {
    *SELF_ID.lock().await = Some(id);
}

pub async fn force_announce() {
    FORCE_ANNOUNCE.notify_one();
}

/// Bu makinedeki tüm IPv4 arayüzlerini (loopback hariç) döndürür.
/// VPN aktifken ya da hem Ethernet hem Wi-Fi bağlıyken birden fazla arayüz
/// olabilir — hepsinden yayın yapmazsak keşif "yanlış" arayüzden gidip LAN'a
/// hiç ulaşmayabilir.
fn local_ipv4_interfaces() -> Vec<Ipv4Addr> {
    match local_ip_address::list_afinet_netifas() {
        Ok(list) => list
            .into_iter()
            .filter_map(|(_name, ip)| match ip {
                IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
                _ => None,
            })
            .collect(),
        Err(e) => {
            println!("Ağ arayüzleri listelenemedi: {:?}", e);
            Vec::new()
        }
    }
}

async fn send_announce_all_interfaces(socket: &UdpSocket, info: &PeerInfo) {
    let Ok(json) = serde_json::to_string(info) else { return; };
    let bytes = json.as_bytes();

    let interfaces = local_ipv4_interfaces();
    if interfaces.is_empty() {
        // Arayüz listesi alınamadıysa en azından OS'un varsayılan rotasından gönder.
        let dest = SocketAddr::from((MULTICAST_ADDR, DISCOVERY_PORT));
        let _ = socket.send_to(bytes, dest).await;
        return;
    }

    let dest = SocketAddr::from((MULTICAST_ADDR, DISCOVERY_PORT));
    for iface in &interfaces {
        // Multicast paketinin bu arayüzden çıkmasını sağla, sonra gönder.
        // tokio::net::UdpSocket, IP_MULTICAST_IF gibi gelişmiş seçenekleri
        // inherent metod olarak sunmuyor — socket2::SockRef ile sahiplik
        // almadan (referansla) alttaki fd/handle üzerinden ayarlıyoruz.
        // SockRef'i her seferinde bloğun içinde oluşturup await'ten ÖNCE
        // düşürüyoruz — spawn edilen görevin Send olması gerektiği için
        // ondan sonraki `.await` noktasında hiçbir şeyin canlı kalmaması
        // önemli.
        {
            let sock_ref = socket2::SockRef::from(socket);
            let _ = sock_ref.set_multicast_if_v4(iface);
        }
        let _ = socket.send_to(bytes, dest).await;
    }

    // Broadcast fallback — subnet'in geniş yayın adresine de gönder.
    let broadcast_dest = SocketAddr::from((BROADCAST_ADDR, DISCOVERY_PORT));
    let _ = socket.send_to(bytes, broadcast_dest).await;
}

/// Belirli bir hedefe (manuel IP eklerken ya da yeni keşfedilen bir eşe hızlı
/// karşılık verirken) tek seferlik unicast paket gönderir.
async fn send_unicast(socket: &UdpSocket, info: &PeerInfo, addr: SocketAddr) {
    if let Ok(json) = serde_json::to_string(info) {
        let _ = socket.send_to(json.as_bytes(), addr).await;
    }
}

pub async fn start_discovery_loop(app: AppHandle, id: String, name: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Zaten çalışıyorsa tekrar başlatma
    {
        let mut running = DISCOVERY_RUNNING.lock().await;
        if *running {
            println!("Discovery zaten çalışıyor, yeniden başlatılmıyor.");
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

    if let Err(e) = socket.join_multicast_v4(MULTICAST_ADDR, Ipv4Addr::new(0, 0, 0, 0)) {
        println!("Multicast join error (ignoring if loopback): {:?}", e);
    }
    // Ayrıca her fiziksel arayüzden ayrı ayrı join dene — bazı işletim
    // sistemlerinde 0.0.0.0 ile join, ikincil arayüzlerde paket almayabiliyor.
    for iface in local_ipv4_interfaces() {
        if let Err(e) = socket.join_multicast_v4(MULTICAST_ADDR, iface) {
            println!("Multicast join ({:?}) hatası (yok sayılıyor): {:?}", iface, e);
        }
    }
    if let Err(e) = socket.set_broadcast(true) {
        println!("Broadcast izni ayarlanamadı: {:?}", e);
    }

    let socket = Arc::new(socket);
    let send_socket = socket.clone();
    let recv_socket = socket.clone();

    let my_info = PeerInfo { id: id.clone(), name: name.clone(), port, ip: None };
    *SELF_INFO.lock().await = Some(my_info.clone());
    *ANNOUNCE_SOCKET.lock().await = Some(socket.clone());

    // ─── YAYINLAMA DÖNGÜSÜ ────────────────────────────────
    let state_clone = state.clone();
    let app_broadcast = app.clone();

    tokio::spawn(async move {
        // Başlangıç burst: 5 hızlı yayın — karşı taraf açıksa anında keşfedilir
        for i in 0..5 {
            send_announce_all_interfaces(&send_socket, &my_info).await;
            println!("Burst announce {}/5", i + 1);
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        loop {
            send_announce_all_interfaces(&send_socket, &my_info).await;

            // Süresi dolmuş cihazları temizle ve periyodik UI güncellemesi gönder
            {
                let mut s = state_clone.lock().await;
                s.peers.retain(|_, (_, last_seen)| last_seen.elapsed() < Duration::from_secs(8));
                let peer_list = build_peer_list(&s);
                let _ = app_broadcast.emit("peers-updated", peer_list);
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = FORCE_ANNOUNCE.notified() => {
                    println!("Force network scan tetiklendi.");
                    for _ in 0..5 {
                        send_announce_all_interfaces(&send_socket, &my_info).await;
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        }
    });

    // ─── DINLEME DÖNGÜSÜ ──────────────────────────────────
    // FIX: app handle artık bu task'a da geçiriliyor — yeni peer'da anında bildirim
    let app_recv = app.clone();
    let reply_socket = socket.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            if let Ok((len, addr)) = recv_socket.recv_from(&mut buf).await {
                if let Ok(msg) = std::str::from_utf8(&buf[..len]) {
                    if let Ok(mut peer) = serde_json::from_str::<PeerInfo>(msg) {
                        peer.ip = Some(addr.ip().to_string());
                        let mut s = state.lock().await;

                        if peer.id != s.id {
                            let is_new = !s.peers.contains_key(&peer.id);
                            s.peers.insert(peer.id.clone(), (peer, std::time::Instant::now()));

                            if is_new {
                                let peer_list = build_peer_list(&s);
                                let my_info_reply = PeerInfo {
                                    id: s.id.clone(),
                                    name: s.name.clone(),
                                    port: s.port,
                                    ip: None,
                                };
                                drop(s);
                                let _ = app_recv.emit("peers-updated", peer_list);

                                // Yeni cihazı beklemek yerine ona doğrudan unicast
                                // yanıt gönder — periyodik multicast döngüsünü
                                // beklemeden karşı taraf da bizi anında görsün.
                                // Bu, multicast'in yalnızca tek yönde filtrelendiği
                                // (asimetrik) ağlarda ve manuel IP eklemede kritik.
                                send_unicast(&reply_socket, &my_info_reply, addr).await;
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(())
}

/// Mevcut peer listesini (kendimiz dahil) döndürür
fn build_peer_list(s: &DiscoveryState) -> Vec<PeerInfo> {
    let mut list: Vec<PeerInfo> = s.peers.values().map(|(info, _)| info.clone()).collect();
    list.push(PeerInfo { id: s.id.clone(), name: s.name.clone(), port: s.port, ip: None });
    list
}

/// "Manuel IP ile Bağlan" — kurumsal ağda multicast/broadcast tamamen
/// filtreleniyorsa (ör. AP client isolation), kullanıcı karşı cihazın IP
/// adresini elle girip doğrudan bir unicast keşif paketi gönderebilir.
/// Karşı taraftaki VeriShare, normal dinleme soketinden bu paketi alır ve
/// (yukarıdaki `is_new` bloğu sayesinde) bize otomatik olarak unicast yanıt
/// verir — böylece iki taraf da birbirini anında görür.
pub async fn probe_peer_ip(target_ip: String) -> Result<(), String> {
    let socket_guard = ANNOUNCE_SOCKET.lock().await;
    let socket = socket_guard
        .as_ref()
        .ok_or_else(|| "Keşif henüz başlatılmadı.".to_string())?
        .clone();
    drop(socket_guard);

    let info_guard = SELF_INFO.lock().await;
    let info = info_guard
        .as_ref()
        .ok_or_else(|| "Kendi kimliğimiz henüz hazır değil.".to_string())?
        .clone();
    drop(info_guard);

    let ip: Ipv4Addr = target_ip
        .trim()
        .parse()
        .map_err(|_| "Geçersiz IP adresi.".to_string())?;

    let addr = SocketAddr::from((ip, DISCOVERY_PORT));
    send_unicast(&socket, &info, addr).await;
    Ok(())
}
