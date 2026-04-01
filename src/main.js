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
  dropArea.addEventListener('dragover', (e) => {
    e.preventDefault();
    if (selectedUser) dropArea.classList.add('dragging');
  });

  dropArea.addEventListener('dragleave', () => dropArea.classList.remove('dragging'));

  dropArea.addEventListener('drop', async (e) => {
    e.preventDefault();
    dropArea.classList.remove('dragging');
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    
    // Klasör veya dosya aktarımı için Tauri API kullanılacak
    invoke('select_dropped_items', { 
      peer: selectedUser.id 
      // Daha sonra Native API ile implement edilecek
    });
  });

  document.getElementById('browse-btn').addEventListener('click', () => {
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Kişinin ağ adresi henüz algılanmadı', 'error'); return; }
    invoke('open_file_dialog', { peerIp: selectedUser.ip });
  });

  // Mock Tauri Listeners (Arkadan gelecek veriler)
  listen('peers-updated', (event) => {
    updateUserList(event.payload);
  });

  listen('transfer-event', async (event) => {
    toast(event.payload, 'success');
  
    // Bildirim yetkisini kontrol et
    let permissionGranted = await isPermissionGranted();
    if (!permissionGranted) {
      const permission = await requestPermission();
      permissionGranted = permission === 'granted';
    }
  
    // Gelen dosyalarda bildirim at
    if (permissionGranted) {
      sendNotification({
        title: 'EasyShare Aktarımı',
        body: event.payload
      });
    }
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
  if (status) { status.textContent = statusText; status.className = `log-status ${statusClass || ''}`; }
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
