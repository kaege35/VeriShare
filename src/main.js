const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { isPermissionGranted, requestPermission, sendNotification } = window.__TAURI__.notification;

// ─── STATE ───────────────────────────────────────────────
let myName = '';
let selectedUser = null;
let pendingTransfer = null; 
let logItems = {};

// ─── INIT ─────────────────────────────────────────────────
document.addEventListener("DOMContentLoaded", () => {
  // Event Listeners
  document.getElementById('join-btn').addEventListener('click', joinNetwork);
  
  document.getElementById('name-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') joinNetwork();
  });

  const dropArea = document.getElementById('drop-area');
  
  // Sadece CSS animasyonlari icin native window
  listen('tauri://drag-enter', () => { if (selectedUser) dropArea.classList.add('dragging'); });
  listen('tauri://drag-leave', () => dropArea.classList.remove('dragging'));

  listen('tauri://drop', async (e) => {
    dropArea.classList.remove('dragging');
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Ağ adresi yok', 'error'); return; }
    
    const paths = e.payload.paths; // Tauri native full path array
    if (!paths || paths.length === 0) return;
    
    // Rust arka ucuna direkt gönder
    invoke('send_paths_directly', { peerIp: selectedUser.ip, paths });
  });

  document.getElementById('browse-btn').addEventListener('click', () => {
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Ağ adresi yok', 'error'); return; }
    invoke('open_file_dialog', { peerIp: selectedUser.ip });
  });

  // Gelen onay istekleri için dinleyici
  listen('transfer-request', (event) => {
    const p = event.payload;
    showModal(p.id, p.total_files, p.total_size);
    notifyOS('EasyShare: Gelen İstek', `${p.total_files} adet dosya gönderilmek isteniyor.`);
  });

  // Genel hata ve durum bildirimleri (Örn: Reddetme)
  listen('transfer-event', (event) => {
    const msg = event.payload;
    if (msg.includes("ERİŞİM_REDDEDİLDİ")) {
      // En son out log'unu bul ve "Reddedildi" olarak işaretle
      for (let peerId in logItems) {
        if (logItems[peerId].startsWith('log-out-')) { // Bu mantık geliştirilebilir ama şimdilik yeterli
           updateLog(peerId, 'Reddedildi', 'error', 0);
        }
      }
      toast('Karşı taraf aktarımı reddetti', 'error');
    } else {
      toast(msg, 'error');
    }
  });

  // Anlık Yüzde Barları için progress dinleyici
  listen('transfer-progress', (event) => {
    const { id, pct, text, is_done } = event.payload;
    if (pct === 0 && text) {
      addLog(id, text, 'in', 'Başlıyor...');
    } else if (is_done) {
      updateLog(id, 'Tamamlandı', 'done', 100);
      toast(text || 'Transfer bitti!', 'success');
      notifyOS('EasyShare İndirmesi', text || 'Dosyalar başarıyla indirildi.');
    } else {
      updateLog(id, `%${pct}`, '', pct);
    }
  });

  listen('transfer-out-progress', (event) => {
    const { id, pct, text, is_done } = event.payload;
    if (pct === 0 && text) {
      addLog(id, text, 'out', 'Gönderiliyor...');
    } else if (is_done) {
      updateLog(id, 'İletildi', 'success', 100);
      toast(text || 'Gönderim tamamlandı!', 'success');
    } else {
      updateLog(id, `%${pct}`, '', pct);
    }
  });

  // Ağ cihazları güncelleme listener'ı
  listen('peers-updated', (event) => {
    updateUserList(event.payload);
  });

  // Otomatik güncelleme bildirimi
  listen('update-available', (event) => {
    const version = event.payload;
    showUpdateBanner(version);
  });
});

async function notifyOS(title, body) {
  let permissionGranted = await isPermissionGranted();
  if (!permissionGranted) {
    const permission = await requestPermission();
    permissionGranted = permission === 'granted';
  }
  if (permissionGranted) sendNotification({ title, body });
}

function showUpdateBanner(version) {
  // Varsa önce eski banner'ı kaldır
  const existing = document.getElementById('update-banner');
  if (existing) existing.remove();

  const banner = document.createElement('div');
  banner.id = 'update-banner';
  banner.innerHTML = `
    <span>🚀 <strong>EasyShare v${version}</strong> mevcut!</span>
    <button onclick="doUpdate()">Hemen Güncelle</button>
    <button onclick="this.parentElement.remove()" style="background:transparent;border:none;color:inherit;cursor:pointer;margin-left:4px;font-size:16px;">✕</button>
  `;
  document.body.appendChild(banner);
}

window.doUpdate = async () => {
  const { relaunch } = window.__TAURI__.process;
  await invoke('install_update');
  await relaunch();
};

let currentTransferId = null;

