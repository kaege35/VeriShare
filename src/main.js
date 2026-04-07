const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ─── STATE (LOCAL & HUB) ──────────────────────────────────
let myName = '';
let myId = null;
let selectedUser = null;
let logItems = {};
let logCount = 0;
let activeTransferId = null;
let lastUsers = [];
let searchQuery = '';

// HUB STATE
const HUB_API_URL = 'https://veritasdijital.tech/onayapp/api.php';
let currentHubId = null;
let currentHubPass = '';
let isAdmin = false;
let lastMsgId = 0;
let hubPollInterval = null;
let pendingHubFile = null;

// ─── INIT ─────────────────────────────────────────────────
document.addEventListener("DOMContentLoaded", () => {
  // LOCAL LOGIN
  document.getElementById('join-btn').addEventListener('click', joinNetwork);
  document.getElementById('name-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') joinNetwork();
  });

  // TABS
  document.getElementById('tab-local').addEventListener('click', () => switchView('local'));
  document.getElementById('tab-hub').addEventListener('click', () => switchView('hub'));

  // HUB ACTIONS
  document.getElementById('hub-join-btn').addEventListener('click', hubJoin);
  document.getElementById('hub-pass-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') hubJoin();
  });
  document.getElementById('hub-logout-btn').addEventListener('click', hubLogout);
  document.getElementById('hub-send-btn').addEventListener('click', hubSendMessage);
  
  const hubAttachBtn = document.getElementById('hub-attach-btn');
  const hubFileInput = document.getElementById('hub-file-input');
  
  hubAttachBtn.addEventListener('click', () => hubFileInput.click());
  hubFileInput.addEventListener('change', (e) => {
    if (e.target.files.length > 0) {
      pendingHubFile = e.target.files[0];
      hubAttachBtn.classList.add('active');
      toast(`${pendingHubFile.name} seçildi.`, 'success');
    }
  });

  document.getElementById('hub-msg-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') hubSendMessage();
  });

  // LOCAL P2P EVENTS
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

  document.getElementById('browse-btn').addEventListener('click', async () => {
    if (!selectedUser) { toast('Önce bir kişi seç', 'error'); return; }
    const { open } = window.__TAURI__.dialog;
    try {
      const filePaths = await open({ multiple: true, directory: false, title: "Dosyaları Seç" });
      if (!filePaths || filePaths.length === 0) return;
      invoke('send_paths_directly', { peerIp: selectedUser.ip, paths: filePaths });
    } catch(e) { toast('İptal edildi.', 'info'); }
  });

  document.getElementById('refresh-btn').addEventListener('click', async () => {
    const btn = document.getElementById('refresh-btn');
    btn.classList.add('spinning');
    try {
      await invoke('scan_network');
      toast('Ağ taranıyor...', 'info');
    } catch(e) { toast('Hata: ' + e, 'error'); }
    setTimeout(() => btn.classList.remove('spinning'), 1500);
  });

  document.getElementById('user-search').addEventListener('input', (e) => {
    searchQuery = e.target.value.toLowerCase();
    renderUserList();
  });

  document.getElementById('log-clear-btn').addEventListener('click', () => {
    const list = document.getElementById('log-list');
    const items = list.querySelectorAll('.log-item.done, .log-item.success, .log-item.error, .log-item.cancelled');
    items.forEach(el => {
      const transferId = el.dataset.transferId;
      if (transferId) delete logItems[transferId];
      el.remove();
    });
    logCount = list.children.length;
    document.getElementById('log-count').textContent = logCount;
  });

  listen('transfer-initiated', (e) => addLog(e.payload.transfer_id, e.payload.text, e.payload.dir, e.payload.dir==='out'?'Onay bekleniyor...':'Başlıyor...'));
  listen('transfer-progress', (e) => {
    const { id, pct, text, is_done, cancelled, path } = e.payload;
    if (cancelled) { updateLog(id, 'İptal Edildi', 'cancelled', pct); return; }
    if (is_done) { updateLog(id, 'Tamamlandı', 'done', 100, path); toast(text + ' indirildi!', 'success'); }
    else { updateLog(id, `%${pct}`, '', pct); }
  });
  listen('transfer-out-progress', (e) => {
    const { id, pct, text, is_done, cancelled } = e.payload;
    if (cancelled) { updateLog(id, 'İptal Edildi', 'cancelled', pct); return; }
    if (is_done) { updateLog(id, 'İletildi', 'success', 100); toast(text + ' gönderildi!', 'success'); }
    else { updateLog(id, `%${pct}`, '', pct); }
  });
  listen('peers-updated', (e) => updateUserList(e.payload));
});

