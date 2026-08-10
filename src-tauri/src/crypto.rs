// ─── UÇTAN UCA ŞİFRELİ KANAL ───────────────────────────────────────────────
//
// Neden: Eskiden TCP bağlantısı düz metin (plaintext) çalışıyordu — aynı
// Wi-Fi/LAN'daki başka bir cihaz trafiği dinleyip (paket koklama) dosya
// içeriğini ve dosya adlarını okuyabilirdi. Ajans ortamında müşteri
// dosyaları taşındığı için bu kabul edilemez.
//
// Yöntem: Her TCP bağlantısında iki taraf da geçici (ephemeral) bir X25519
// anahtar çifti üretir, açık anahtarlarını değişir, Diffie-Hellman ile ortak
// bir sır türetir ve bunu SHA-256'dan geçirerek AES-256-GCM oturum anahtarı
// elde eder. Anahtarlar bağlantı kapanınca yok olur (forward secrecy) —
// kalıcı bir kimlik/parola olmadığı için "kimlik doğrulama" sağlamaz, sadece
// pasif dinlemeye (eavesdropping) karşı korur. Bu, LAN'da AirDrop benzeri
// bir araç için makul bir tehdit modelidir; sıfır-konfigürasyon eşleştirme
// isteniyorsa PIN/QR ile kimlik doğrulama ayrı bir iyileştirme olarak
// eklenebilir.
//
// Çerçeveleme: Önceden kontrol mesajları 4-byte uzunluk + JSON, dosya
// baytları ise çerçevesiz ham akış olarak gönderiliyordu. Artık HER ŞEY
// (kontrol mesajları + dosya baytları) aynı şifreli çerçeve biçimini
// kullanıyor: [4 byte BE uzunluk][AES-GCM şifreli veri + 16 byte etiket].
// Dosya baytları CHUNK_SIZE'lık parçalara bölünüp ayrı ayrı şifreleniyor.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use x25519_dalek::{x25519, X25519_BASEPOINT_BYTES};

/// Dosya baytları bu boyutta parçalara bölünüp her biri ayrı ayrı şifrelenir.
/// Çok büyük tutulursa bellek kullanımı artar, çok küçük tutulursa şifreleme
/// başlığı (nonce+tag) oranı yükselir. 4MB, LAN hızında iyi bir denge.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 4;
// Tek bir çerçevenin üst sınırı: kontrol mesajları küçük, dosya parçaları en
// fazla CHUNK_SIZE — GCM etiketi (16 byte) dahil biraz pay bırakıyoruz.
const MAX_FRAME_LEN: usize = CHUNK_SIZE + 1024;

pub struct SecureStream {
    stream: TcpStream,
    cipher: Aes256Gcm,
    send_salt: [u8; SALT_LEN],
    recv_salt: [u8; SALT_LEN],
    send_counter: u64,
    recv_counter: u64,
}

fn gen_keypair() -> ([u8; 32], [u8; 32]) {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let public = x25519(secret, X25519_BASEPOINT_BYTES);
    (secret, public)
}