function showModal(transferId, count, size) {
  currentTransferId = transferId;
  const overlay = document.getElementById('incoming-modal');
  document.getElementById('modal-file-name').textContent = `${count} adet dosya/klasör`;
  document.getElementById('modal-file-meta').textContent = formatSize(size);
  overlay.classList.add('visible');
}

document.addEventListener("DOMContentLoaded", () => {
  const acceptBtn = document.getElementById('accept-btn');
  if(acceptBtn) acceptBtn.addEventListener('click', () => {
    document.getElementById('incoming-modal').classList.remove('visible');
    if (currentTransferId) invoke('respond_to_transfer', { id: currentTransferId, accept: true });
  });

  const declineBtn = document.getElementById('decline-btn');
  if(declineBtn) declineBtn.addEventListener('click', () => {
    document.getElementById('incoming-modal').classList.remove('visible');
    if (currentTransferId) invoke('respond_to_transfer', { id: currentTransferId, accept: false });
  });
});

// ─── LOGIN ────────────────────────────────────────────────
async function joinNetwork() {
  const name = document.getElementById('name-input').value.trim();
  if (!name) return;
  myName = name;
  
  try {
    // Rust arka ucuna isim gönderip mDNS/Multicast başlatılacak
    await invoke('start_discovery', { name });
    
    document.getElementById('login-screen').style.display = 'none';
    document.getElementById('app').classList.add('visible');
    document.getElementById('header-name').textContent = myName;
  } catch(e) {
    toast('Ağa katılma hatası: ' + e, 'error');
  }
}

// ─── KULLANICI LİSTESİ ───────────────────────────────────
function updateUserList(users) {
  const list = document.getElementById('user-list');
  const count = document.getElementById('online-count');
  
  // kendimiz hariç
  const otherUsers = users.filter(u => u.name !== myName);
  count.textContent = otherUsers.length;
  list.innerHTML = '';
  
  users.forEach(u => {
    const isSelf = u.name === myName;
    const isSelected = selectedUser && selectedUser.id === u.id;
    const el = document.createElement('div');
    el.className = `user-item${isSelf ? ' self' : ''}${isSelected ? ' selected' : ''}`;
    
    const avatar = u.name.slice(0, 2).toUpperCase();

    el.innerHTML = `
      <div class="avatar">${avatar}</div>
      <div class="user-info">
        <div class="user-name">${u.name}${isSelf ? ' (sen)' : ''}</div>
        <div class="user-status">● çevrimiçi</div>
      </div>
      ${!isSelf ? '<div class="send-badge">GÖNDER →</div>' : ''}
    `;
    
    if (!isSelf) el.onclick = () => selectUser(u);
    list.appendChild(el);
  });

  if (selectedUser && !users.find(u => u.id === selectedUser.id)) {
    selectedUser = null;
    showDropUI(false);
    toast(`${selectedUser?.name || 'Kişi'} ağdan ayrıldı`, 'error');
  }
}

function selectUser(user) {
  selectedUser = user;
  document.getElementById('drop-target-name').textContent = user.name + ' →';
  showDropUI(true);
  document.querySelectorAll('.user-item').forEach(el => el.classList.remove('selected'));
  document.querySelectorAll('.user-item').forEach(el => {
    if (el.querySelector('.user-name')?.textContent.startsWith(user.name)) {
      el.classList.add('selected');
    }
  });
}

function showDropUI(show) {
  document.getElementById('no-target').style.display = show ? 'none' : 'block';
  document.getElementById('drop-target-ui').style.display = show ? 'flex' : 'none';
}

// ─── LOG ─────────────────────────────────────────────────
function addLog(peerId, fileName, direction, statusText) {
  const list = document.getElementById('log-list');
  const id = `log-${peerId}-${Date.now()}`;
  logItems[peerId] = id;
  const icon = direction === 'out' ? '⬆' : '⬇';
  const el = document.createElement('div');
  el.className = 'log-item';
  el.id = id;
  el.innerHTML = `
    <div class="log-icon">${icon}</div>
    <div class="log-text"><strong>${fileName}</strong></div>
    <div class="log-progress"><div class="log-progress-fill" style="width:0%"></div></div>
    <div class="log-status">${statusText}</div>
  `;
  list.prepend(el);
  list.scrollTop = 0;
}

function updateLog(peerId, statusText, statusClass, pct) {
  const id = logItems[peerId];
  if (!id) return;
  const el = document.getElementById(id);
  if (!el) return;
  const status = el.querySelector('.log-status');
  const fill = el.querySelector('.log-progress-fill');
  if (status) { 
    status.textContent = statusText; 
    status.className = `log-status ${statusClass || ''}`; 
  }
  if (statusClass) el.classList.add(statusClass);
  if (fill && pct !== undefined) fill.style.width = pct + '%';
}

// ─── TOAST ───────────────────────────────────────────────
function toast(msg, type = 'info') {
  const c = document.getElementById('toast-container');
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = msg;
  c.appendChild(el);
  setTimeout(() => el.remove(), 3500);
}

function formatSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
}
