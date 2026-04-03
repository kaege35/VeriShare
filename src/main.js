const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ─── STATE ───────────────────────────────────────────────
let myName = '';
let myId = null;
let selectedUser = null;
let logItems = {};
let logCount = 0;
let activeTransferId = null;

// ─── INIT ─────────────────────────────────────────────────
document.addEventListener("DOMContentLoaded", () => {
  document.getElementById('join-btn').addEventListener('click', joinNetwork);
  
  document.getElementById('name-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') joinNetwork();
  });

  const dropArea = document.getElementById('drop-area');
  
  listen('tauri://drag-enter', () => { if (selectedUser) dropArea.classList.add('dragging'); });
  listen('tauri://drag-over', () => { if (selectedUser) dropArea.classList.add('dragging'); });
  listen('tauri://drag-leave', () => dropArea.classList.remove('dragging'));

  listen('tauri://drag-drop', async (e) => {
    dropArea.classList.remove('dragging');
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Ağ adresi yok', 'error'); return; }
    
    const paths = e.payload.paths;
    if (!paths || paths.length === 0) return;
    
    invoke('send_paths_directly', { peerIp: selectedUser.ip, paths });
  });

  const { open } = window.__TAURI__.dialog;

  document.getElementById('browse-btn').addEventListener('click', async () => {
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Ağ adresi yok', 'error'); return; }
    
    try {
      const filePaths = await open({
        multiple: true,
        directory: false,
        title: "Dosyaları Seç",
      });
      
      if (!filePaths || filePaths.length === 0) return;
      invoke('send_paths_directly', { peerIp: selectedUser.ip, paths: filePaths });
    } catch(e) {
      toast('Dosya seçimi iptal edildi.', 'info');
    }
  });

  document.getElementById('refresh-btn').addEventListener('click', async () => {
    const btn = document.getElementById('refresh-btn');
    btn.classList.add('spinning');
    try {
      await invoke('scan_network');
      toast('Ağ taranıyor...', 'info');
    } catch(e) {
      toast('Tarama hatası: ' + e, 'error');
    }
    setTimeout(() => btn.classList.remove('spinning'), 1500);
  });

  document.getElementById('log-clear-btn').addEventListener('click', () => {
    const list = document.getElementById('log-list');
    const items = list.querySelectorAll('.log-item.done, .log-item.success, .log-item.error, .log-item.cancelled');
    items.forEach(el => {
      const peerId = el.dataset.transferId;
      if (peerId) delete logItems[peerId];
      el.remove();
      logCount = Math.max(0, logCount - 1);
    });
    document.getElementById('log-count').textContent = logCount;
  });

  listen('transfer-request', (event) => {
    const p = event.payload;
    showModal(p.id, p.total_files, p.total_size);
  });

  listen('transfer-event', (event) => {
    const msg = event.payload;
    if (msg.includes("ERİŞİM_REDDEDİLDİ")) {
      toast('Karşı taraf aktarımı reddetti', 'error');
      // Update UI if we had activeTransferId
      if (activeTransferId) {
        updateLog(activeTransferId, 'Reddedildi', 'error', 0);
      }
    } else if (msg.includes("iptal") || msg.includes("İPTAL")) {
      toast(msg, 'info');
    } else {
      toast(msg, 'error');
    }
  });

  listen('transfer-id-assigned', (event) => {
    activeTransferId = event.payload.transfer_id;
  });

  listen('transfer-initiated', (event) => {
    const { transfer_id, text, dir } = event.payload;
    const statusText = dir === 'out' ? 'Karşı tarafın onayı bekleniyor...' : 'Başlıyor...';
    addLog(transfer_id, text, dir, statusText);
    if (dir === 'out') { activeTransferId = transfer_id; }
  });

  listen('transfer-progress', (event) => {
    const { id, pct, text, is_done, cancelled, path } = event.payload;
    if (cancelled) {
      updateLog(id, 'İptal Edildi', 'cancelled', pct);
      toast('Alım iptal edildi', 'info');
      return;
    }
    if (is_done) {
      updateLog(id, 'Tamamlandı', 'done', 100, path);
      toast(text + ' indirildi!', 'success');
    } else {
      updateLog(id, `%${pct}`, '', pct);
    }
  });

  listen('transfer-out-progress', (event) => {
    const { id, pct, text, is_done, cancelled } = event.payload;
    if (cancelled) {
      updateLog(id, 'İptal Edildi', 'cancelled', pct);
      toast('Gönderim iptal edildi', 'info');
      return;
    }
    if (is_done) {
      updateLog(id, 'İletildi', 'success', 100);
      toast(text + ' gönderildi!', 'success');
    } else {
      updateLog(id, `%${pct}`, '', pct);
    }
  });

  listen('peers-updated', (event) => {
    updateUserList(event.payload);
  });

  listen('update-available', (event) => {
    const version = event.payload;
    showUpdateBanner(version);
  });

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

