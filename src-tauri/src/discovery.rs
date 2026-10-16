// ─── DAYANIKLI AĞ KEŞFİ ────────────────────────────────────────────────────
//
// Bu modül eskiden iki ayrı task'tan (yayın + dinleme) oluşuyordu ve şu
// zayıflıkları vardı (ayrıntı: README "v6.2.0" notu):
//
//  1. `DISCOVERY_RUNNING` bayrağı bind'dan ÖNCE set ediliyordu; bind
//     başarısız olursa (port başka uygulamada / Windows tarafından rezerve)
//     bayrak sonsuza dek true kalıyor, sonraki denemeler "zaten çalışıyor"
//     deyip hiçbir şey başlatmıyordu → kullanıcı "hiçbir şekilde giremiyor".
//  2. Multicast üyeliği yalnızca açılışta kuruluyordu. Wi-Fi/VPN/kablo
//     değişince ya da uyku sonrası üyelikler kernel'de düşüyor, uygulama
//     bunu fark etmiyordu → "interneti kapatıp açınca düzeliyor".
//  3. Peer ömrü 8 sn'ydi; Wi-Fi'da çoklu yayın paketleri güç tasarrufu /
//     DTIM yüzünden sık kaybolduğundan cihazlar listede yanıp sönüyordu.
//  4. Yayın yalnızca 255.255.255.255'e gidiyordu (macOS/Windows'ta tek bir
//     arayüzden çıkar); çoklu arayüzlü makinelerde yanlış arayüz seçilebiliyordu.
//  5. Çoklu yayın/broadcast filtreleyen kurumsal AP'lerde (ama cihazlar arası
//     unicast'e izin verenlerde) hiçbir yedek yol yoktu.
//
// Yeni tasarım: TEK bir supervisor task'ı "nesil" (generation) döngüsüyle
// çalışır. Her nesil: soketi bağlar → tüm arayüzlere multicast üyeliği kurar
// → select! döngüsüyle dinler/yayınlar. Şunlardan biri olursa nesil biter ve
// soket sıfırdan kurulur: arayüz kümesi değişti, sistem uykudan döndü, uzun
// süre hiç paket gelmedi, alım hataları sürekli, kullanıcı "sıfırla" dedi.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;

use crate::settings;
use crate::transfer::TRANSFER_PORT;

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const DISCOVERY_PORT: u16 = 53317;
const BROADCAST_ADDR: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

// Peer'dan en son bu süre içinde hiçbir kanaldan (multicast/broadcast/unicast)
// paket gelmezse listeden düşer. 2 sn'lik yayın aralığında ~10 kayıp paket
// demek; Wi-Fi güç tasarrufu kayıplarını tolere eder.
const PEER_TTL: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_secs(2);
// Hiçbir cihazdan paket duyulmazsa (sessizlik) soketi bir kez sıfırlamadan önce beklenen süre.
const SILENCE_RESET_SECS: u64 = 45;
// Sessizlik nedeniyle yapılan sıfırlamalar arasındaki minimum süre
// (ortamda tek başına olan kullanıcıyı sürekli yeniden bağlamamak için).
const SILENCE_RESET_MIN_GAP: Duration = Duration::from_secs(300);
// Hiç peer yokken otomatik alt ağ taraması aralığı.
const AUTO_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
// Wall-clock'ta tick'ler arası bu kadar boşluk = sistem uyudu / askıya alındı.
const SLEEP_GAP: Duration = Duration::from_secs(8);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub ip: Option<String>,
    /// Gönderenin TÜM IPv4 adresleri. Eski sürümler bu alanı yok sayar
    /// (serde bilinmeyen alanları atlar), eski sürümden gelen paketlerde boş
    /// kalır. Alıcı, kendi alt ağında olan adresi seçer — VPN/sanal adaptör
    /// IP'sinin yanlışlıkla hedef alınmasını önler.
    #[serde(default)]
    pub ips: Vec<String>,
}

struct PeerEntry {
    info: PeerInfo,
    /// Paketin geldiği soket adresi (unicast heartbeat/yanıt buraya gider).
    disc_addr: SocketAddr,
    last_seen: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Iface {
    name: String,
    ip: Ipv4Addr,
    mask: Ipv4Addr,
}

impl Iface {
    fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.ip) | !u32::from(self.mask))
    }
    fn prefix(&self) -> u32 {
        u32::from(self.mask).count_ones()
    }
}

#[derive(Default, Clone)]
struct DiagState {
    bind_error: Option<String>,
    bind_port: u16,
    fallback_reason: Option<String>,
    interfaces: Vec<String>,
    joined: Vec<String>,
    packets_sent: u64,
    self_packets: u64,
    peer_packets: u64,
    recv_errors: u64,
    resets: u32,
    last_reset_reason: Option<String>,
    sweeps: u32,
    gen_started: Option<Instant>,
    last_peer_packet: Option<Instant>,
    transfer_error: Option<String>,
    transfer_ok: bool,
}