fn derive_key(shared_secret: &[u8; 32]) -> [u8; 32] {
    // Ham DH çıktısını doğrudan AES anahtarı olarak kullanmak yerine
    // SHA-256'dan geçiriyoruz — böylece anahtar, alan/gruba özgü zayıf
    // noktalardan (ör. düşük entropili nadir DH çıktıları) bağımsız olarak
    // düzgün dağılmış 256 bit'e sahip olur.
    let mut hasher = Sha256::new();
    hasher.update(b"verishare-v1-session-key");
    hasher.update(shared_secret);
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

impl SecureStream {
    fn new(
        stream: TcpStream,
        key: [u8; 32],
        send_salt: [u8; SALT_LEN],
        recv_salt: [u8; SALT_LEN],
    ) -> Self {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        Self {
            stream,
            cipher,
            send_salt,
            recv_salt,
            send_counter: 0,
            recv_counter: 0,
        }
    }

    /// Bağlantıyı açan taraf (giden transfer) bunu çağırır.
    /// Sıra: yaz(public) → oku(public) → yaz(salt) → oku(salt)
    pub async fn handshake_initiator(mut stream: TcpStream) -> std::io::Result<Self> {
        let (my_secret, my_public) = gen_keypair();
        stream.write_all(&my_public).await?;

        let mut their_public = [0u8; 32];
        stream.read_exact(&mut their_public).await?;

        let mut my_salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut my_salt);
        stream.write_all(&my_salt).await?;

        let mut their_salt = [0u8; SALT_LEN];
        stream.read_exact(&mut their_salt).await?;

        let shared = x25519(my_secret, their_public);
        let key = derive_key(&shared);
        Ok(Self::new(stream, key, my_salt, their_salt))
    }

    /// Bağlantıyı kabul eden taraf (gelen transfer sunucusu) bunu çağırır.
    /// Sıra: oku(public) → yaz(public) → oku(salt) → yaz(salt)
    /// (initiator ile karşılıklı kilitlenmeyi önlemek için adımlar ters.)
    pub async fn handshake_responder(mut stream: TcpStream) -> std::io::Result<Self> {
        let mut their_public = [0u8; 32];
        stream.read_exact(&mut their_public).await?;

        let (my_secret, my_public) = gen_keypair();
        stream.write_all(&my_public).await?;

        let mut their_salt = [0u8; SALT_LEN];
        stream.read_exact(&mut their_salt).await?;

        let mut my_salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut my_salt);
        stream.write_all(&my_salt).await?;

        let shared = x25519(my_secret, their_public);
        let key = derive_key(&shared);
        Ok(Self::new(stream, key, my_salt, their_salt))
    }

    fn build_nonce(salt: &[u8; SALT_LEN], counter: u64) -> [u8; NONCE_LEN] {
        // Nonce = 4 byte rastgele oturum tuzu + 8 byte artan sayaç.
        // Tuz, iki yönün (gönderim/alım) aynı anahtarı paylaşsa bile nonce
        // uzayının çakışmamasını sağlar; sayaç her çerçevede bir artar ve
        // bu bağlantı ömrü boyunca asla tekrarlanmaz (2^64 çerçeve pratikte
        // ulaşılamaz bir sınır).
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..SALT_LEN].copy_from_slice(salt);
        nonce[SALT_LEN..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    /// Tek bir düz-metin parçasını şifreleyip [uzunluk][şifreli veri] olarak yazar.
    pub async fn write_frame(&mut self, plaintext: &[u8]) -> std::io::Result<()> {
        let nonce_bytes = Self::build_nonce(&self.send_salt, self.send_counter);
        self.send_counter += 1;

        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "şifreleme hatası"))?;

        self.stream
            .write_all(&(ciphertext.len() as u32).to_be_bytes())
            .await?;
        self.stream.write_all(&ciphertext).await?;
        Ok(())
    }

    /// Bir çerçeve okuyup çözer. Bağlantı düzgün kapandıysa `Ok(None)` döner.
    pub async fn read_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        let n = self.stream.read(&mut len_buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if n < 4 {
            self.stream.read_exact(&mut len_buf[n..]).await?;
        }

        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "çerçeve boyutu sınırı aşıldı",
            ));
        }

        let mut ciphertext = vec![0u8; len];
        self.stream.read_exact(&mut ciphertext).await?;

        let nonce_bytes = Self::build_nonce(&self.recv_salt, self.recv_counter);
        self.recv_counter += 1;

        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "şifre çözme başarısız — bağlantı bozulmuş veya bütünlük ihlali",
                )
            })?;

        Ok(Some(plaintext))
    }

    /// `set_nodelay` gibi alttaki soketi yapılandırmak için.
    pub fn set_nodelay(&self, nodelay: bool) -> std::io::Result<()> {
        self.stream.set_nodelay(nodelay)
    }
}
