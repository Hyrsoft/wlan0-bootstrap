document.addEventListener('DOMContentLoaded', () => {
  const wifiList = document.getElementById('wifi-list');
  const refreshBtn = document.getElementById('refresh-btn');
  const modal = document.getElementById('passwordModal');
  const modalSsid = document.getElementById('modal-ssid-name');
  const passwordInput = document.getElementById('password-input');
  const connectForm = document.getElementById('connect-form');
  const connectBtn = document.getElementById('connect-btn');
  const cancelBtn = document.getElementById('cancel-btn');
  const connectionStatus = document.getElementById('connection-status');
  const modalClose = document.getElementById('modal-close');

  let selectedSsid = null;
  let statusTimer = null;

  function showScannerStatus(text) {
    wifiList.innerHTML = `<div class="scanner-status"><div class="spinner"></div><div class="scanner-text">${escapeHtml(text)}</div></div>`;
  }

  async function fetchWifiNetworks() {
    showScannerStatus('正在读取可用 Wi‑Fi...');
    refreshBtn.disabled = true;
    try {
      const res = await fetch('/api/scan');
      if (!res.ok) throw new Error('扫描结果读取失败: ' + res.status);
      const nets = await res.json();
      renderList(nets);
    } catch (err) {
      console.warn('scan error', err);
      showScannerStatus('读取失败，7秒后重试...');
      setTimeout(fetchWifiNetworks, 7000);
    } finally {
      refreshBtn.disabled = false;
    }
  }

  function renderList(nets) {
    if (!nets || nets.length === 0) {
      showScannerStatus('未找到可用网络');
      return;
    }
    wifiList.innerHTML = '';
    nets.forEach(n => {
      const el = document.createElement('div');
      el.className = 'network-item';
      el.innerHTML = `
        <div class="network-left">
          <img class="wifi-svg" src="assets/wifi.svg" alt="wifi">
          <div class="network-info">
            <div class="net-ssid">${escapeHtml(n.ssid)}</div>
            <div class="net-meta">${escapeHtml(n.security || 'Unknown')} • 信号 ${n.signal}%</div>
          </div>
        </div>
        <div class="net-right">
          <div class="net-signal">${n.signal}%</div>
          ${signalBarsHtml(n.signal)}
        </div>
      `;
      el.addEventListener('click', () => {
        if (n.security && n.security !== 'Open') {
          openModal(n.ssid);
        } else {
          selectedSsid = n.ssid;
          connect(n.ssid, '');
        }
      });
      wifiList.appendChild(el);
    });
  }

  function openModal(ssid) {
    selectedSsid = ssid;
    modalSsid.textContent = `连接 ${ssid}`;
    passwordInput.value = '';
    connectionStatus.textContent = '';
    connectionStatus.style.color = '';
    modal.style.display = 'flex';
    modal.setAttribute('aria-hidden', 'false');
    setTimeout(() => passwordInput.focus(), 50);
  }

  function closeModal() {
    modal.style.display = 'none';
    modal.setAttribute('aria-hidden', 'true');
  }

  async function connect(ssid, password) {
    clearStatusTimer();
    connectBtn.disabled = true;
    connectionStatus.textContent = '正在发送连接请求...';
    connectionStatus.style.color = '';
    try {
      const res = await fetch('/api/connect', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ssid, password })
      });
      if (!res.ok) {
        const e = await res.json().catch(() => ({ message: '连接请求失败' }));
        throw new Error(e.message || '连接请求失败');
      }
      connectionStatus.textContent = '请求已接收，正在连接...';
      startStatusPolling();
    } catch (err) {
      connectionStatus.textContent = '连接失败：' + err.message;
      connectionStatus.style.color = '#ff6b6b';
      connectBtn.disabled = false;
    }
  }

  function startStatusPolling() {
    statusTimer = setInterval(async () => {
      try {
        const res = await fetch('/api/status');
        if (!res.ok) return;
        const status = await res.json();
        renderConnectionStatus(status);
      } catch (err) {
        clearStatusTimer();
      }
    }, 1000);
  }

  function renderConnectionStatus(status) {
    const state = status.state || 'Unknown';
    if (state === 'Connected') {
      connectionStatus.textContent = '连接成功，设备正在切换到目标 Wi‑Fi。';
      connectionStatus.style.color = '#2dd4bf';
      clearStatusTimer();
      setTimeout(closeModal, 2000);
      return;
    }

    if (status.last_error) {
      connectionStatus.textContent = `连接失败：${status.last_error.message}`;
      connectionStatus.style.color = '#ff6b6b';
      connectBtn.disabled = false;
      clearStatusTimer();
      return;
    }

    connectionStatus.textContent = `当前状态：${state}`;
  }

  function clearStatusTimer() {
    if (statusTimer) {
      clearInterval(statusTimer);
      statusTimer = null;
    }
  }

  connectForm.addEventListener('submit', ev => {
    ev.preventDefault();
    connect(selectedSsid, passwordInput.value || '');
  });
  cancelBtn.addEventListener('click', closeModal);
  refreshBtn.addEventListener('click', fetchWifiNetworks);
  if (modalClose) modalClose.addEventListener('click', closeModal);

  function signalBarsHtml(signal) {
    const level = Math.max(0, Math.min(4, Math.ceil((signal / 100) * 4)));
    let html = '<div class="signal-bars" aria-hidden="true">';
    for (let i = 1; i <= 4; i++) {
      html += `<span class="${i <= level ? 'active' : ''}" style="height:${8 + i * 6}px"></span>`;
    }
    html += '</div>';
    return html;
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, c => ({
      '&': '&amp;',
      '<': '&lt;',
      '>': '&gt;',
      '"': '&quot;',
      "'": '&#39;'
    }[c]));
  }

  refreshBtn.style.display = 'none';
  fetchWifiNetworks();
});