function showUpdateBanner(version) {
  const existing = document.getElementById('update-banner');
  if (existing) existing.remove();

  const banner = document.createElement('div');
  banner.id = 'update-banner';
  banner.innerHTML = `
    <span>🚀 <strong>VeriShare v${version}</strong> mevcut!</span>
    <button onclick="doUpdate()">Hemen Güncelle</button>
    <button onclick="this.parentElement.remove()" style="background:transparent;border:none;color:inherit;cursor:pointer;margin-left:4px;font-size:16px;">✕</button>
  `;
  document.body.appendChild(banner);
}

window.doUpdate = async () => {
  const btn = document.querySelector('#update-banner button');
  if (btn) {
    btn.textContent = 'İndiriliyor ve Kuruluyor...';
    btn.disabled = true;
  }
  
  try {
    await invoke('install_update');
  } catch(e) {
    toast('Güncelleme hatası: ' + e, 'error');
    if (btn) {
      btn.textContent = 'Hemen Güncelle';
      btn.disabled = false;
    }
  }
};

let currentTransferId = null;

function showModal(transferId, count, size) {
  currentTransferId = transferId;
  const overlay = document.getElementById('incoming-modal');
  document.getElementById('modal-file-name').textContent = `${count} adet içerik`;
  document.getElementById('modal-file-meta').textContent = formatSize(size);
  overlay.classList.add('visible');
}

// ─── LOGIN ────────────────────────────────────────────────
async function joinNetwork() {
  const name = document.getElementById('name-input').value.trim();
  if (!name) return;
  myName = name;
  
  try {
    myId = await invoke('start_discovery', { name });
    
    document.getElementById('login-screen').style.display = 'none';
    document.getElementById('app').classList.add('visible');
    document.getElementById('header-name').textContent = myName;

    fetchWifiSSID();
  } catch(e) {
    toast('Ağa katılma hatası: ' + e, 'error');
  }
}

async function fetchWifiSSID() {
  try {
    const ssid = await invoke('get_wifi_ssid');
    const el = document.getElementById('wifi-name');
    if (ssid && el) {
      el.textContent = ssid;
    }
  } catch(e) {
    console.log('WiFi SSID alınamadı:', e);
  }
}