// ─── NAV LOGIC ────────────────────────────────────────────
function switchView(view) {
  document.querySelectorAll('.nav-tab').forEach(t => t.classList.remove('active'));
  document.querySelectorAll('.sidebar-view').forEach(s => s.classList.remove('active'));
  document.querySelectorAll('.main-view').forEach(m => m.classList.remove('active'));

  document.getElementById(`tab-${view}`).classList.add('active');
  document.getElementById(`${view}-sidebar`).classList.add('active');
  document.getElementById(`${view}-view`).classList.add('active');
}

// ─── HUB LOGIC ────────────────────────────────────────────
async function hubJoin() {
  const pass = document.getElementById('hub-pass-input').value.trim();

  if(!pass) { toast('Lütfen parolayı giriniz.', 'error'); return; }

  const formData = new FormData();
  formData.append('password', pass);

  try {
    const res = await fetch(`${HUB_API_URL}?action=login`, { method: 'POST', body: formData });
    const data = await res.json();
    
    if(data.status === 'success') {
      currentHubId = 'grafik-tasarim';
      isAdmin = data.role === 'admin';
      currentHubPass = pass;

      document.getElementById('hub-auth-container').style.display = 'none';
      document.getElementById('hub-main-container').style.display = 'flex';
      document.getElementById('active-hub-id').textContent = "GRAFİK TASARIM HUB";
      
      const badge = document.getElementById('admin-badge');
      badge.textContent = isAdmin ? 'LİDER' : 'ÜYE';
      badge.className = `hub-badge ${isAdmin ? 'admin' : ''}`;
      document.querySelector('.hub-status-text').textContent = `Tasarım Hub oturumu aktif.`;

      startHubPolling();
      toast(isAdmin ? 'Lider olarak giriş yapıldı' : 'Ekip üyesi olarak giriş yapıldı', 'success');
    } else {
      toast(data.message || 'Hatalı şifre!', 'error');
    }
  } catch(e) {
    toast('Bağlantı hatası!', 'error');
  }
}

function startHubPolling() {
  lastMsgId = 0;
  document.getElementById('hub-messages').innerHTML = '';
  hubFetchMessages();
  hubPollInterval = setInterval(hubFetchMessages, 4000);
}

async function hubFetchMessages() {
  if(!currentHubId) return;
  try {
    const res = await fetch(`${HUB_API_URL}?action=fetch&hub_id=${currentHubId}&last_id=${lastMsgId}`);
    const data = await res.json();
    if(data && data.length > 0) {
      data.forEach(msg => {
        renderHubMessage(msg);
        lastMsgId = Math.max(lastMsgId, msg.id);
      });
      const list = document.getElementById('hub-messages');
      list.scrollTop = list.scrollHeight;
    }
  } catch(e) { console.error('Hub error:', e); }
}

function renderHubMessage(msg) {
  const list = document.getElementById('hub-messages');
  const isMe = msg.sender_name === myName;
  const existing = document.getElementById(`hub-msg-${msg.id}`);
  if(existing) {
     // Durum güncellemesi kontrolü
     const tag = existing.querySelector('.status-tag');
     if(tag && !tag.classList.contains(msg.status)) {
        tag.className = `status-tag ${msg.status}`;
        tag.textContent = msg.status.toUpperCase();
     }
     return;
  }

  const div = document.createElement('div');
  div.className = `hub-message ${isMe ? 'sent' : 'received'}`;
  div.id = `hub-msg-${msg.id}`;

  let time = msg.created_at?.split(' ')?.[1]?.slice(0,5) ?? '--:--';
  let html = `<div class="hub-msg-meta">${msg.sender_name} • ${time}</div>`;
  if(msg.message) html += `<div>${msg.message}</div>`;
  
  if(msg.file_url) {
    html += `
      <a href="${msg.file_url}" target="_blank" class="hub-msg-file">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>
        <span style="word-break:break-all;">${msg.file_name}</span>
      </a>
      <div class="hub-msg-status">
        <span class="status-tag ${msg.status}">${msg.status.toUpperCase()}</span>
        ${msg.admin_note ? `<small style="display:block;opacity:0.7;">"${msg.admin_note}"</small>` : ''}
      </div>
    `;
    
    if(isAdmin && msg.status === 'pending' && !isMe) {
      html += `
        <div class="hub-admin-actions">
          <button class="hub-action-btn approve" onclick="updateHubStatus(${msg.id}, 'approved')">✓ ONAY</button>
          <button class="hub-action-btn revise" onclick="updateHubStatus(${msg.id}, 'revised')">⚠ REVİZE</button>
        </div>
      `;
    }
  }

  div.innerHTML = html;
  list.appendChild(div);
}

