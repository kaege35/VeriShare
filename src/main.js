const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ─── STATE ───────────────────────────────────────────────
let myName = '';
let myId = null;
let selectedUser = null;
let logItems = {};          // transferId → true  (duplicate koruması)
let logCount = 0;
let activeTransferId = null;
let lastUsers = [];
let searchQuery = '';

// ─── INIT ─────────────────────────────────────────────────
document.addEventListener("DOMContentLoaded", () => {
  document.getElementById('join-btn').addEventListener('click', joinNetwork);
  document.getElementById('name-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') joinNetwork();
  });

  // ── Sürükle‑Bırak ──────────────────────────────────────
  const dropArea = document.getElementById('drop-area');
  listen('tauri://drag-enter', () => { if (selectedUser) dropArea.classList.add('dragging'); });
  listen('tauri://drag-over',  () => { if (selectedUser) dropArea.classList.add('dragging'); });
  listen('tauri://drag-leave', () => dropArea.classList.remove('dragging'));

  listen('tauri://drag-drop', async (e) => {
    dropArea.classList.remove('dragging');
    if (!selectedUser)     { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip)  { toast('Ağ adresi yok', 'error'); return; }
    const paths = e.payload.paths;
    if (!paths || paths.length === 0) return;
    invoke('send_paths_directly', { peerIp: selectedUser.ip, paths });
  });

  // ── Dosya Seç butonu ────────────────────────────────────
  const { open } = window.__TAURI__.dialog;
  document.getElementById('browse-btn').addEventListener('click', async () => {
    if (!selectedUser)    { toast('Önce bir kişi seç', 'error'); return; }
    if (!selectedUser.ip) { toast('Ağ adresi yok', 'error'); return; }
    try {
      const filePaths = await open({ multiple: true, directory: false, title: 'Dosyaları Seç' });
      if (!filePaths || filePaths.length === 0) return;
      invoke('send_paths_directly', { peerIp: selectedUser.ip, paths: filePaths });
    } catch {
      toast('Dosya seçimi iptal edildi.', 'info');
    }
  });

  // ── Ağ Tara ─────────────────────────────────────────────
  document.getElementById('refresh-btn').addEventListener('click', async () => {
    const btn = document.getElementById('refresh-btn');
    btn.classList.add('spinning');
    try {
      await invoke('scan_network');
      toast('Ağ taranıyor...', 'info');
    } catch (e) {
      toast('Tarama hatası: ' + e, 'error');
    }
    setTimeout(() => btn.classList.remove('spinning'), 1500);
  });

  // ── Kullanıcı Arama ─────────────────────────────────────
  document.getElementById('user-search').addEventListener('input', (e) => {
    searchQuery = e.target.value.toLowerCase();
    renderUserList();
  });

  // ── Log Temizle ─────────────────────────────────────────
  document.getElementById('log-clear-btn').addEventListener('click', () => {
    const list = document.getElementById('log-list');
    const done = list.querySelectorAll(
      '.log-item.done, .log-item.success, .log-item.error, .log-item.cancelled'
    );
    done.forEach(el => {
      const id = el.dataset.transferId;
      if (id) delete logItems[id];
      el.remove();
    });
    logCount = list.children.length;
    document.getElementById('log-count').textContent = logCount;
  });

  // ── Tauri Event Dinleyicileri ────────────────────────────

  // Gelen transfer isteği — modal göster
  listen('transfer-request', (event) => {
    const { id, total_files, total_size } = event.payload;
    showModal(id, total_files, total_size);
  });

  // Genel hata/iptal mesajı
  listen('transfer-event', (event) => {
    const msg = event.payload;
    if (msg.includes('ERİŞİM_REDDEDİLDİ')) {
      toast('Karşı taraf aktarımı reddetti', 'error');
      if (activeTransferId) {
        updateLog(activeTransferId, 'Reddedildi', 'error', 0, null);
      }
    } else if (msg.includes('iptal') || msg.includes('İPTAL')) {
      toast(msg, 'info');
    } else {
      toast(msg, 'error');
    }
  });

  // Gönderici transfer ID ataması (kabul sonrası)
  listen('transfer-id-assigned', (event) => {
    activeTransferId = event.payload.transfer_id;
  });

  // Transfer başladı (her iki yön)
  listen('transfer-initiated', (event) => {
    const { transfer_id, text, dir } = event.payload;
    addLog(transfer_id, text, dir, dir === 'out' ? 'Onay bekleniyor...' : 'Başlıyor...');
    if (dir === 'out') activeTransferId = transfer_id;
  });

  // Alım ilerlemesi
  listen('transfer-progress', (event) => {
    const { id, pct, text, is_done, cancelled, path } = event.payload;
    if (cancelled) {
      updateLog(id, 'İptal Edildi', 'cancelled', pct, null);
      toast('Alım iptal edildi', 'info');
      return;
    }
    if (is_done) {
      updateLog(id, 'Tamamlandı', 'done', 100, path || null);
      toast(`${text} indirildi!`, 'success');
    } else {
      updateLog(id, `%${pct}`, '', pct, null);
    }
  });

  // Gönderim ilerlemesi
  listen('transfer-out-progress', (event) => {
    const { id, pct, text, is_done, cancelled } = event.payload;
    if (cancelled) {
      updateLog(id, 'İptal Edildi', 'cancelled', pct, null);
      toast('Gönderim iptal edildi', 'info');
      return;
    }
    if (is_done) {
      updateLog(id, 'İletildi', 'success', 100, null);
      toast(`${text} gönderildi!`, 'success');
    } else {
      updateLog(id, `%${pct}`, '', pct, null);
    }
  });

  // Kullanıcı listesi güncellendi
  listen('peers-updated', (event) => {
    updateUserList(event.payload);
  });

  // Güncelleme mevcut
  listen('update-available', (event) => {
    showUpdateBanner(event.payload);
  });

  // Modal butonları
  document.getElementById('accept-btn').addEventListener('click', () => {
    document.getElementById('incoming-modal').classList.remove('visible');
    if (currentTransferId) invoke('respond_to_transfer', { id: currentTransferId, accept: true });
  });
  document.getElementById('decline-btn').addEventListener('click', () => {
    document.getElementById('incoming-modal').classList.remove('visible');
    if (currentTransferId) invoke('respond_to_transfer', { id: currentTransferId, accept: false });
  });
});

