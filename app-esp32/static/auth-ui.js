// Prepended to the board UI shim only while password protection is enabled.
(function () {
  'use strict';
  const origin = location.origin;
  const login = () => location.assign('/login?next=' + encodeURIComponent(location.pathname + location.search));
  const originalFetch = window.fetch.bind(window);
  window.fetch = async function (...args) {
    const response = await originalFetch(...args);
    if (response.status === 401 && new URL(response.url || origin, origin).origin === origin) login();
    return response;
  };
  const originalOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...args) {
    const board = new URL(url, location.href).origin === origin;
    this.addEventListener('load', () => { if (board && this.status === 401) login(); });
    return originalOpen.call(this, method, url, ...args);
  };
  const OriginalWebSocket = window.WebSocket;
  window.WebSocket = function (url, protocols) {
    const ws = protocols === undefined ? new OriginalWebSocket(url) : new OriginalWebSocket(url, protocols);
    if (new URL(url, location.href).origin.replace(/^ws/, 'http') === origin) {
      ws.addEventListener('close', () => {
        originalFetch('/settings/password', {cache:'no-store'}).then(response => { if (response.status === 401) login(); }).catch(() => {});
      });
    }
    return ws;
  };
  window.WebSocket.prototype = OriginalWebSocket.prototype;
  for (const key of ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED']) window.WebSocket[key] = OriginalWebSocket[key];
})();
