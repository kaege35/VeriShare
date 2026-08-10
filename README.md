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

### v5.1.0 — Ajans içi güvenlik ve güvenilirlik güncellemesi
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
