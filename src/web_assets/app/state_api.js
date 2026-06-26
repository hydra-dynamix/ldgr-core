const $ = id => document.getElementById(id);
const text = value => value == null ? 'none' : String(value);

let lastSnapshotJson = '';
let lastContext = null;
let lastMissionLog = null;
let lastConduct = null;
let conductLoadInFlight = false;
let currentView = initialViewFromHash();
let currentDetailRoute = '';
let selectedArtifactId = null;
let selectedInspector = 'overview';
let artifactSearchTerm = '';
let decisionFilterTerm = '';

function storedControlToken() {
  const fromUrl = new URLSearchParams(location.search).get('control_token');
  if (fromUrl) {
    sessionStorage.setItem('ldgr-control-token', fromUrl);
    const url = new URL(location.href);
    url.searchParams.delete('control_token');
    history.replaceState(null, '', url.pathname + url.search + url.hash);
  }
  return sessionStorage.getItem('ldgr-control-token');
}

function controlHeaders() {
  const headers = {'Content-Type': 'application/x-www-form-urlencoded'};
  const token = storedControlToken();
  if (token) headers['X-LDGR-Control-Token'] = token;
  return headers;
}

function esc(value) {
  return text(value).replace(/[&<>\"]/g, char => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '\"': '&quot;'
  }[char]));
}

function status(value) {
  const normalized = text(value).toLowerCase().replace(/_/g, '-');
  return `<span class="status ${esc(normalized)}">${esc(value)}</span>`;
}

function route(value) {
  return encodeURIComponent(text(value));
}

function encodedRouteSegment(prefix) {
  const segment = location.pathname.slice(prefix.length);
  try {
    return encodeURIComponent(decodeURIComponent(segment));
  } catch {
    return encodeURIComponent(segment);
  }
}

async function apiJson(url, options) {
  const response = await fetch(url, options);
  if (!response.ok) throw new Error(await apiErrorMessage(response));
  return response.json();
}

async function apiErrorMessage(response) {
  const fallback = `${response.status} ${response.statusText}`.trim() || 'Request failed';
  try {
    const body = await response.json();
    return body && body.error && body.error.message ? body.error.message : fallback;
  } catch {
    try {
      const body = await response.text();
      return body || fallback;
    } catch {
      return fallback;
    }
  }
}

async function load() {
  if (isEditingControl()) return;
  const [context, missionLog] = await Promise.all([
    apiJson('/api/context'),
    apiJson('/api/mission-log')
  ]);
  const snapshotJson = JSON.stringify({context, missionLog});
  if (snapshotJson === lastSnapshotJson) {
    $('last-refresh').textContent = 'Checked ' + new Date().toLocaleTimeString();
    return;
  }
  lastSnapshotJson = snapshotJson;
  lastContext = context;
  lastMissionLog = missionLog;
  render();
  await renderRoute();
  $('last-refresh').textContent = 'Updated ' + new Date().toLocaleTimeString();
  if (currentView === 'waves') loadConduct();
}

async function loadConduct() {
  if (conductLoadInFlight) return;
  conductLoadInFlight = true;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 2500);
  try {
    lastConduct = await apiJson('/api/conduct/waves', {signal: controller.signal});
  } catch (error) {
    lastConduct = {available: false, error: String(error), batches: [], feeds: []};
  } finally {
    clearTimeout(timeout);
    conductLoadInFlight = false;
  }
  if (currentView === 'waves') renderWaveManagement(lastConduct);
}

function isEditingControl() {
  const element = document.activeElement;
  return element && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA' || element.tagName === 'SELECT');
}

function controlDraft() {
  return {
    reason: $('control-reason') ? $('control-reason').value : 'Operator requested from cockpit',
    instruction: $('control-instruction') ? $('control-instruction').value : '',
    status: $('control-status') ? $('control-status').textContent : 'Controls write durable loop intervention events.',
    prompt: $('loop-prompt') ? $('loop-prompt').value : 'prompts/loop-prompt.md',
    promptSlug: $('loop-prompt-slug') ? $('loop-prompt-slug').value : '',
    bundle: $('loop-bundle') ? $('loop-bundle').value : '',
    promptRole: $('loop-prompt-role') ? $('loop-prompt-role').value : '',
    agent: $('loop-agent') ? $('loop-agent').value : 'agentctl',
    agentArgv: $('loop-agent-argv') ? $('loop-agent-argv').value : '',
    agentTimeoutSeconds: $('loop-agent-timeout-seconds') ? $('loop-agent-timeout-seconds').value : '43200',
    auditArgv: $('loop-audit-argv') ? $('loop-audit-argv').value : '["agentctl","run"]',
    dryRun: $('loop-dry-run') ? $('loop-dry-run').checked : false,
    streamAgentOutput: $('loop-stream-agent-output') ? $('loop-stream-agent-output').checked : false,
    maxIterations: $('loop-max-iterations') ? $('loop-max-iterations').value : '1',
    projectComplete: $('loop-project-complete') ? $('loop-project-complete').checked : false,
    startStatus: $('loop-start-status') ? $('loop-start-status').textContent : 'Start launches one bounded loop cycle in the background.'
  };
}

