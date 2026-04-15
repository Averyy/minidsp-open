/**
 * ezbeq-shim.js served directly by firmware so the board does not keep stale
 * browser discovery behavior from older LittleFS UI bundles.
 */
(function() {
  'use strict';

  const BOARD_BASE = new URL('.', document.currentScript?.src || window.location.href)
    .href
    .replace(/\/$/, '');
  const DB_NAME = 'ezbeq-cache';
  const STORE_NAME = 'catalogue';
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
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
  }

  function processEntry(entry) {
    delete entry.filters;
    entry.id = entry.digest || (entry.title + '_' + entry.year + '_' + (entry.audioTypes || []).join('_'));
    entry.contentType = entry.content_type || 'film';
    entry.mvAdjust = parseFloat(entry.mv || '0');
    entry.collection = typeof entry.collection === 'string'
      ? entry.collection
      : (entry.collection && typeof entry.collection.name === 'string'
          ? entry.collection.name
          : '');

    let fmt = entry.title || '';
    if (entry.season) {
      fmt += ' S' + entry.season;
      if (entry.episode) {
        fmt += 'E' + entry.episode;
      }
    }
    entry.formattedTitle = fmt;
    return entry;
  }

  async function loadCatalogue(ws) {
    let entries = null;

    try {
      const db = await openDB();
      const cachedVersion = await idbGet(db, 'version').catch(() => null);
      let remoteVersion = null;

      try {
        const vResp = await fetch(BOARD_BASE + '/version.txt');
        if (vResp.ok) {
          remoteVersion = (await vResp.text()).trim();
        }
      } catch (e) {
        // Ignore and fall through to cache or a fresh board-origin fetch.
      }

      if (cachedVersion && (remoteVersion === null || remoteVersion === cachedVersion)) {
        entries = await idbGet(db, 'entries').catch(() => null);
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
        entries = raw.map(processEntry);
        const version = remoteVersion || 'unknown';
        await idbPut(db, 'version', version);
        await idbPut(db, 'entries', entries);
        console.log('[ezbeq-shim] cached ' + entries.length + ' entries (v' + version +
                    ') via ' + BOARD_BASE);
      }
    } catch (err) {
      console.error('[ezbeq-shim] catalogue load failed:', err);
      const errMsg = JSON.stringify({
        message: 'Error',
        data: 'Could not load catalogue. Check internet connection.'
      });
      ws.dispatchEvent(new MessageEvent('message', { data: errMsg }));
      return;
    }

    entryMap = {};
    for (const entry of entries) {
      if (entry.id) {
        entryMap[entry.id] = entry;
      }
    }

    const msg = JSON.stringify({ message: 'CatalogueEntries', data: entries });
    ws.dispatchEvent(new MessageEvent('message', { data: msg }));
    console.log('[ezbeq-shim] dispatched ' + entries.length + ' entries to UI');
  }

  const OrigWebSocket = window.WebSocket;

  window.WebSocket = function(url, protocols) {
    const ws = protocols !== undefined
      ? new OrigWebSocket(url, protocols)
      : new OrigWebSocket(url);

    if (!url.endsWith('/ws')) {
      return ws;
    }

    const origSend = ws.send.bind(ws);
    ws.send = function(data) {
      if (typeof data === 'string' && data.trim() === 'load catalogue') {
        loadCatalogue(ws);
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

  const origFetch = window.fetch;

  window.fetch = function(url, opts) {
    if (typeof url === 'string' && opts && opts.method) {
      try {
        const method = opts.method.toUpperCase();
        const body = JSON.parse(opts.body);

        if (/\/api\/\d+\/devices\/[^/]+\/filter\/\d+/.test(url) && method === 'PUT') {
          if (body.entryId && entryMap[body.entryId]) {
            const entry = entryMap[body.entryId];
            let changed = false;

            if (!body.title && entry.title) {
              body.title = entry.title;
              changed = true;
            }
            if (!body.author && entry.author) {
              body.author = entry.author;
              changed = true;
            }
            if (changed) {
              opts = Object.assign({}, opts, { body: JSON.stringify(body) });
            }
          }
        }
      } catch (e) {
        // Pass malformed or non-JSON bodies through unchanged.
      }
    }
    return origFetch.call(this, url, opts);
  };

  console.log('[ezbeq-shim] loaded — board origin ' + BOARD_BASE);
})();