lazy_static::lazy_static! {
    static ref SELF_INFO: StdMutex<Option<PeerInfo>> = StdMutex::new(None);
    static ref PEERS: StdMutex<HashMap<String, PeerEntry>> = StdMutex::new(HashMap::new());
    static ref PINNED: StdMutex<HashSet<Ipv4Addr>> = StdMutex::new(HashSet::new());
    static ref CURRENT_SOCKET: StdMutex<Option<Arc<UdpSocket>>> = StdMutex::new(None);
    static ref DIAG: StdMutex<DiagState> = StdMutex::new(DiagState::default());
    static ref LAST_STATUS: StdMutex<String> = StdMutex::new(String::new());
    static ref LAST_SILENCE_RESET: StdMutex<Option<Instant>> = StdMutex::new(None);
    static ref FORCE_ANNOUNCE: Arc<Notify> = Arc::new(Notify::new());
    static ref RESET_REQUEST: Arc<Notify> = Arc::new(Notify::new());
}

static SUPERVISOR_STARTED: AtomicBool = AtomicBool::new(false);
static SWEEPING: AtomicBool = AtomicBool::new(false);

// ─── Küçük yardımcılar (hepsi senkron — std Mutex guard'ı asla await'e sızmaz) ──

fn diag<F: FnOnce(&mut DiagState)>(f: F) {
    if let Ok(mut d) = DIAG.lock() {
        f(&mut d);
    }
}

/// Transfer (TCP) sunucusu kendi bağlanma durumunu buraya bildirir —
/// tanılama panelinde "dosya alma servisi çalışıyor mu" görünsün diye.
pub fn set_transfer_status(ok: bool, error: Option<String>) {
    diag(|d| {
        d.transfer_ok = ok;
        d.transfer_error = error;
    });
}

pub fn force_announce_now() {
    FORCE_ANNOUNCE.notify_one();
}

pub async fn force_announce() {
    force_announce_now();
}

/// Soketi baştan kurdurur (tanılama panelindeki "Bağlantıyı Sıfırla").
pub fn request_reset() {
    RESET_REQUEST.notify_one();
}

pub fn clear_pinned() {
    if let Ok(mut p) = PINNED.lock() {
        p.clear();
    }
}

fn current_socket() -> Option<Arc<UdpSocket>> {
    CURRENT_SOCKET.lock().ok().and_then(|g| g.clone())
}

fn set_current_socket(s: Option<Arc<UdpSocket>>) {
    if let Ok(mut g) = CURRENT_SOCKET.lock() {
        *g = s;
    }
}

fn clear_peers() {
    if let Ok(mut p) = PEERS.lock() {
        p.clear();
    }
}

fn expire_peers() {
    if let Ok(mut p) = PEERS.lock() {
        p.retain(|_, e| e.last_seen.elapsed() < PEER_TTL);
    }
}

fn peer_count() -> usize {
    PEERS.lock().map(|p| p.len()).unwrap_or(0)
}

fn silence_reset_allowed() -> bool {
    if let Ok(mut last) = LAST_SILENCE_RESET.lock() {
        let ok = last.map(|t| t.elapsed() > SILENCE_RESET_MIN_GAP).unwrap_or(true);
        if ok {
            *last = Some(Instant::now());
        }
        ok
    } else {
        false
    }
}

/// Kendi bilgimiz + güncel IPv4 adres listemiz.
fn current_self_info(ifaces: &[Iface]) -> PeerInfo {
    let mut info = SELF_INFO
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or(PeerInfo {
            id: String::new(),
            name: String::new(),
            port: TRANSFER_PORT,
            ip: None,
            ips: Vec::new(),
        });
    info.ip = None;
    info.ips = ifaces.iter().map(|i| i.ip.to_string()).collect();
    info
}

/// Arayüze gönderilecek liste: bilinen peer'lar + (en sonda) kendimiz.
fn build_peer_list() -> Vec<PeerInfo> {
    let mut list: Vec<PeerInfo> = PEERS
        .lock()
        .map(|p| p.values().map(|e| e.info.clone()).collect())
        .unwrap_or_default();
    if let Some(me) = SELF_INFO.lock().ok().and_then(|g| g.clone()) {
        list.push(PeerInfo { ip: None, ips: Vec::new(), ..me });
    }
    list
}

// ─── Ağ arayüzleri ───────────────────────────────────────────────────────────

