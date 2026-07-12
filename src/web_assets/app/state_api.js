const $ = id => document.getElementById(id);
const text = value => value == null ? 'none' : String(value);

let lastSnapshotJson = '';
let lastContext = null;
let lastMissionLog = null;
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

function renderOperatorControls(draft) {
  const dryRunChecked = draft.dryRun ? ' checked' : '';
  const projectCompleteChecked = draft.projectComplete ? ' checked' : '';
  return `<div class="control-grid">
    <button type="button" class="control-button" data-action="request-intervention" data-intervention="pause">Pause next cycle</button>
    <button type="button" class="control-button" data-action="request-intervention" data-intervention="resume">Resume paused loop</button>
    <button type="button" class="control-button danger" data-action="request-intervention" data-intervention="stop">Stop next cycle</button>
    <button type="button" class="control-button" data-action="request-intervention" data-intervention="steer">Add steering</button>
  </div>
  <label for="control-reason">Reason</label><input id="control-reason" value="${esc(draft.reason)}">
  <label for="control-instruction">Instruction for the next loop prompt</label><textarea id="control-instruction">${esc(draft.instruction)}</textarea>
  <details class="inline-details"><summary>Loop start options</summary>${renderLoopStart(draft, dryRunChecked, projectCompleteChecked)}</details>
  <p id="control-status" class="muted">${esc(draft.status)}</p>`;
}

function renderLoopStart(draft, dryRunChecked, projectCompleteChecked) {
  const streamAgentOutputChecked = draft.streamAgentOutput ? ' checked' : '';
  return `<label for="loop-prompt">Prompt path</label><input id="loop-prompt" value="${esc(draft.prompt)}">
  <label for="loop-prompt-slug">Prompt slug</label><input id="loop-prompt-slug" value="${esc(draft.promptSlug)}">
  <label for="loop-bundle">Bundle slug</label><input id="loop-bundle" value="${esc(draft.bundle)}">
  <label for="loop-prompt-role">Bundle prompt role</label><input id="loop-prompt-role" value="${esc(draft.promptRole)}">
  <label for="loop-agent">Agent</label><select id="loop-agent"><option value="agentctl"${draft.agent === 'agentctl' ? ' selected' : ''}>agentctl</option></select>
  <label for="loop-agent-argv">Agent argv JSON</label><textarea id="loop-agent-argv">${esc(draft.agentArgv)}</textarea>
  <label for="loop-agent-timeout-seconds">Agent timeout seconds</label><input id="loop-agent-timeout-seconds" type="number" min="1" value="${esc(draft.agentTimeoutSeconds)}">
  <label for="loop-audit-argv">Audit argv JSON</label><textarea id="loop-audit-argv">${esc(draft.auditArgv)}</textarea>
  <label for="loop-max-iterations">Max iterations</label><input id="loop-max-iterations" type="number" min="1" value="${esc(draft.maxIterations)}">
  <label><input id="loop-dry-run" type="checkbox"${dryRunChecked}> Dry run</label>
  <label><input id="loop-stream-agent-output" type="checkbox"${streamAgentOutputChecked}> Stream agent output</label>
  <label><input id="loop-project-complete" type="checkbox"${projectCompleteChecked}> Request project completion audit</label>
  <button type="button" class="control-button" data-action="start-loop">Start loop cycle</button>
  <p id="loop-start-status" class="muted">${esc(draft.startStatus)}</p>`;
}

async function startLoop() {
  const statusNode = $('loop-start-status');
  statusNode.textContent = 'Starting loop cycle...';
  const agentArgv = $('loop-agent-argv').value.trim();
  const body = new URLSearchParams({prompt:$('loop-prompt').value.trim(),prompt_slug:$('loop-prompt-slug').value.trim(),bundle:$('loop-bundle').value.trim(),prompt_role:$('loop-prompt-role').value.trim(),agent:agentArgv?'':$('loop-agent').value,agent_argv:agentArgv,agent_timeout_seconds:$('loop-agent-timeout-seconds').value.trim(),audit_argv:$('loop-audit-argv').value.trim(),dry_run:$('loop-dry-run').checked?'true':'false',stream_agent_output:$('loop-stream-agent-output').checked?'true':'false',max_iterations:$('loop-max-iterations').value.trim(),project_complete_requested:$('loop-project-complete').checked?'true':'false'});
  const response = await fetch('/api/loop/start', {method:'POST',headers:controlHeaders(),body});
  if (!response.ok) { statusNode.textContent = await apiErrorMessage(response); return; }
  const result = await response.json();
  statusNode.textContent = `${result.message || 'Loop process started.'} pid ${result.pid}.`;
  if (document.activeElement) document.activeElement.blur();
  lastSnapshotJson = '';
  await load();
}

async function requestIntervention(action) {
  const body = new URLSearchParams({reason:$('control-reason').value.trim()});
  if (action === 'steer') body.set('instruction', $('control-instruction').value.trim());
  await postControl(`/api/loop/interventions/${action}`, body);
}

async function clearIntervention(id) {
  const reason = $('control-reason').value.trim() || 'Operator cleared from cockpit';
  await postControl(`/api/loop/interventions/clear/${id}`, new URLSearchParams({reason}));
}

async function postControl(url, body) {
  const statusNode = $('control-status');
  statusNode.textContent = 'Writing control event...';
  const response = await fetch(url, {method:'POST',headers:controlHeaders(),body});
  if (!response.ok) { statusNode.textContent = await apiErrorMessage(response); return; }
  statusNode.textContent = 'Control event recorded.';
  if (document.activeElement) document.activeElement.blur();
  lastSnapshotJson = '';
  await load();
}
