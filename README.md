# VeriShare 🛰️

VeriShare, kurumsal ağlar ve yerel ağlar üzerindeki kullanıcılar arasında hızlı, güvenli ve sürükle-bırak mantığıyla çalışan bir dosya aktarım ve iletişim platformudur.

## Özellikler ✨

- **Otomatik Keşif:** Aynı ağdaki kullanıcıları anında bulun.
- **Sürükle-Bırak:** Dosyaları kullanıcıların üzerine bırakarak anında gönderin.
- **Hızlı Aktarım:** Yerel ağ gücünü kullanarak yüksek hızda veri transferi.
- **Güvenli:** Kurumsal standartlarda bağlantı ve veri yönetimi.
- **Otomatik Güncelleme:** En güncel özelliklere anında sahip olun.

## Geliştirme 🛠️

VeriShare, **Tauri**, **Rust** ve **Vanilla JavaScript** teknolojileri kullanılarak geliştirilmiştir.

### Ön Gereksinimler
- Rust
- Node.js
- npm

### Başlatma
```bash
npm install
npm run tauri dev
```

## Sürüm Notları 📝

### v6.2.0 — Keşif güvenilirliği (cihaz görünmeme / giriş yapamama düzeltmeleri)
Sahada görülen iki sorunun kök nedenleri bulunup giderildi: (1) cihazların
zaman zaman listede görünmemesi, interneti kapatıp açınca düzelmesi; (2) bir
kullanıcının hiçbir şekilde ağa girememesi / kimseyi görememesi.

**Kök nedenler**
- `DISCOVERY_RUNNING` bayrağı soket bağlanmadan ÖNCE set ediliyordu. UDP 53317
  açılamazsa (LocalSend gibi bir uygulama, ya da Windows'ta Hyper-V/WSL/Docker'ın
  rezerve ettiği port aralığı) bayrak sonsuza dek true kalıyor, sonraki denemeler
  "zaten çalışıyor" deyip hiçbir şey başlatmıyordu.
- Multicast üyeliği yalnızca açılışta kuruluyordu; Wi-Fi/VPN/kablo değişince ya da
  uyku sonrası kernel üyeliği düşürüyor, uygulama bunu fark etmiyordu.
- Peer ömrü 8 sn'ydi; Wi-Fi'da çoklu yayın paketleri güç tasarrufu yüzünden sık
  kayboluyor, cihazlar yanıp sönüyordu. Yayın yalnızca genel 255.255.255.255'e
  gidiyordu (çoklu arayüzlü makinelerde yanlış arayüzden çıkabilir).
- Çoklu yayın/broadcast'i filtreleyen ama cihazlar arası unicast'e izin veren
  kurumsal Wi-Fi'larda hiçbir yedek yol yoktu. TCP bağlantısı zaman aşımsızdı.

**Çözümler**
- Tek bir "supervisor" keşif motoru: arayüz kümesi değişince, sistem uykudan
  dönünce, uzun süre paket gelmeyince ya da alım hatası sürünce soketi otomatik
  baştan kurar. Bind hatası girişi engellemez; arka planda yeniden dener.
- SO_REUSEADDR/SO_REUSEPORT + standart port kullanılamazsa yedek (rastgele) port.
- Her arayüzden multicast + o arayüzün yönlendirilmiş broadcast'i + bilinen
  cihazlara doğrudan unicast heartbeat. Peer ömrü 20 sn.
- Otomatik alt ağ taraması (çoklu yayın engelli ağlar için): yeni bağlantıda,
  hiç cihaz yokken her 30 sn'de ve "Ağı Tara"da.