/// Bu makinedeki kullanılabilir IPv4 arayüzleri (loopback / link-local /
/// bariz sanal arayüzler hariç). Alt ağ maskesini de verir — böylece
/// "255.255.255.255" yerine her arayüzün KENDİ yönlendirilmiş broadcast
/// adresine yayın yapabiliyoruz (çoklu arayüzlü makinelerde doğru ağa çıkar).
/// macOS/Linux'taki bariz "ağ değil" arayüzleri (AirDrop, VPN tüneli, köprüler...).
/// Önek + sayı kalıbı kullanıyoruz ki Windows'taki "Local Area Connection" /
/// "Apple Mobile Device Ethernet" gibi gerçek adaptör adları yanlışlıkla elenmesin.
fn is_virtual_iface(lname: &str) -> bool {
    for p in ["awdl", "llw", "utun", "gif", "stf", "ap"] {
        if let Some(rest) = lname.strip_prefix(p) {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    ["docker", "veth", "br-", "virbr"].iter().any(|p| lname.starts_with(p))
}

fn list_ifaces() -> Vec<Iface> {
    match if_addrs::get_if_addrs() {
        Ok(list) => {
            let mut out: Vec<Iface> = Vec::new();
            for i in list {
                if i.is_loopback() {
                    continue;
                }
                let name = i.name.clone();
                if is_virtual_iface(&name.to_lowercase()) {
                    continue;
                }
                if let if_addrs::IfAddr::V4(v4) = i.addr {
                    if v4.ip.is_link_local() || v4.ip.is_unspecified() {
                        continue;
                    }
                    let iface = Iface { name, ip: v4.ip, mask: v4.netmask };
                    if !out.contains(&iface) {
                        out.push(iface);
                    }
                }
            }
            out
        }
        Err(e) => {
            println!("Ağ arayüzleri listelenemedi (if-addrs): {:?}", e);
            // Yedek: maske bilgisi yok, /24 varsay.
            match local_ip_address::list_afinet_netifas() {
                Ok(list) => list
                    .into_iter()
                    .filter_map(|(name, ip)| match ip {
                        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(Iface {
                            name,
                            ip: v4,
                            mask: Ipv4Addr::new(255, 255, 255, 0),
                        }),
                        _ => None,
                    })
                    .collect(),
                Err(_) => Vec::new(),
            }
        }
    }
}

fn iface_key(ifaces: &[Iface]) -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut k: Vec<(Ipv4Addr, Ipv4Addr)> = ifaces.iter().map(|i| (i.ip, i.mask)).collect();
    k.sort();
    k
}

fn describe_ifaces(ifaces: &[Iface]) -> Vec<String> {
    ifaces
        .iter()
        .map(|i| format!("{} {}/{}", i.name, i.ip, i.prefix()))
        .collect()
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    (u32::from(a) & u32::from(mask)) == (u32::from(b) & u32::from(mask))
}

/// Peer'ın duyurduğu adresler arasından BİZİM alt ağımızda olanı seç; yoksa
/// paketin geldiği kaynak adresi kullan.
fn choose_peer_ip(src: IpAddr, announced: &[String], ifaces: &[Iface]) -> String {
    for s in announced {
        if let Ok(ip) = s.parse::<Ipv4Addr>() {
            if ifaces.iter().any(|i| i.ip != ip && same_subnet(i.ip, ip, i.mask)) {
                return ip.to_string();
            }
        }
    }
    src.to_string()
}

// ─── Soket ───────────────────────────────────────────────────────────────────

/// SO_REUSEADDR (+ unix'te SO_REUSEPORT) ile UDP soketi. Böylece nesil
/// değişiminde eski soket henüz tamamen kapanmamış olsa bile ya da aynı portu
/// paylaşan başka bir uygulama (ör. LocalSend) varken bind patlamaz.
fn make_socket(port: u16) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    {
        let _ = socket.set_reuse_port(true);
    }
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    let addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], port));
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Önce standart portu dene; olmazsa (Windows'un rezerve ettiği port
/// aralığı, başka uygulama vb.) sistemin verdiği rastgele porta düş. Yedek
/// portta multicast/broadcast ALAMAYIZ ama yayınlarımız hedeflere ulaşır,
/// onlar da bize kaynak adresimize unicast yanıt/heartbeat gönderir —
/// yani cihaz tamamen kör kalmaz.
fn bind_with_fallback() -> Result<(UdpSocket, u16, Option<String>), String> {
    match make_socket(DISCOVERY_PORT).and_then(UdpSocket::from_std) {
        Ok(s) => Ok((s, DISCOVERY_PORT, None)),
        Err(e1) => match make_socket(0).and_then(UdpSocket::from_std) {
            Ok(s) => {
                let p = s.local_addr().map(|a| a.port()).unwrap_or(0);
                Ok((s, p, Some(format!("UDP {} açılamadı: {}", DISCOVERY_PORT, e1))))
            }
            Err(e2) => Err(format!(
                "UDP {} açılamadı: {} | yedek port da açılamadı: {}",
                DISCOVERY_PORT, e1, e2
            )),
        },
    }
}

fn primary_port_available() -> bool {
    make_socket(DISCOVERY_PORT).is_ok()
}

fn join_groups(socket: &UdpSocket, ifaces: &[Iface]) -> Vec<String> {
    let mut joined = Vec::new();
    for iface in ifaces {
        match socket.join_multicast_v4(MULTICAST_ADDR, iface.ip) {
            Ok(_) => joined.push(format!("{} ({})", iface.name, iface.ip)),
            Err(e) => joined.push(format!("{} ({}) — HATA: {}", iface.name, iface.ip, e)),
        }
    }
    if ifaces.is_empty() {
        let _ = socket.join_multicast_v4(MULTICAST_ADDR, Ipv4Addr::UNSPECIFIED);
    }
    joined
}

// ─── Gönderim ────────────────────────────────────────────────────────────────

fn heartbeat_targets() -> Vec<SocketAddr> {
    let mut set: HashSet<SocketAddr> = HashSet::new();
    if let Ok(peers) = PEERS.lock() {
        for e in peers.values() {
            set.insert(e.disc_addr);
        }
    }
    if let Ok(pinned) = PINNED.lock() {
        for ip in pinned.iter() {
            set.insert(SocketAddr::from((*ip, DISCOVERY_PORT)));
        }
    }
    set.into_iter().collect()
}

/// Her tick'te: her arayüzden multicast + o arayüzün yönlendirilmiş
/// broadcast'i, bir de genel broadcast, bir de bilinen peer'lara / sabit IP'lere
/// doğrudan unicast heartbeat. Çoklu yayın filtreleyen ağlarda bile unicast yolu
/// peer'ları canlı tutar.
async fn announce_all(socket: &UdpSocket, ifaces: &[Iface]) {
    let info = current_self_info(ifaces);
    let Ok(json) = serde_json::to_vec(&info) else { return; };
    let mut sent: u64 = 0;

    let mcast = SocketAddr::from((MULTICAST_ADDR, DISCOVERY_PORT));
    for iface in ifaces {
        // tokio::net::UdpSocket IP_MULTICAST_IF'i sunmuyor — socket2::SockRef ile
        // sahiplik almadan ayarlıyoruz; await'ten ÖNCE düşsün diye blok içinde.
        {
            let sock_ref = socket2::SockRef::from(socket);
            let _ = sock_ref.set_multicast_if_v4(&iface.ip);
        }
        if socket.send_to(&json, mcast).await.is_ok() {
            sent += 1;
        }
        let directed = SocketAddr::from((iface.broadcast(), DISCOVERY_PORT));
        if socket.send_to(&json, directed).await.is_ok() {
            sent += 1;
        }
    }
    if ifaces.is_empty() {
        if socket.send_to(&json, mcast).await.is_ok() {
            sent += 1;
        }
    }
    let limited = SocketAddr::from((BROADCAST_ADDR, DISCOVERY_PORT));
    if socket.send_to(&json, limited).await.is_ok() {
        sent += 1;
    }

    for target in heartbeat_targets() {
        if socket.send_to(&json, target).await.is_ok() {
            sent += 1;
        }
    }

    diag(|d| d.packets_sent += sent);
}

async fn send_unicast(socket: &UdpSocket, info: &PeerInfo, addr: SocketAddr) {
    if let Ok(json) = serde_json::to_vec(info) {
        if socket.send_to(&json, addr).await.is_ok() {
            diag(|d| d.packets_sent += 1);
        }
    }
}

// ─── Alt ağ taraması (multicast/broadcast filtreleyen ağlar için yedek yol) ──

/// Her arayüzün alt ağındaki tüm ana bilgisayar adresleri (en fazla /22;
/// daha geniş ağlarda yalnızca kendi /24'ümüz).
fn sweep_targets(ifaces: &[Iface]) -> Vec<SocketAddr> {
    let mut set: HashSet<SocketAddr> = HashSet::new();
    for iface in ifaces {
        let prefix = iface.prefix();
        if prefix < 16 || prefix >= 31 {
            continue;
        }
        let ip = u32::from(iface.ip);
        let mask = u32::from(iface.mask);
        let (net, host_bits) = if prefix >= 22 {
            (ip & mask, 32 - prefix)
        } else {
            (ip & 0xFFFF_FF00, 8)
        };
        let count: u32 = 1u32 << host_bits;
        for h in 1..count.saturating_sub(1) {
            let cand = Ipv4Addr::from(net + h);
            if cand != iface.ip {
                set.insert(SocketAddr::from((cand, DISCOVERY_PORT)));
            }
        }
    }
    set.into_iter().collect()
}

/// Alt ağdaki her adrese küçük bir tanışma paketi yollar. Karşıdaki VeriShare
/// bunu alınca bize unicast yanıt verir. Çoklu yayın/broadcast engelli ama
/// cihazlar arası unicast serbest olan (çoğu kurumsal AP) ağlarda keşfi
/// çalışır hale getirir. Paketler 40'lık gruplar halinde, aralarda kısa bekleyerek
/// gönderilir (ARP/ağ fırtınası yaratmamak için).
fn start_sweep(socket: Arc<UdpSocket>, ifaces: Vec<Iface>) {
    if SWEEPING.swap(true, Ordering::SeqCst) {
        return; // zaten çalışıyor
    }
    diag(|d| d.sweeps += 1);
    tokio::spawn(async move {
        let info = current_self_info(&ifaces);
        if let Ok(json) = serde_json::to_vec(&info) {
            let targets = sweep_targets(&ifaces);
            let mut n: usize = 0;
            for t in targets {
                let _ = socket.send_to(&json, t).await;
                n += 1;
                if n % 40 == 0 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
            diag(|d| d.packets_sent += n as u64);
        }
        SWEEPING.store(false, Ordering::SeqCst);
    });
}

// ─── Paket işleme ────────────────────────────────────────────────────────────

enum PacketOutcome {
    Ignored,
    SelfPacket,
    Peer { is_new: bool, needs_reply: bool, reply: PeerInfo },
}

fn process_datagram(data: &[u8], src: SocketAddr, ifaces: &[Iface]) -> PacketOutcome {
    let Ok(mut peer) = serde_json::from_slice::<PeerInfo>(data) else {
        return PacketOutcome::Ignored;
    };
    let my_id = SELF_INFO.lock().ok().and_then(|g| g.as_ref().map(|i| i.id.clone()));
    if Some(&peer.id) == my_id.as_ref() {
        return PacketOutcome::SelfPacket;
    }

    peer.ip = Some(choose_peer_ip(src.ip(), &peer.ips, ifaces));

    let (is_new, needs_reply) = match PEERS.lock() {
        Ok(mut peers) => {
            let prev_age = peers.get(&peer.id).map(|e| e.last_seen.elapsed());
            let is_new = prev_age.is_none();
            // Yeni VEYA bir süredir sessiz olan peer'a doğrudan yanıt ver ki o da
            // bizi hemen (tek yönlü multicast filtrelemesi olsa bile) görsün.
            let needs_reply = prev_age.map(|a| a > Duration::from_secs(6)).unwrap_or(true);
            peers.insert(
                peer.id.clone(),
                PeerEntry { info: peer, disc_addr: src, last_seen: Instant::now() },
            );
            (is_new, needs_reply)
        }
        Err(_) => (false, false),
    };

    PacketOutcome::Peer { is_new, needs_reply, reply: current_self_info(ifaces) }
}

// ─── UI'ya bildirimler ───────────────────────────────────────────────────────

fn emit_peers_if_changed(app: &AppHandle, last_sig: &mut String) {
    let list = build_peer_list();
    let mut keys: Vec<String> = list
        .iter()
        .map(|p| format!("{}|{}|{}", p.id, p.name, p.ip.clone().unwrap_or_default()))
        .collect();
    keys.sort();
    let sig = keys.join(";");
    if *last_sig != sig {
        *last_sig = sig;
        let _ = app.emit("peers-updated", list);
    }
}

fn emit_status_if_changed(app: &AppHandle) {
    let r = diagnostics();
    let key = format!("{}|{}", r.state, r.summary);
    let changed = match LAST_STATUS.lock() {
        Ok(mut last) => {
            if *last != key {
                *last = key;
                true
            } else {
                false
            }
        }
        Err(_) => false,
    };
    if changed {
        let _ = app.emit(
            "discovery-status",
            serde_json::json!({ "state": r.state, "summary": r.summary }),
        );
    }
}

// ─── Nesil döngüsü ───────────────────────────────────────────────────────────

/// Tek bir soket "nesli". Dönünce supervisor soketi baştan kurar.
async fn run_generation(app: &AppHandle, socket: Arc<UdpSocket>, port: u16, fallback_reason: Option<String>) {
    // Nesiller arasında bekleyen (bayat) sıfırlama izni varsa boşalt — yoksa
    // kullanıcının bastığı tek bir "Sıfırla" yeni nesli bir kez daha sıfırlar.
    let _ = tokio::time::timeout(Duration::ZERO, RESET_REQUEST.notified()).await;

    let fallback = fallback_reason.is_some();
    let ifaces = list_ifaces();
    let gen_key = iface_key(&ifaces);
    let joined = if fallback { Vec::new() } else { join_groups(&socket, &ifaces) };

    let described = describe_ifaces(&ifaces);
    diag(|d| {
        d.bind_error = None;
        d.bind_port = port;
        d.fallback_reason = fallback_reason.clone();
        d.interfaces = described;
        d.joined = joined;
        d.gen_started = Some(Instant::now());
        // Paket sayaçları NESİL bazlı: ağ değişince bozulan çoklu yayın, eski
        // neslin sayaçları yüzünden "ok" görünmesin.
        d.self_packets = 0;
        d.peer_packets = 0;
        d.last_peer_packet = None;
    });
    set_current_socket(Some(socket.clone()));
    emit_status_if_changed(app);

    // Açılış yayını: hızlı 3'lü + tüm alt ağa tanışma paketi.
    for _ in 0..3 {
        announce_all(&socket, &ifaces).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    start_sweep(socket.clone(), ifaces.clone());
    let mut last_sweep: Option<Instant> = Some(Instant::now());
    let mut last_primary_probe = Instant::now();

    let gen_start = Instant::now();
    let mut heard_peer = false;
    let mut consecutive_errors: u32 = 0;
    let mut last_sig = String::new();
    let mut last_wall = SystemTime::now();
    let mut buf = vec![0u8; 4096];
    let mut reason = String::new();

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            res = socket.recv_from(&mut buf) => {
                match res {
                    Ok((len, addr)) => {
                        consecutive_errors = 0;
                        match process_datagram(&buf[..len], addr, &ifaces) {
                            PacketOutcome::Ignored => {}
                            PacketOutcome::SelfPacket => {
                                diag(|d| d.self_packets += 1);
                            }
                            PacketOutcome::Peer { is_new, needs_reply, reply } => {
                                heard_peer = true;
                                diag(|d| {
                                    d.peer_packets += 1;
                                    d.last_peer_packet = Some(Instant::now());
                                });
                                if is_new {
                                    emit_peers_if_changed(app, &mut last_sig);
                                }
                                if needs_reply {
                                    send_unicast(&socket, &reply, addr).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Windows: daha önce gönderilen bir unicast pakete ICMP "port
                        // unreachable" gelirse bir sonraki recv_from ConnectionReset ile
                        // döner. Bu normal (alt ağ taramasında çok olur) — sayma.
                        if e.kind() != std::io::ErrorKind::ConnectionReset {
                            consecutive_errors += 1;
                            diag(|d| d.recv_errors += 1);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            if consecutive_errors >= 20 {
                                reason = format!("alım hatası sürüyor: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
            _ = tick.tick() => {
                let wall_now = SystemTime::now();
                let gap = wall_now.duration_since(last_wall).unwrap_or_default();
                last_wall = wall_now;
                if gap > SLEEP_GAP {
                    clear_peers();
                    reason = format!("sistem uykudan/askıdan döndü ({} sn boşluk)", gap.as_secs());
                    break;
                }

                expire_peers();

                let current = list_ifaces();
                if iface_key(&current) != gen_key {
                    clear_peers();
                    reason = "ağ arayüzleri değişti (Wi-Fi/VPN/kablo)".to_string();
                    break;
                }

                if fallback && last_primary_probe.elapsed() > Duration::from_secs(30) {
                    last_primary_probe = Instant::now();
                    if primary_port_available() {
                        reason = "standart keşif portu tekrar kullanılabilir".to_string();
                        break;
                    }
                }

                announce_all(&socket, &ifaces).await;

                if !heard_peer
                    && gen_start.elapsed() > Duration::from_secs(SILENCE_RESET_SECS)
                    && silence_reset_allowed()
                {
                    reason = "uzun süre hiçbir cihazdan paket alınamadı".to_string();
                    break;
                }

                if peer_count() == 0
                    && last_sweep.map(|t| t.elapsed() > AUTO_SWEEP_INTERVAL).unwrap_or(true)
                {
                    start_sweep(socket.clone(), ifaces.clone());
                    last_sweep = Some(Instant::now());
                }

                emit_peers_if_changed(app, &mut last_sig);
                emit_status_if_changed(app);
            }
            _ = FORCE_ANNOUNCE.notified() => {
                for _ in 0..3 {
                    announce_all(&socket, &ifaces).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                start_sweep(socket.clone(), ifaces.clone());
                last_sweep = Some(Instant::now());
            }
            _ = RESET_REQUEST.notified() => {
                reason = "manuel sıfırlama".to_string();
                break;
            }
        }
    }

    set_current_socket(None);
    println!("Keşif soketi yeniden kuruluyor: {}", reason);
    diag(|d| {
        d.resets += 1;
        d.last_reset_reason = Some(reason);
    });
}

async fn supervisor(app: AppHandle) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match bind_with_fallback() {
            Ok((socket, port, fallback_reason)) => {
                backoff = Duration::from_secs(1);
                run_generation(&app, Arc::new(socket), port, fallback_reason).await;

                // Eski soketi tutan alt ağ taraması bitsin ki yeni bind çakışmasın.
                let mut waited = 0u32;
                while SWEEPING.load(Ordering::SeqCst) && waited < 60 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    waited += 1;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Err(e) => {
                println!("Keşif soketi açılamadı: {}", e);
                diag(|d| d.bind_error = Some(e));
                emit_status_if_changed(&app);
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(15));
            }
        }
    }
}

// ─── Dış API ─────────────────────────────────────────────────────────────────

/// Keşfi başlatır (ilk çağrıda) ya da çalışan keşfin adını günceller ve
/// aynı kimliği (id) döndürür. Bind hatası artık girişi engellemez: supervisor
/// arka planda yeniden dener, durum arayüzde "Keşif sorunu" olarak görünür ve
/// "Ağ Tanılama" paneli nedenini söyler.
pub async fn ensure_started(app: AppHandle, name: String, tcp_port: u16) -> Result<String, String> {
    // Kimliği (id) ATOMİK üret/koru — hiçbir await'ten önce. Eş zamanlı iki
    // çağrı aynı id'yi görür; arayüzün "kendim" kimliği ile gerçek yayın
    // kimliği birbirinden kopmaz. Guard iç bloktadır, await'e sızmaz.
    let id = {
        let mut g = SELF_INFO
            .lock()
            .map_err(|_| "Keşif kilidi bozuk.".to_string())?;
        match g.as_mut() {
            Some(i) => {
                i.name = name.clone();
                i.id.clone()
            }
            None => {
                let new_id = uuid::Uuid::new_v4().to_string();
                *g = Some(PeerInfo {
                    id: new_id.clone(),
                    name: name.clone(),
                    port: tcp_port,
                    ip: None,
                    ips: Vec::new(),
                });
                new_id
            }
        }
    };

    if SUPERVISOR_STARTED.load(Ordering::SeqCst) {
        force_announce_now();
        return Ok(id);
    }

    // Kalıcı sabit IP'leri yükle (ayarlar henüz okunmamış olabilir).
    settings::load().await;
    let pinned = settings::pinned_ips().await;
    if let Ok(mut p) = PINNED.lock() {
        for s in pinned {
            if let Ok(ip) = s.parse::<Ipv4Addr>() {
                p.insert(ip);
            }
        }
    }

    if !SUPERVISOR_STARTED.swap(true, Ordering::SeqCst) {
        tokio::spawn(supervisor(app));
    }
    Ok(id)
}

/// "Manuel IP ile Bağlan": adresi sabitler (her tick'te unicast heartbeat gider,
/// uygulama yeniden açılınca da hatırlanır) ve hemen bir tanışma paketi yollar.
pub async fn pin_peer_ip(ip_str: String) -> Result<Ipv4Addr, String> {
    let ip: Ipv4Addr = ip_str
        .trim()
        .parse()
        .map_err(|_| "Geçersiz IPv4 adresi.".to_string())?;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return Err("Bu adres bir cihaz adresi olamaz.".to_string());
    }
    let socket = current_socket().ok_or_else(|| {
        "Keşif servisi şu an çalışmıyor — başlığın yanındaki durum noktasından Ağ Tanılama'yı açın.".to_string()
    })?;

    if let Ok(mut p) = PINNED.lock() {
        p.insert(ip);
    }

    let info = current_self_info(&list_ifaces());
    send_unicast(&socket, &info, SocketAddr::from((ip, DISCOVERY_PORT))).await;
    Ok(ip)
}

// ─── Tanılama raporu ─────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct DiagnosticsReport {
    pub state: String,
    pub summary: String,
    pub hints: Vec<String>,
    pub bind_port: u16,
    pub fallback_port_mode: bool,
    pub fallback_reason: Option<String>,
    pub bind_error: Option<String>,
    pub interfaces: Vec<String>,
    pub joined_multicast: Vec<String>,
    pub packets_sent: u64,
    pub self_packets: u64,
    pub peer_packets: u64,
    pub recv_errors: u64,
    pub last_peer_packet_secs_ago: Option<u64>,
    pub generation_age_secs: u64,
    pub peers_known: usize,
    pub resets: u32,
    pub last_reset_reason: Option<String>,
    pub sweeps: u32,
    pub pinned_ips: Vec<String>,
    pub discovery_port: u16,
    pub transfer_port: u16,
    pub transfer_server_ok: bool,
    pub transfer_server_error: Option<String>,
}

pub fn diagnostics() -> DiagnosticsReport {
    let d: DiagState = DIAG.lock().map(|g| g.clone()).unwrap_or_default();
    let bind_error = d.bind_error.clone();
    let bind_port = d.bind_port;
    let fallback_reason = d.fallback_reason.clone();
    let interfaces = d.interfaces.clone();
    let joined = d.joined.clone();
    let packets_sent = d.packets_sent;
    let self_packets = d.self_packets;
    let peer_packets = d.peer_packets;
    let recv_errors = d.recv_errors;
    let resets = d.resets;
    let last_reset_reason = d.last_reset_reason.clone();
    let sweeps = d.sweeps;
    let gen_age: u64 = d.gen_started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
    let last_peer_ago: Option<u64> = d.last_peer_packet.map(|t| t.elapsed().as_secs());
    let transfer_error = d.transfer_error.clone();
    let transfer_ok = d.transfer_ok;

    let peers_known = peer_count();
    let mut pinned_ips: Vec<String> = PINNED
        .lock()
        .map(|p| p.iter().map(|i| i.to_string()).collect())
        .unwrap_or_default();
    pinned_ips.sort();

    let fallback = fallback_reason.is_some();
    let mut hints: Vec<String> = Vec::new();
    let state: &str;
    let summary: String;

    if bind_error.is_some() {
        state = "error";
        summary = "Keşif başlatılamıyor".to_string();
        hints.push(format!(
            "UDP {} portu açılamadı. Başka bir uygulama (ör. LocalSend) bu portu kullanıyor ya da Windows (Hyper-V/WSL/Docker) bu port aralığını rezerve etmiş olabilir. Windows'ta 'netsh int ipv4 show excludedportrange protocol=udp' ile kontrol edilebilir. Uygulama arka planda yeniden deniyor.",
            DISCOVERY_PORT
        ));
    } else if interfaces.is_empty() {
        state = "error";
        summary = "Ağ bağlantısı bulunamadı".to_string();
        hints.push("Cihazın aktif bir Wi-Fi/Ethernet bağlantısı yok (ya da yalnızca VPN/sanal arayüz var). Ağa bağlanınca otomatik yeniden denenir.".to_string());
    } else if fallback {
        state = "degraded";
        summary = "Yedek port modunda".to_string();
        hints.push(format!(
            "Standart keşif portu ({}) kullanılamadığı için yedek portta çalışılıyor. Bu cihaz diğerlerini görebilir; diğerleri ise onu doğrudan (unicast) yanıtlarla görür — birkaç saniye gecikme olabilir. Neden: {}",
            DISCOVERY_PORT,
            fallback_reason.clone().unwrap_or_default()
        ));
    } else if self_packets == 0 && gen_age > 20 {
        state = "degraded";
        summary = "Ağ yayını alınamıyor".to_string();
        hints.push("Kendi yayınımızı bile alamıyoruz: güvenlik duvarı gelen UDP trafiğini engelliyor ya da ağ arayüzü çoklu yayını desteklemiyor olabilir. Windows'ta VeriShare'e hem 'Özel' hem 'Genel' ağlar için izin verildiğinden emin olun; macOS'ta Sistem Ayarları → Ağ → Güvenlik Duvarı'nı kontrol edin.".to_string());
    } else {
        state = "ok";
        summary = if peers_known > 0 {
            format!("{} cihaz görünüyor", peers_known)
        } else {
            "Cihazlar aranıyor".to_string()
        };
    }

    if state != "error" && peer_packets == 0 && gen_age > 30 && !interfaces.is_empty() {
        hints.push("Henüz hiçbir cihazdan yanıt gelmedi. Ortamda açık başka bir VeriShare yoksa bu normaldir. Varsa: Wi-Fi'da 'client isolation' (cihaz izolasyonu) ya da AP çoklu yayın/broadcast filtrelemesi olabilir — kablolu bağlantıyı ya da 'Manuel IP ile Bağlan'ı deneyin.".to_string());
    }
    if let Some(e) = &transfer_error {
        hints.push(format!(
            "Dosya alma servisi (TCP {}) başlatılamadı: {} — bu cihaz dosya ALAMAZ. Port başka bir uygulamada ya da güvenlik duvarı engelliyor olabilir. Otomatik yeniden denenir.",
            TRANSFER_PORT, e
        ));
    }
    if interfaces.len() >= 3 {
        hints.push("Birden fazla ağ arayüzü var (VPN/sanal adaptör olabilir). Hepsi otomatik kullanılıyor; yine de sorun sürerse VPN'i geçici kapatmayı deneyin.".to_string());
    }

    DiagnosticsReport {
        state: state.to_string(),
        summary,
        hints,
        bind_port,
        fallback_port_mode: fallback,
        fallback_reason,
        bind_error,
        interfaces,
        joined_multicast: joined,
        packets_sent,
        self_packets,
        peer_packets,
        recv_errors,
        last_peer_packet_secs_ago: last_peer_ago,
        generation_age_secs: gen_age,
        peers_known,
        resets,
        last_reset_reason,
        sweeps,
        pinned_ips,
        discovery_port: DISCOVERY_PORT,
        transfer_port: TRANSFER_PORT,
        transfer_server_ok: transfer_ok,
        transfer_server_error: transfer_error,
    }
}
