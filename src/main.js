import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import './styles.css';

const form = document.querySelector('#scan-form');
const pathInput = document.querySelector('#path-input');
const scanButton = document.querySelector('#scan-button');
const statusEl = document.querySelector('#status');
const countEl = document.querySelector('#count');
const resultsEl = document.querySelector('#results');

let activeScanId = null;
let foundCount = 0;

function setStatus(message, state = 'idle') {
  statusEl.textContent = message;
  statusEl.className = `status ${state}`;
}

function resetResults() {
  foundCount = 0;
  countEl.textContent = '0 found';
  resultsEl.replaceChildren();
}

function appendResult(path) {
  foundCount += 1;
  countEl.textContent = `${foundCount} found`;

  const item = document.createElement('li');
  item.textContent = path;
  item.title = path;
  resultsEl.appendChild(item);
}

function isActive(payload) {
  return payload?.scanId === activeScanId;
}

async function init() {
  await Promise.all([
    listen('epub-found', (event) => {
      if (!isActive(event.payload)) return;
      appendResult(event.payload.path);
    }),
    listen('scan-finished', (event) => {
      if (!isActive(event.payload)) return;
      setStatus(`Finished. ${foundCount} EPUB file${foundCount === 1 ? '' : 's'} found.`, 'done');
      scanButton.disabled = false;
    }),
    listen('scan-error', (event) => {
      if (!isActive(event.payload)) return;
      setStatus(event.payload.message, 'error');
      scanButton.disabled = false;
    }),
  ]);

  form.addEventListener('submit', async (event) => {
    event.preventDefault();

    const path = pathInput.value.trim();
    if (!path) return;

    activeScanId = crypto.randomUUID();
    resetResults();
    scanButton.disabled = true;
    setStatus('Scanning...', 'running');

    try {
      await invoke('scan_epubs', { path, scanId: activeScanId });
    } catch (error) {
      setStatus(String(error), 'error');
      scanButton.disabled = false;
    }
  });
}

init().catch((error) => {
  setStatus(`Failed to initialize event listeners: ${error}`, 'error');
  scanButton.disabled = true;
});