// ─── GÜNCELLEME ──────────────────────────────────────────
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
  if (btn) { btn.textContent = 'İndiriliyor...'; btn.disabled = true; }
  try {
    await invoke('install_update');
  } catch (e) {
    toast('Güncelleme hatası: ' + e, 'error');
    if (btn) { btn.textContent = 'Hemen Güncelle'; btn.disabled = false; }
  }
};

// ─── MODAL ────────────────────────────────────────────────
let currentTransferId = null;

function showModal(transferId, count, size) {
  currentTransferId = transferId;
  document.getElementById('modal-file-name').textContent = `${count} adet içerik`;
  document.getElementById('modal-file-meta').textContent = formatSize(size);
  document.getElementById('incoming-modal').classList.add('visible');
}

// ─── GİRİŞ ────────────────────────────────────────────────
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
  } catch (e) {
    toast('Ağa katılma hatası: ' + e, 'error');
  }
}

async function fetchWifiSSID() {
  try {
    const ssid = await invoke('get_wifi_ssid');
    const el = document.getElementById('wifi-name');
    if (ssid && el) el.textContent = ssid;
  } catch {
    // macOS Gizlilik koruması vb. — sessizce görmezden gel
  }
}

// ─── KULLANICI LİSTESİ ───────────────────────────────────
function updateUserList(users) {
  lastUsers = users;
  renderUserList();
}