async function hubSendMessage() {
  const input = document.getElementById('hub-msg-input');
  const msg = input.value.trim();
  if(!msg && !pendingHubFile) return;

  const btn = document.getElementById('hub-send-btn');
  btn.disabled = true;

  const formData = new FormData();
  formData.append('hub_id', currentHubId);
  formData.append('sender_name', myName);
  formData.append('message', msg);
  if(pendingHubFile) formData.append('file', pendingHubFile);

  try {
    const res = await fetch(`${HUB_API_URL}?action=send`, { method: 'POST', body: formData });
    const data = await res.json();
    if(data.status === 'success') {
      input.value = '';
      pendingHubFile = null;
      document.getElementById('hub-attach-btn').classList.remove('active');
      hubFetchMessages();
    }
  } catch(e) { toast('Gönderim başarısız', 'error'); }
  btn.disabled = false;
}

window.updateHubStatus = async (id, status) => {
  const note = status === 'revised' ? prompt('Revize notu/açıklama:') : '';
  const formData = new FormData();
  formData.append('id', id);
  formData.append('status', status);
  formData.append('note', note || '');

  try {
    await fetch(`${HUB_API_URL}?action=update_status`, { method: 'POST', body: formData });
    hubFetchMessages();
  } catch(e) { toast('İşlem başarısız', 'error'); }
}

function hubLogout() {
  clearInterval(hubPollInterval);
  currentHubId = null;
  document.getElementById('hub-auth-container').style.display = 'flex';
  document.getElementById('hub-main-container').style.display = 'none';
  document.querySelector('.hub-status-text').textContent = 'Hub oturumu kapalı.';
  toast('Oturum kapatıldı', 'info');
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
  } catch(e) { toast('Ağa katılma hatası: ' + e, 'error'); }
}

async function fetchWifiSSID() {
  try {
    const ssid = await invoke('get_wifi_ssid');
    const el = document.getElementById('wifi-name');
    if (ssid && el) el.textContent = ssid;
  } catch(e) {}
}

// ─── KULLANICI LİSTESİ ───────────────────────────────────
function updateUserList(users) {
  lastUsers = users;
  renderUserList();
}

function renderUserList() {
  const list = document.getElementById('user-list');
  const count = document.getElementById('online-count');
  const otherUsers = lastUsers.filter(u => u.id !== myId);
  count.textContent = otherUsers.length;
  list.innerHTML = '';
  
  lastUsers.filter(u => u.name.toLowerCase().includes(searchQuery)).forEach(u => {
    const isSelf = u.id === myId;
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
}

function selectUser(user) {
  selectedUser = user;
  document.getElementById('drop-target-name').textContent = user.name + ' cihazına gönder';
  showDropUI(true);
  document.querySelectorAll('.user-item').forEach(el => el.classList.remove('selected'));
  // Basit görsel eşleşme
}

function showDropUI(show) {
  document.getElementById('no-target').style.display = show ? 'none' : 'flex';
  document.getElementById('drop-target-ui').style.display = show ? 'flex' : 'none';
}

function addLog(transferId, fileName, direction, statusText) {
  if (logItems[transferId]) return;
  const list = document.getElementById('log-list');
  logItems[transferId] = transferId;
  const dirClass = direction === 'out' ? 'log-dir-out' : 'log-dir-in';
  const el = document.createElement('div');
  el.className = 'log-item';
  el.id = 'log-' + transferId;
  el.innerHTML = `
    <div class="log-icon ${dirClass}">
      ${direction==='out'?'↑':'↓'}
    </div>
    <div class="log-text"><strong>${fileName}</strong></div>
    <div class="log-progress"><div class="log-progress-fill" style="width:0%"></div></div>
    <div class="log-status">${statusText}</div>
  `;
  list.prepend(el);
  document.getElementById('log-count').textContent = list.children.length;
}

function updateLog(transferId, statusText, statusClass, pct, savedPath) {
  const el = document.getElementById('log-' + transferId);
  if (!el) return;
  const status = el.querySelector('.log-status');
  const fill = el.querySelector('.log-progress-fill');
  if (status) { status.textContent = statusText; status.className = `log-status ${statusClass || ''}`; }
  if (fill && pct !== undefined) fill.style.width = pct + '%';
}

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