- Peer'ın duyurduğu IP'ler arasından BİZİM alt ağımızdaki seçilir (VPN/sanal
  adaptör IP'sine gönderme sorunu).
- "Manuel IP ile Bağlan" artık kalıcı: sabitlenen IP'ye sürekli heartbeat gider,
  uygulama yeniden açılınca hatırlanır.
- Başlıkta tıklanabilir durum noktası + **Ağ Tanılama** paneli: bind hatası,
  arayüzler, paket sayaçları, güvenlik duvarı/izolasyon ipuçları, "Bağlantıyı
  Sıfırla", sonucu kopyalama.
- Dosya alma (TCP) servisi bind hatasında artık yeniden denenir; gönderimde
  bağlantı (5 sn) ve el sıkışma (10 sn) zaman aşımı, anlaşılır hata mesajı.
- Ağ geri gelince / pencere öne gelince arayüz kendiliğinden yeniden tarar.
- Protokol uyumluluğu korundu: v6.1.x ile v6.2.0 birbirini görür ve dosya
  gönderir (yeni `ips` alanı eski sürümlerce yok sayılır).

### v6.1.1 — Auto-update zinciri doğrulama
- İçerik değişikliği yok. v6.1.0 çalıştıran cihazların otomatik güncellemeyi
  doğru şekilde alıp almadığını doğrulamak için yayınlanan test sürümü
  (bkz. v6.1.0'a geçişteki tag çakışması sorunu).

### v6.1.0 — Ajans içi güvenlik ve güvenilirlik güncellemesi
> **Sürüm numarası notu:** Bu güncelleme önce `v5.1.0` olarak etiketlenmişti,
> ama `v5.1.0`–`v5.2.5` ve `v6.0.0` arası tag'ler geçmişte (Nisan ayında,
> artık `main`'de olmayan "Design Hub" özelliği döneminde) zaten kullanılmıştı
> — aynı tag'i tekrar kullanmak GitHub Release'inde eski/yeni dosyaların
> karışmasına (asset adı çakışması) yol açtı ve auto-update'in eski Hub'lı
> build'i dağıtmasına neden oldu. Çakışmayı kesin çözmek için hiç kullanılmamış
> `v6.1.0`'a atlandı. **Yeni bir sürüm çıkarırken önce `git tag -l` ile
> çakışma kontrolü yapılmalı.**
- **Uçtan uca şifreleme:** her transfer bağlantısında geçici X25519 anahtar
  değişimi + AES-256-GCM. Aynı ağdaki üçüncü bir cihaz artık trafiği dinleyip
  dosya içeriğini okuyamaz. (Not: kalıcı bir kimlik doğrulaması yok — sadece
  pasif dinlemeye karşı korur, "man-in-the-middle" senaryosuna karşı değil.)
- **Dosya bütünlüğü doğrulaması:** her dosya için SHA-256 checksum, alıcı
  tarafta karşılaştırılıyor; uyuşmazlıkta arayüzde uyarı gösteriliyor.
- **Üzerine yazma koruması:** hedefte aynı isimde farklı bir dosya varsa
  sessizce üzerine yazılmıyor, "(1)", "(2)" ekiyle yeni isim veriliyor.
- **Kaldığı yerden devam etme (resume):** yarım kalan bir transfer, hedefte
  eksik dosya bulunursa kaldığı yerden devam eder. Best-effort'tur — bütünlük
  yalnızca dosya tamamlandıktan sonra checksum ile doğrulanır.
- **Gelen isteklere 60 saniyelik zaman aşımı** — yanıtsız kalan istekler
  otomatik reddedilir, gönderen taraf artık sonsuza kadar beklemez.
- **MB/s hız göstergesi** transfer günlüğünde.
- **Yapılandırılabilir indirme klasörü** (Ayarlar panelinden değiştirilebilir,
  `%APPDATA%/verishare/settings.json`'da saklanır).
- **Keşif iyileştirmeleri:** çoklu ağ arayüzü desteği (VPN/Ethernet+Wi-Fi aynı
  anda açıkken), UDP broadcast fallback (multicast'in IGMP snooping/AP
  kısıtlamasıyla filtrelendiği kurumsal ağlar için), yeni cihaza anında
  unicast yanıt, ve "Manuel IP ile Bağlan" (multicast/broadcast'in tamamen
  engellendiği — AP client isolation — ağlarda tek çözüm).
- **macOS Wi-Fi adı tespiti** artık `networksetup` ile deneniyor (eskiden
  tamamen devre dışıydı).

**Bilinen sınırlamalar:**
- Şifreleme kimlik doğrulaması yok (sıfır-konfigürasyon eşleştirme
  hedeflendiği için) — sadece pasif dinlemeye karşı korur.
- Resume, diskteki kısmi dosyanın gerçekten aynı kaynağın devamı olduğunu
  varsayar; yanlışsa transfer tamamlandıktan sonra checksum uyarısıyla
  fark edilir, dosya otomatik silinmez.
- Bu sürüm önceki protokolle (v5.0.0 ve öncesi) uyumlu DEĞİL — tüm cihazların
  aynı anda güncellenmesi gerekir, aksi halde güncellenmemiş/güncellenmiş
  cihazlar arası transfer başarısız olur (otomatik güncelleyici bunu kısa
  sürede çözer).

### v4.2.4
- Arayüz metinleri "Ağdaki Kullanıcılar" olarak güncellendi.
- "VeriShare" markalaması tamamlandı.
- Otomatik güncelleme stabil hale getirildi.

---
*Geliştiren: [kaege35](https://github.com/kaege35)*
