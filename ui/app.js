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
  const discoveryHint = document.getElementById('discovery-hint');

  let selectedSsid = null;
  let statusTimer = null;
  let plannedHostname = null;

  async function fetchInitialStatus() {
    try {
      const res = await fetch('/api/status');
      if (!res.ok) return false;
      const status = await res.json();
      updateDiscoveryHint(status);
      if (status.state === 'Connected') {
        renderConnectedPanel(status);
        return true;
      }
    } catch (err) {
      return false;
    }
    return false;
  }

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
    connectionStatus.innerHTML = plannedHostname ? preconnectHostnameHtml() : '';
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
      connectionStatus.innerHTML = plannedHostname
        ? `请求已接收，正在连接...${preconnectHostnameHtml()}`
        : '请求已接收，正在连接...';
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
        updateDiscoveryHint(status);
        renderConnectionStatus(status);
      } catch (err) {
        clearStatusTimer();
      }
    }, 1000);
  }

  function renderConnectionStatus(status) {
    const state = status.state || 'Unknown';
    if (state === 'Connected') {
      connectionStatus.innerHTML = connectedStatusHtml(status);
      connectionStatus.style.color = '#2dd4bf';
      renderConnectedPanel(status);
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

  function updateDiscoveryHint(status) {
    if (!status || !status.hostname || !discoveryHint) return;
    plannedHostname = status.hostname;
    discoveryHint.innerHTML = `
      <div class="hint-title">联网后访问地址</div>
      <div class="hint-link">http://${escapeHtml(status.hostname)}</div>
      <div class="hint-note">提交 Wi‑Fi 后，手机切回同一个家庭 Wi‑Fi 再打开这个地址。</div>
    `;
  }

  function preconnectHostnameHtml() {
    return `
      <div class="preconnect-hostname">
        联网后访问：<span>http://${escapeHtml(plannedHostname)}</span>
      </div>
    `;
  }

  function renderConnectedPanel(status) {
    wifiList.innerHTML = `
      <div class="connected-panel">
        <div class="connected-title">设备已联网</div>
        <div class="connected-detail">${connectedStatusHtml(status)}</div>
        <div class="connected-note">手机需要切回同一个家庭 Wi‑Fi 后访问。</div>
      </div>
    `;
  }

  function connectedStatusHtml(status) {
    const rows = [];
    if (status.address) {
      rows.push(`<div>当前 IP：<a href="http://${escapeHtml(status.address)}">http://${escapeHtml(status.address)}</a></div>`);
    }
    if (status.hostname) {
      rows.push(`<div>推荐地址：<a href="http://${escapeHtml(status.hostname)}">http://${escapeHtml(status.hostname)}</a></div>`);
    }
    if (!rows.length) {
      rows.push('<div>连接成功，正在准备访问地址...</div>');
    }
    if (status.discovery && status.discovery.last_error) {
      rows.push(`<div class="discovery-warning">mDNS 发布失败：${escapeHtml(status.discovery.last_error)}</div>`);
    }
    return rows.join('');
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
  fetchInitialStatus().then(connected => {
    if (!connected) fetchWifiNetworks();
  });
});
