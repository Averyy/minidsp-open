/**
 * Shared source for the firmware fallback and versioned UI bundle.
 * Catalogue browsing stays offline; summaries hydrate from the Pages asset.
 */
(function() {
  'use strict';

  const BOARD_BASE = new URL('.', document.currentScript?.src || window.location.href)
    .href
    .replace(/\/$/, '');
  // Upstream 2.12.4 retains entries only in React memory and reloads them on
  // each page load. IndexedDB avoids repeatedly streaming ~10 MB of decoded
  // metadata through the single ESP32 httpd task. A separate name invalidates
  // older caches whose entries were transformed by previous shim versions.
  const DB_NAME = 'ezbeq-2.12.4-cache';
  const STORE_NAME = 'catalogue';
  const DEFAULT_SITE_BASE = 'https://averyy.github.io/minidsp-open';
  let currentEntries = null;
  let entryMap = {};

  function openDB() {
    return new Promise((resolve, reject) => {
      const req = indexedDB.open(DB_NAME, 1);
      req.onupgradeneeded = () => req.result.createObjectStore(STORE_NAME);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  function idbGet(db, key) {
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readonly');
      const req = tx.objectStore(STORE_NAME).get(key);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  function idbPut(db, key, value) {
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readwrite');
      const req = tx.objectStore(STORE_NAME).put(value, key);
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error);
      req.onerror = () => reject(req.error);
    });
  }

  // React owns selection and receives JSON copies, so hydrate through its
  // existing catalogue update path after the offline listing has been sent.
  // Cache only summaries, keyed by the board's site version and digest/id.
  async function hydrateOverviews(ws, entries, version) {
    if (entries.every(entry => Object.prototype.hasOwnProperty.call(entry, 'overview'))) return;
    let db = null;
    try {
      db = await openDB().catch(() => null);
      const cached = db ? await idbGet(db, 'overviews').catch(() => null) : null;
      let values = cached && cached.version === version ? cached.values : null;
      if (!values) {
        if (navigator.onLine === false) return;
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 15000);
        try {
          let base = DEFAULT_SITE_BASE;
          try {
            const config = await fetch(BOARD_BASE + '/debug/updater', { signal: controller.signal });
            if (config.ok) base = (await config.json()).updaterBaseUrl || base;
          } catch (e) { /* default published site */ }
          const resp = await fetch(base.replace(/\/$/, '') + '/overviews.json.gz', { signal: controller.signal });
          if (!resp.ok) return;
          const bytes = new Uint8Array(await resp.arrayBuffer());
          // Pages serves .gz as a file; other hosts may decode Content-Encoding.
          const body = bytes[0] === 0x1f && bytes[1] === 0x8b
            ? new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip')))
            : new Response(bytes);
          values = await body.json();
        } finally {
          clearTimeout(timeout);
        }
        if (db) await idbPut(db, 'overviews', { version, values }).catch(() => {});
      }
      // A reconnect/version change may have replaced this catalogue meanwhile.
      if (currentEntries !== entries || ws.readyState !== OrigWebSocket.OPEN) return;
      let changed = false;
      for (const entry of entries) {
        if (values[entry.id] && !entry.overview) {
          entry.overview = values[entry.id];
          changed = true;
        }
      }
      if (changed) {
        ws.dispatchEvent(new MessageEvent('message', {
          data: JSON.stringify({ message: 'CatalogueEntries', data: entries })
        }));
      }
    } catch (e) {
      // Internet/CORS/cache failures are optional: the offline listing is ready.
    } finally {
      if (db) db.close();
    }
  }

  async function loadCatalogue(ws, remoteVersion) {
    let entries = null;
    let catalogueVersion = null;
    let db = null;

    try {
      db = await openDB().catch(() => null);
      const cached = db ? await idbGet(db, 'snapshot').catch(() => null) : null;
      const cachedVersion = cached && cached.version;
      if (cachedVersion && (remoteVersion === null || remoteVersion === cachedVersion)) {
        entries = cached.entries;
        catalogueVersion = cachedVersion;
        if (entries) {
          console.log('[ezbeq-shim] using cached catalogue v' + cachedVersion +
                      ' (' + entries.length + ' entries) via ' + BOARD_BASE);
        }
      }

      if (!entries) {
        console.log('[ezbeq-shim] fetching database.json from ' + BOARD_BASE + '...');
        const resp = await fetch(BOARD_BASE + '/database.json');
        if (!resp.ok) {
          throw new Error('HTTP ' + resp.status);
        }
        const raw = await resp.json();
        entries = raw;
        const version = remoteVersion || 'unknown';
        catalogueVersion = version;
        if (db) {
          await idbPut(db, 'snapshot', { version, entries }).catch(() => {});
        }
        console.log('[ezbeq-shim] cached ' + entries.length + ' entries (v' + version +
                    ') via ' + BOARD_BASE);
      }
    } catch (err) {
      console.error('[ezbeq-shim] catalogue load failed:', err);
      const errMsg = JSON.stringify({
        message: 'Error',
        data: 'Could not load catalogue from the board.'
      });
      ws.dispatchEvent(new MessageEvent('message', { data: errMsg }));
      return;
    } finally {
      if (db) db.close();
    }

    if (ws.readyState !== OrigWebSocket.OPEN) return;
    currentEntries = entries;
    entryMap = Object.fromEntries(entries.map(entry => [entry.id, entry]));

    const msg = JSON.stringify({ message: 'CatalogueEntries', data: entries });
    ws.dispatchEvent(new MessageEvent('message', { data: msg }));
    console.log('[ezbeq-shim] dispatched ' + entries.length + ' entries to UI');
    void hydrateOverviews(ws, entries, catalogueVersion);
  }

  const OrigWebSocket = window.WebSocket;

  window.WebSocket = function(url, protocols) {
    const ws = protocols !== undefined
      ? new OrigWebSocket(url, protocols)
      : new OrigWebSocket(url);

    if (new URL(url, BOARD_BASE).pathname !== '/ws') {
      return ws;
    }

    // Reuse the upstream Catalogue version announcement; only persistence is
    // ESP32-specific. Do not add a second HTTP version-check protocol.
    let version = null;
    ws.addEventListener('message', event => {
      if (typeof event.data !== 'string' ||
          !/"message"\s*:\s*"Catalogue"/.test(event.data.slice(0, 1024))) return;
      try {
        const message = JSON.parse(event.data);
        if (message.data) version = message.data.version;
      } catch (e) { /* upstream handles invalid messages */ }
    });
    const origSend = ws.send.bind(ws);
    ws.send = function(data) {
      if (typeof data === 'string' && data.trim() === 'load catalogue') {
        loadCatalogue(ws, version);
        return;
      }
      origSend(data);
    };

    return ws;
  };

  window.WebSocket.prototype = OrigWebSocket.prototype;
  window.WebSocket.CONNECTING = OrigWebSocket.CONNECTING;
  window.WebSocket.OPEN = OrigWebSocket.OPEN;
  window.WebSocket.CLOSING = OrigWebSocket.CLOSING;
  window.WebSocket.CLOSED = OrigWebSocket.CLOSED;

  console.log('[ezbeq-shim] loaded — board origin ' + BOARD_BASE);
})();