function renderUserList() {
  const list  = document.getElementById('user-list');
  const count = document.getElementById('online-count');

  const otherUsers    = lastUsers.filter(u => u.id !== myId);
  count.textContent   = otherUsers.length;
  list.innerHTML      = '';

  const filtered = lastUsers.filter(u =>
    u.name.toLowerCase().includes(searchQuery)
  );

  filtered.forEach(u => {
    const isSelf     = u.id === myId;
    const isSelected = selectedUser && selectedUser.id === u.id;
    const el = document.createElement('div');
    el.className = `user-item${isSelf ? ' self' : ''}${isSelected ? ' selected' : ''}`;

    el.innerHTML = `
      <div class="avatar">${u.name.slice(0, 2).toUpperCase()}</div>
      <div class="user-info">
        <div class="user-name">${u.name}${isSelf ? ' (sen)' : ''}</div>
        <div class="user-status">● çevrimiçi</div>
      </div>
      ${!isSelf ? '<div class="send-badge">GÖNDER →</div>' : ''}
    `;

    if (!isSelf) el.onclick = () => selectUser(u);
    list.appendChild(el);
  });

  // Seçili kullanıcı ağdan çıktıysa temizle
  if (selectedUser && !lastUsers.find(u => u.id === selectedUser.id)) {
    const name = selectedUser.name;
    selectedUser = null;
    showDropUI(false);
    toast(`${name} ağdan ayrıldı`, 'info');
  }
}

function selectUser(user) {
  selectedUser = user;
  document.getElementById('drop-target-name').textContent = `${user.name} cihazına gönder`;
  showDropUI(true);
  document.querySelectorAll('.user-item').forEach(el => {
    const nameEl = el.querySelector('.user-name');
    el.classList.toggle('selected', nameEl?.textContent.startsWith(user.name) ?? false);
  });
}

function showDropUI(show) {
  document.getElementById('no-target').style.display    = show ? 'none' : 'flex';
  document.getElementById('drop-target-ui').style.display = show ? 'flex' : 'none';
}

// ─── LOG ─────────────────────────────────────────────────

/**
 * Yeni log satırı ekle.
 * Butonlar başlangıçta gizlidir; updateLog ile statusClass ve savedPath
 * geldiğinde gösterilir.
 */
function addLog(transferId, fileName, direction, statusText) {
  if (logItems[transferId]) return;   // duplicate koruması
  logItems[transferId] = true;

  const list = document.getElementById('log-list');

  const dirIcon = direction === 'out'
    ? '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="17 11 12 6 7 11"/><line x1="12" y1="18" x2="12" y2="6"/></svg>'
    : '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="7 13 12 18 17 13"/><line x1="12" y1="6" x2="12" y2="18"/></svg>';

  const el = document.createElement('div');
  el.className = 'log-item';
  el.id = `log-${transferId}`;
  el.dataset.transferId = transferId;

  el.innerHTML = `
    <div class="log-icon ${direction === 'out' ? 'log-dir-out' : 'log-dir-in'}">${dirIcon}</div>
    <div class="log-text"><strong>${fileName}</strong></div>
    <div class="log-progress"><div class="log-progress-fill" style="width:0%"></div></div>
    <div class="log-status">${statusText}</div>
    <div class="log-actions">
      <button class="log-cancel-btn" title="İptal Et"
        onclick="cancelTransfer('${transferId}')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
          <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
        </svg>
      </button>

      <button class="log-action-btn" title="Dosyayı Aç" style="display:none"
        id="btn-open-${transferId}" onclick="openPath('${transferId}')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
          <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
          <polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/>
        </svg>
      </button>

      <button class="log-action-btn" title="Klasörde Göster" style="display:none"
        id="btn-folder-${transferId}" onclick="showInFolder('${transferId}')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
          <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
        </svg>
      </button>

      <button class="log-delete-btn" title="Listeden Kaldır" style="display:none"
        id="btn-del-${transferId}" onclick="removeLogItem('${transferId}')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
          <polyline points="3 6 5 6 21 6"/>
          <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
        </svg>
      </button>
    </div>
  `;

  list.prepend(el);
  logCount = list.children.length;
  document.getElementById('log-count').textContent = logCount;
}