// ─── KULLANICI LİSTESİ ───────────────────────────────────
function updateUserList(users) {
  const list = document.getElementById('user-list');
  const count = document.getElementById('online-count');
  
  const otherUsers = users.filter(u => u.id !== myId);
  count.textContent = otherUsers.length;
  list.innerHTML = '';
  
  users.forEach(u => {
    const isSelf = u.id === myId;
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
    const oldName = selectedUser.name;
    selectedUser = null;
    showDropUI(false);
    toast(`${oldName} ağdan ayrıldı`, 'info');
  }
}

function selectUser(user) {
  selectedUser = user;
  document.getElementById('drop-target-name').textContent = user.name + ' cihazına gönder';
  showDropUI(true);
  document.querySelectorAll('.user-item').forEach(el => el.classList.remove('selected'));
  document.querySelectorAll('.user-item').forEach(el => {
    if (el.querySelector('.user-name')?.textContent.startsWith(user.name)) {
      el.classList.add('selected');
    }
  });
}

function showDropUI(show) {
  document.getElementById('no-target').style.display = show ? 'none' : 'flex';
  document.getElementById('drop-target-ui').style.display = show ? 'flex' : 'none';
}

// ─── LOG ─────────────────────────────────────────────────
function addLog(transferId, fileName, direction, statusText) {
  if (logItems[transferId]) return; // Zaten varsa tekrar ekleme

  const list = document.getElementById('log-list');
  logItems[transferId] = transferId;
  logCount++;
  document.getElementById('log-count').textContent = logCount;

  const dirIcon = direction === 'out' 
    ? '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="17 11 12 6 7 11"/><line x1="12" y1="18" x2="12" y2="6"/></svg>'
    : '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="7 13 12 18 17 13"/><line x1="12" y1="6" x2="12" y2="18"/></svg>';
  
  const dirClass = direction === 'out' ? 'log-dir-out' : 'log-dir-in';

  const el = document.createElement('div');
  el.className = 'log-item';
  el.id = 'log-' + transferId;
  el.dataset.transferId = transferId;
  el.innerHTML = `
    <div style="display:flex; align-items:center; width:100%; gap:12px;">
      <div class="log-icon ${dirClass}">${dirIcon}</div>
      <div class="log-text"><strong>${fileName}</strong></div>
      <div class="log-progress"><div class="log-progress-fill" style="width:0%"></div></div>
      <div class="log-status">${statusText}</div>
      <button class="log-cancel-btn" title="İptal Et" onclick="cancelTransfer('${transferId}')">✕</button>
    </div>
    <div class="log-actions" style="display:none;" id="actions-${transferId}">
      <button class="log-action-btn" onclick="openPath('${transferId}')">Aç</button>
      <button class="log-action-btn" onclick="showInFolder('${transferId}')">Klasörde Göster</button>
    </div>
  `;
  list.prepend(el);
  list.scrollTop = 0;
}

function updateLog(transferId, statusText, statusClass, pct, savedPath) {
  const el = document.getElementById('log-' + transferId);
  if (!el) return;
  const status = el.querySelector('.log-status');
  const fill = el.querySelector('.log-progress-fill');
  const cancelBtn = el.querySelector('.log-cancel-btn');
  const actions = el.querySelector('#actions-' + transferId);
  
  if (status) { 
    status.textContent = statusText; 
    status.className = `log-status ${statusClass || ''}`; 
  }
  if (statusClass) el.classList.add(statusClass);
  if (fill && pct !== undefined) fill.style.width = pct + '%';
  
  if (statusClass === 'done' || statusClass === 'success' || statusClass === 'cancelled' || statusClass === 'error') {
    if (cancelBtn) cancelBtn.style.display = 'none';
  }

  if (statusClass === 'done' && savedPath) {
    el.style.flexDirection = 'column';
    if (actions) {
      actions.style.display = 'flex';
      el.dataset.savedPath = savedPath; // Tıklanınca açmak için saklıyoruz
    }
  }
}

window.cancelTransfer = async (transferId) => {
  try {
    await invoke('cancel_transfer', { id: transferId });
    updateLog(transferId, 'İptal Edildi', 'cancelled', undefined);
    toast('Transfer iptal edildi', 'info');
  } catch(e) {
    console.error('İptal hatası:', e);
  }
};

window.openPath = async (transferId) => {
  const el = document.getElementById('log-' + transferId);
  if(el && el.dataset.savedPath) invoke('open_file', { path: el.dataset.savedPath });
};

window.showInFolder = async (transferId) => {
  const el = document.getElementById('log-' + transferId);
  if(el && el.dataset.savedPath) invoke('show_in_folder', { path: el.dataset.savedPath });
};

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
  if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
  return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
}