/**
 * Mevcut log satırını güncelle.
 * @param {string}      transferId
 * @param {string}      statusText    Gösterilecek durum metni
 * @param {string}      statusClass   done | success | error | cancelled | ''
 * @param {number}      pct           0‑100
 * @param {string|null} savedPath     İndirilen dosya/klasör yolu (gelen transfer için)
 */
function updateLog(transferId, statusText, statusClass, pct, savedPath) {
  const el = document.getElementById(`log-${transferId}`);
  if (!el) return;

  const status    = el.querySelector('.log-status');
  const fill      = el.querySelector('.log-progress-fill');
  const cancelBtn = el.querySelector('.log-cancel-btn');
  const btnOpen   = document.getElementById(`btn-open-${transferId}`);
  const btnFolder = document.getElementById(`btn-folder-${transferId}`);
  const btnDel    = document.getElementById(`btn-del-${transferId}`);

  if (status) {
    status.textContent = statusText;
    status.className   = `log-status${statusClass ? ' ' + statusClass : ''}`;
  }
  if (statusClass) el.classList.add(statusClass);
  if (fill && pct !== undefined) fill.style.width = `${pct}%`;

  const isTerminal = ['done', 'success', 'cancelled', 'error'].includes(statusClass);
  if (isTerminal) {
    if (cancelBtn) cancelBtn.style.display = 'none';
    if (btnDel)    btnDel.style.display    = 'flex';
  }

  // "Dosyayı Aç" ve "Klasörde Göster" — yalnızca gelen tamamlanmış transferlerde
  // ve geçerli bir path geldiyse göster
  if (statusClass === 'done' && savedPath && savedPath.length > 0) {
    el.dataset.savedPath = savedPath;
    if (btnOpen)   btnOpen.style.display   = 'flex';
    if (btnFolder) btnFolder.style.display = 'flex';
  }
}

window.removeLogItem = (transferId) => {
  const el = document.getElementById(`log-${transferId}`);
  if (!el) return;
  el.remove();
  delete logItems[transferId];
  logCount = document.getElementById('log-list').children.length;
  document.getElementById('log-count').textContent = logCount;
};

window.cancelTransfer = async (transferId) => {
  try {
    await invoke('cancel_transfer', { id: transferId });
    updateLog(transferId, 'İptal Edildi', 'cancelled', undefined, null);
    toast('Transfer iptal edildi', 'info');
  } catch (e) {
    console.error('İptal hatası:', e);
  }
};

window.openPath = async (transferId) => {
  const el = document.getElementById(`log-${transferId}`);
  if (el && el.dataset.savedPath) {
    try { await invoke('open_file', { path: el.dataset.savedPath }); }
    catch (e) { toast('Dosya açılamadı: ' + e, 'error'); }
  }
};

window.showInFolder = async (transferId) => {
  const el = document.getElementById(`log-${transferId}`);
  if (el && el.dataset.savedPath) {
    try { await invoke('show_in_folder', { path: el.dataset.savedPath }); }
    catch (e) { toast('Klasör açılamadı: ' + e, 'error'); }
  }
};

// ─── TOAST ───────────────────────────────────────────────
function toast(msg, type = 'info') {
  const c  = document.getElementById('toast-container');
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = msg;
  c.appendChild(el);
  setTimeout(() => el.remove(), 3500);
}

// ─── YARDIMCILAR ─────────────────────────────────────────
function formatSize(bytes) {
  if (bytes < 1024)             return `${bytes} B`;
  if (bytes < 1048576)          return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824)       return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(2)} GB`;
}
