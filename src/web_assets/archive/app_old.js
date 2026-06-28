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

function render() {
  if (!lastContext || !lastMissionLog) return;
  const context = lastContext;
  const missionLog = lastMissionLog;
  renderShell(context);
  renderCurrent(context, missionLog);
  renderDecisionTrail(missionLog);
  renderArtifactView(missionLog);
  renderWaveManagement(lastConduct);
  $('operator-controls').innerHTML = renderOperatorControls(controlDraft());
}

function pendingInterventions(context) {
  return (context.loop_interventions || []).filter(intervention => intervention.status === 'pending');
}

function loopStatusInfo(context) {
  const loopState = context.loop_state || {};
  const paused = pendingInterventions(context).some(intervention => intervention.action === 'pause');
  if (paused) return {label: 'paused', className: 'warning'};
  if (loopState.run_id == null) return {label: 'idle', className: 'muted'};
  if (!loopState.terminal_status) return {label: 'running', className: 'running'};
  return {
    label: loopState.terminal_status,
    className: loopState.terminal_status === 'success' ? 'success' : 'failed'
  };
}

function renderShell(context) {
  const loopState = context.loop_state || {};
  const loopStatus = loopStatusInfo(context);
  $('loop-state-dot').className = `state-dot ${loopStatus.className}`;
  const work = loopState.work_slug || (context.next_work_item && context.next_work_item.slug) || 'no active work';
  $('status-summary').innerHTML = `${status(loopStatus.label)} <span>${esc(loopState.current_phase || 'idle')}</span> <span>work: ${esc(work)}</span>`;
  $('status-strip').innerHTML = [
    ['Run', loopState.run_id ? `<a href="/runs/${route(loopState.run_id)}">${esc(loopState.run_id)}</a>` : 'none'],
    ['Next work', context.next_work_item ? `<a href="/work/${route(context.next_work_item.slug)}">${esc(context.next_work_item.slug)}</a>` : 'none'],
    ['Decision', context.latest_decision ? esc(context.latest_decision.outcome) : 'none'],
    ['Artifact', context.latest_artifacts && context.latest_artifacts[0] ? `<button type="button" class="link-button" data-action="select-artifact" data-artifact-id="${esc(context.latest_artifacts[0].artifact_id)}">${esc(artifactName(context.latest_artifacts[0].path))}</button>` : 'none']
  ].map(([label, value]) => `<div class="status-cell"><span>${esc(label)}</span><strong>${value}</strong></div>`).join('');
  $('inspector-state').className = `pill compact ${loopStatus.className}`;
  $('inspector-state').textContent = loopStatus.label;
  $('control-count').textContent = String(pendingInterventions(context).length);
}

function renderCurrent(context, missionLog) {
  const next = context.next_work_item;
  $('next-work-state').textContent = next ? text(next.status) : 'none';
  $('next-work').innerHTML = next
    ? `<div class="record-title"><a href="/work/${route(next.slug)}">${esc(next.slug)}</a>${status(next.status)}</div><p>${esc(next.title)}</p><details class="inline-details"><summary>More about this work</summary><p class="muted">${esc(next.description)}</p></details>`
    : `<p class="muted">No pending work item. The ledger is waiting for new scope or operator direction.</p>`;

  const activeRun = (context.active_runs || [])[0];
  const loopState = context.loop_state || {};
  $('active-run-state').textContent = activeRun ? `run ${activeRun.run_id}` : text(loopState.current_phase || 'none');
  $('active-run').innerHTML = activeRun
    ? renderActiveRun(activeRun)
    : renderCompletedLoop(loopState);

  const latestObservation = (context.latest_observations || [])[0];
  $('latest-observation').innerHTML = latestObservation
    ? `<div class="record-title"><a href="/runs/${latestObservation.run_id}">observation ${esc(latestObservation.observation_id)}</a><span class="muted">${esc(latestObservation.created_at)}</span></div><p>“${esc(latestObservation.body)}”</p><details class="inline-details"><summary>Observation history</summary>${renderObservationHistory(context.latest_observations || [])}</details>`
    : '<p class="muted">No observations recorded.</p>';

  $('recent-decision').innerHTML = context.latest_decision
    ? renderCompactDecision(context.latest_decision)
    : '<p class="muted">No decisions recorded.</p>';

  renderCurrentInspector(context, missionLog);
}

function renderCompactDecision(decision) {
  const next = decision.next_work_slug
    ? `<a href="/work/${route(decision.next_work_slug)}">next: ${esc(decision.next_work_slug)}</a>`
    : '<span class="muted">terminal decision</span>';
  return `<div class="record-title"><strong>${esc(decision.outcome)}</strong><span class="muted">${esc(decision.created_at)}</span></div><p>${esc(decision.outcome)} → ${next}</p><details class="inline-details"><summary>Decision rationale</summary><p>${esc(decision.rationale)}</p></details>`;
}

function renderObservationHistory(observations) {
  return observations.length
    ? observations.slice(0, 8).map(observation => `<div class="timeline-row"><span>${esc(observation.created_at)} | ${esc(observation.work_slug)}</span><p>${esc(observation.body)}</p></div>`).join('')
    : '<p class="muted">No observations recorded.</p>';
}

function renderActiveRun(run) {
  return `<div class="record-title"><a href="/runs/${run.run_id}">run ${esc(run.run_id)}</a><a href="/work/${route(run.work_slug)}">${esc(run.work_slug)}</a></div><p>${esc(run.work_title)}</p><details class="inline-details"><summary>Run command</summary><pre>${esc(run.command || 'No command recorded')}</pre></details><p class="muted">started ${esc(run.started_at)}</p>`;
}

function renderCurrentInspector(context, missionLog) {
  const title = inspectorTitle(selectedInspector);
  $('inspector-heading').textContent = title;
  document.querySelectorAll('.selectable-panel').forEach(panel => {
    panel.classList.toggle('selected', panel.dataset.inspector === selectedInspector);
  });
  $('current-inspector-body').innerHTML = inspectorBody(selectedInspector, context, missionLog);
}

function inspectorTitle(target) {
  return ({
    overview: 'Inspector',
    'next-work': 'Next Work Inspector',
    'active-run': 'Run Inspector',
    'latest-observation': 'Observation Inspector',
    'recent-decision': 'Decision Inspector',
    artifacts: 'Artifact Inspector',
    runs: 'Run History'
  })[target] || 'Inspector';
}

function inspectorBody(target, context, missionLog) {
  if (target === 'next-work') return inspectNextWork(context, missionLog);
  if (target === 'active-run') return inspectActiveRun(context, missionLog);
  if (target === 'latest-observation') return inspectObservations(context);
  if (target === 'recent-decision') return inspectDecision(context, missionLog);
  if (target === 'artifacts') return inspectArtifacts(context);
  if (target === 'runs') return inspectRuns(context, missionLog);
  return `${inspectActiveRun(context, missionLog)}${inspectArtifacts(context)}`;
}

function inspectNextWork(context, missionLog) {
  const next = context.next_work_item;
  if (!next) return '<p class="muted">No queued work to inspect.</p>';
  const entry = (missionLog.entries || []).find(item => item.slug === next.slug);
  return `<section class="inspector-section"><div class="record-title"><a href="/work/${route(next.slug)}">${esc(next.slug)}</a>${status(next.status)}</div><p>${esc(next.title)}</p><p class="muted">${esc(next.description)}</p></section>${entry ? inspectEntryHistory(entry) : '<p class="muted">No recent mission-log history for this work.</p>'}`;
}

function inspectActiveRun(context, missionLog) {
  const activeRun = (context.active_runs || [])[0];
  if (activeRun) {
    const entry = (missionLog.entries || []).find(item => item.slug === activeRun.work_slug);
    return `<section class="inspector-section">${renderActiveRun(activeRun)}</section>${entry ? inspectEntryHistory(entry) : ''}`;
  }
  return `<section class="inspector-section">${renderCompletedLoop(context.loop_state || {})}</section>`;
}

function inspectObservations(context) {
  return `<section class="inspector-section"><h4>Recent observations</h4>${renderObservationHistory(context.latest_observations || [])}</section>`;
}

function inspectDecision(context, missionLog) {
  if (!context.latest_decision) return '<p class="muted">No decision to inspect.</p>';
  return renderDecisionCard(context.latest_decision, relatedEntryForDecision(missionLog, context.latest_decision), true);
}

function inspectArtifacts(context) {
  return `<section class="inspector-section"><h4>Latest artifacts</h4>${renderArtifactButtons(context.latest_artifacts || [])}</section>`;
}

function inspectRuns(context, missionLog) {
  const active = (context.active_runs || []).map(run => `<div class="compact-record"><strong><a href="/runs/${route(run.run_id)}">run ${esc(run.run_id)}</a></strong><p class="muted">${esc(run.work_slug)} | ${esc(run.started_at)}</p></div>`).join('');
  const historic = (missionLog.entries || []).flatMap(entry => (entry.runs || []).map(run => ({entry, run}))).slice(0, 8)
    .map(item => `<div class="compact-record"><strong><a href="/runs/${route(item.run.run_id)}">run ${esc(item.run.run_id)}</a></strong>${status(item.run.status)}<p class="muted">${esc(item.entry.slug)} | ${esc(item.run.finished_at || item.run.started_at)}</p></div>`).join('');
  return `<section class="inspector-section"><h4>Active</h4>${active || '<p class="muted">No active runs.</p>'}</section><section class="inspector-section"><h4>Recent history</h4>${historic || '<p class="muted">No run history in the current window.</p>'}</section>`;
}

function inspectEntryHistory(entry) {
  const runs = (entry.runs || []).slice(0, 5).map(run => `<div class="compact-record"><strong><a href="/runs/${route(run.run_id)}">run ${esc(run.run_id)}</a></strong>${status(run.status)}<p class="muted">${esc(run.started_at)} → ${esc(run.finished_at || 'pending')}</p></div>`).join('');
  const observations = (entry.runs || []).flatMap(run => run.observations || []).slice(0, 5).map(observation => `<p class="context-line">${esc(observation.body)}</p>`).join('');
  const artifacts = (entry.runs || []).flatMap(run => run.artifacts || []).slice(0, 5);
  return `<section class="inspector-section"><h4>Historic runs</h4>${runs || '<p class="muted">No runs recorded in this window.</p>'}</section><section class="inspector-section"><h4>Historic observations</h4>${observations || '<p class="muted">No observations recorded in this window.</p>'}</section><section class="inspector-section"><h4>Linked artifacts</h4>${artifacts.length ? renderArtifactButtons(artifacts) : '<p class="muted">No artifacts recorded in this window.</p>'}</section>`;
}

function renderCompletedLoop(loopState) {
  const narrative = (loopState.recent_cycle_narrative || []).map(entry => `<div class="timeline-row"><span>${esc(entry.created_at)}</span><p>${esc(entry.message)}</p></div>`).join('');
  return `<p>${esc(loopState.progress_report || 'No active run.')}</p><details class="inline-details"><summary>Recent cycle narrative</summary>${narrative || '<p class="muted">No recent cycle narrative.</p>'}</details>`;
}

function renderDecisionTrail(missionLog) {
  const decisions = allDecisions(missionLog)
    .filter(item => decisionMatches(item, decisionFilterTerm))
    .sort((left, right) => text(right.decision.created_at).localeCompare(text(left.decision.created_at)));
  $('decision-trail').innerHTML = decisions.length
    ? decisions.map(item => renderDecisionCard(item.decision, item.entry, true)).join('')
    : '<p class="muted">No decisions match this filter.</p>';
}

function allDecisions(missionLog) {
  return (missionLog.entries || []).flatMap(entry => (entry.decisions || []).map(decision => ({entry, decision})));
}

function decisionMatches(item, term) {
  if (!term) return true;
  const haystack = [
    item.entry.slug,
    item.entry.title,
    item.decision.outcome,
    item.decision.rationale,
    item.decision.next_work_slug
  ].join(' ').toLowerCase();
  return haystack.includes(term.toLowerCase());
}

function relatedEntryForDecision(missionLog, decision) {
  return (missionLog.entries || []).find(entry => entry.slug === decision.work_slug);
}

function renderDecisionCard(decision, entry, expanded) {
  const runs = entry ? entry.runs || [] : [];
  const latestRun = runs[0];
  const observations = runs.flatMap(run => run.observations || []).slice(0, 3);
  const artifacts = runs.flatMap(run => run.artifacts || []).slice(0, 5);
  const next = decision.next_work_slug
    ? `<a href="/work/${route(decision.next_work_slug)}">next: ${esc(decision.next_work_slug)}</a>`
    : '<span class="muted">terminal decision</span>';
  return `<details class="decision-card" ${expanded ? 'open' : ''}>
    <summary>
      <span class="decision-outcome">DECISION ${esc(decision.outcome)}</span>
      <span class="decision-work">${esc(decision.work_slug)}</span>
      <span class="decision-time">${esc(decision.created_at)}</span>
    </summary>
    <div class="decision-body">
      <p>${esc(decision.rationale)}</p>
      <div class="decision-links">${next}${latestRun ? `<a href="/runs/${route(latestRun.run_id)}">linked run ${esc(latestRun.run_id)}</a>` : '<span class="muted">no linked run in mission log window</span>'}</div>
      <div class="context-grid">
        <section><h4>Observations</h4>${observations.length ? observations.map(observation => `<p class="context-line">${esc(observation.body)}</p>`).join('') : '<p class="muted">No observations linked in the current window.</p>'}</section>
        <section><h4>Artifacts</h4>${artifacts.length ? renderArtifactButtons(artifacts) : '<p class="muted">No artifacts linked in the current window.</p>'}</section>
      </div>
    </div>
  </details>`;
}

function renderArtifactView(missionLog) {
  const artifacts = allArtifacts(missionLog).filter(item => artifactMatches(item, artifactSearchTerm));
  $('artifact-list').innerHTML = artifacts.length
    ? artifacts.map(item => renderArtifactListItem(item)).join('')
    : '<p class="muted">No artifacts match this search.</p>';
  if (!selectedArtifactId && artifacts[0]) {
    selectedArtifactId = artifacts[0].artifact.artifact_id;
    openArtifact(selectedArtifactId, {replaceHistory: false});
  }
}

function allArtifacts(missionLog) {
  const byId = new Map();
  for (const entry of missionLog.entries || []) {
    for (const run of entry.runs || []) {
      for (const artifact of run.artifacts || []) {
        if (!byId.has(artifact.artifact_id)) byId.set(artifact.artifact_id, {entry, run, artifact});
      }
    }
  }
  return Array.from(byId.values()).sort((left, right) => text(right.artifact.created_at).localeCompare(text(left.artifact.created_at)));
}

function artifactMatches(item, term) {
  if (!term) return true;
  const haystack = [
    item.entry.slug,
    item.entry.title,
    item.run.run_id,
    item.artifact.kind,
    item.artifact.path,
    item.artifact.description
  ].join(' ').toLowerCase();
  return haystack.includes(term.toLowerCase());
}

function renderArtifactListItem(item) {
  const selected = Number(selectedArtifactId) === Number(item.artifact.artifact_id) ? ' selected' : '';
  return `<button type="button" class="artifact-list-item${selected}" data-action="select-artifact" data-artifact-id="${esc(item.artifact.artifact_id)}">
    <strong>${esc(artifactName(item.artifact.path))}</strong>
    <span>${esc(item.artifact.kind)} | run ${esc(item.run.run_id)} | ${esc(item.entry.slug)}</span>
  </button>`;
}

function renderArtifactButtons(artifacts) {
  return artifacts.length
    ? `<div class="artifact-buttons">${artifacts.map(artifact => `<button type="button" class="artifact-button" data-action="open-artifact" data-artifact-id="${esc(artifact.artifact_id)}">${esc(artifact.kind)} ${esc(artifactName(artifact.path))}</button>`).join('')}</div>`
    : '<p class="muted">No artifacts recorded.</p>';
}

async function openArtifact(rawId, options = {}) {
  const id = decodeArtifactId(rawId);
  selectedArtifactId = Number(id);
  const encodedId = encodeURIComponent(id);
  const response = await fetch(`/api/artifacts/${encodedId}`);
  if (!response.ok) {
    $('artifact-reader').innerHTML = `<pre>Failed to open artifact ${esc(id)}\n\n${esc(await apiErrorMessage(response))}</pre>`;
    if (options.showDetail) {
      $('detail-panel').classList.add('visible');
      $('detail-log').innerHTML = $('artifact-reader').innerHTML;
    }
    return;
  }
  const data = await response.json();
  const backlink = backlinkForArtifact(Number(id));
  $('reader-title').textContent = `Artifact ${id}: ${artifactName(data.artifact.path)}`;
  $('reader-raw-link').href = data.raw_url;
  $('reader-raw-link').hidden = false;
  $('artifact-reader').innerHTML = renderArtifactReader(data, backlink);
  if (options.showDetail) {
    $('detail-panel').classList.add('visible');
    $('detail-log').innerHTML = $('artifact-reader').innerHTML;
  }
  if (options.replaceHistory !== false) {
    history.replaceState(null, '', `/artifacts/${encodedId}`);
    currentDetailRoute = location.pathname;
  }
  renderArtifactView(lastMissionLog || {entries: []});
}

function decodeArtifactId(rawId) {
  try {
    return decodeURIComponent(String(rawId));
  } catch {
    return String(rawId);
  }
}

function backlinkForArtifact(artifactId) {
  return allArtifacts(lastMissionLog || {entries: []}).find(item => Number(item.artifact.artifact_id) === Number(artifactId));
}

function renderArtifactReader(data, backlink) {
  const links = backlink
    ? `<div class="backlinks">
        <a href="/runs/${route(backlink.run.run_id)}">produced by run ${esc(backlink.run.run_id)}</a>
        <a href="/work/${route(backlink.entry.slug)}">work ${esc(backlink.entry.slug)}</a>
      </div>`
    : '<p class="muted">No mission-log backlink found in the current window.</p>';
  if (data.viewer === 'image') {
    return `${links}<img class="artifact-preview" src="${esc(data.raw_url)}" alt="artifact ${esc(data.artifact.artifact_id)}">`;
  }
  let content = data.content || '';
  if (data.viewer === 'json') {
    try {
      content = JSON.stringify(JSON.parse(content), null, 2);
    } catch {}
    return `${links}<pre>${esc(content)}</pre>`;
  }
  if (data.viewer === 'markdown') {
    return `${links}<div class="markdown-reader">${renderMarkdown(content)}</div>`;
  }
  if (data.viewer === 'csv') {
    content = content.split('\n').slice(0, 80).join('\n');
  }
  return `${links}<pre>${esc(content || data.artifact.description || 'No text preview available.')}</pre>`;
}

function renderMarkdown(markdown) {
  const lines = text(markdown).split('\n');
  const html = [];
  let inList = false;
  for (const line of lines) {
    if (line.startsWith('# ')) {
      if (inList) { html.push('</ul>'); inList = false; }
      html.push(`<h1>${esc(line.slice(2))}</h1>`);
    } else if (line.startsWith('## ')) {
      if (inList) { html.push('</ul>'); inList = false; }
      html.push(`<h2>${esc(line.slice(3))}</h2>`);
    } else if (line.startsWith('### ')) {
      if (inList) { html.push('</ul>'); inList = false; }
      html.push(`<h3>${esc(line.slice(4))}</h3>`);
    } else if (line.startsWith('- ')) {
      if (!inList) { html.push('<ul>'); inList = true; }
      html.push(`<li>${esc(line.slice(2))}</li>`);
    } else if (!line.trim()) {
      if (inList) { html.push('</ul>'); inList = false; }
    } else {
      if (inList) { html.push('</ul>'); inList = false; }
      html.push(`<p>${esc(line)}</p>`);
    }
  }
  if (inList) html.push('</ul>');
  return html.join('');
}

function renderWaveManagement(conduct) {
  const root = $('wave-management');
  if (!root) return;
  if (!conduct || !conduct.available) {
    root.innerHTML = `<article class="panel"><p class="muted">Conduct wave state unavailable: ${esc(conduct && (conduct.message || conduct.error) || 'not loaded')}</p></article>`;
    return;
  }
  const batches = conduct.batches || [];
  const globalFeeds = conduct.feeds || [];
  root.innerHTML = `${renderWaveBatches(batches)}${renderGlobalFeeds(globalFeeds)}`;
}

function renderWaveBatches(batches) {
  if (!batches.length) return '<article class="panel"><p class="muted">No .ldgr/.conduct worker batches found.</p></article>';
  return batches.map(batch => {
    const workers = batch.workers || [];
    const counts = workerCounts(workers);
    return `<article class="panel wave-batch">
      <div class="panel-head"><h3>${esc(batch.batch_id)}</h3><span class="panel-stat">${esc(batch.worker_count || workers.length)} workers</span></div>
      <div class="status-strip compact-strip">${Object.entries(counts).map(([key, value]) => `<div class="status-cell"><span>${esc(key)}</span><strong>${esc(value)}</strong></div>`).join('')}</div>
      <div class="worker-grid">${workers.map(renderWorkerCard).join('')}</div>
    </article>`;
  }).join('');
}

function workerCounts(workers) {
  const counts = {running: 0, done: 0, blocked: 0, dirty: 0};
  for (const worker of workers) {
    const state = workerState(worker);
    if (state === 'running') counts.running += 1;
    else if (state === 'done') counts.done += 1;
    else counts.blocked += 1;
    if (worker.git && worker.git.dirty) counts.dirty += 1;
  }
  return counts;
}

function workerState(worker) {
  const ldgr = worker.worker_ldgr || {};
  if ((ldgr.active_run_count || 0) > 0 || !ldgr.terminal_status && ldgr.phase === 'started') return 'running';
  if (ldgr.terminal_status === 'success') return 'done';
  if (ldgr.terminal_status) return ldgr.terminal_status;
  return 'unknown';
}

function renderWorkerCard(worker) {
  const ldgr = worker.worker_ldgr || {};
  const state = workerState(worker);
  const feeds = (worker.feeds || []).slice(0, 3);
  const files = worker.git && worker.git.files && worker.git.files.length
    ? `<details class="inline-details"><summary>Changed files</summary><pre>${esc(worker.git.files.join('\n'))}</pre></details>`
    : '<p class="muted">worktree clean or unavailable</p>';
  return `<section class="worker-card">
    <div class="record-title"><strong>${esc(worker.worker_id)}</strong>${status(state)}${worker.git && worker.git.dirty ? status('dirty') : ''}</div>
    <p><strong>${esc(worker.ticket_slug)}</strong></p>
    <p class="muted">worker DB: ${esc(worker.worker_db)}</p>
    <p class="muted">phase: ${esc(ldgr.phase || 'unknown')} run: ${esc(ldgr.run_id || 'none')} decision: ${esc(ldgr.latest_decision || 'none')}</p>
    ${ldgr.latest_observation ? `<p class="context-line">${esc(ldgr.latest_observation)}</p>` : '<p class="muted">No worker observation summary.</p>'}
    ${files}
    ${feeds.length ? `<details class="inline-details" open><summary>Feeds</summary>${feeds.map(renderFeed).join('')}</details>` : '<p class="muted">No stdout/stderr feeds found.</p>'}
  </section>`;
}

function renderGlobalFeeds(feeds) {
  return `<article class="panel"><div class="panel-head"><h3>Conduct feed tail</h3><span class="panel-stat">${esc(feeds.length)} feeds</span></div>${feeds.length ? feeds.map(renderFeed).join('') : '<p class="muted">No conduct log feeds found.</p>'}</article>`;
}

function renderFeed(feed) {
  return `<div class="feed-card"><div class="feed-meta"><strong>${esc(feed.label)}</strong><span>${esc(feed.path)}</span><span>${esc(feed.size)} bytes</span></div><pre>${esc(feed.tail || '')}</pre></div>`;
}

function renderOperatorControls(draft) {
  const dryRunChecked = draft.dryRun ? ' checked' : '';
  const projectCompleteChecked = draft.projectComplete ? ' checked' : '';
  return `
    <div class="control-grid">
      <button type="button" class="control-button" data-action="request-intervention" data-intervention="pause">Pause next cycle</button>
      <button type="button" class="control-button" data-action="request-intervention" data-intervention="resume">Resume paused loop</button>
      <button type="button" class="control-button danger" data-action="request-intervention" data-intervention="stop">Stop next cycle</button>
      <button type="button" class="control-button" data-action="request-intervention" data-intervention="steer">Add steering</button>
    </div>
    <label for="control-reason">Reason</label><input id="control-reason" value="${esc(draft.reason)}">
    <label for="control-instruction">Instruction for the next loop prompt</label><textarea id="control-instruction">${esc(draft.instruction)}</textarea>
    <details class="inline-details">
      <summary>Loop start options</summary>
      ${renderLoopStart(draft, dryRunChecked, projectCompleteChecked)}
    </details>
    <p id="control-status" class="muted">${esc(draft.status)}</p>`;
}

function renderLoopStart(draft, dryRunChecked, projectCompleteChecked) {
  const streamAgentOutputChecked = draft.streamAgentOutput ? ' checked' : '';
  return `
    <label for="loop-prompt">Prompt path</label><input id="loop-prompt" value="${esc(draft.prompt)}">
    <label for="loop-prompt-slug">Prompt slug</label><input id="loop-prompt-slug" value="${esc(draft.promptSlug)}">
    <label for="loop-bundle">Bundle slug</label><input id="loop-bundle" value="${esc(draft.bundle)}">
    <label for="loop-prompt-role">Bundle prompt role</label><input id="loop-prompt-role" value="${esc(draft.promptRole)}">
    <label for="loop-agent">Agent</label><select id="loop-agent"><option value="agentctl"${draft.agent === 'agentctl' ? ' selected' : ''}>agentctl</option></select>
    <label for="loop-agent-argv">Agent argv JSON</label><textarea id="loop-agent-argv" placeholder='optional, e.g. ["my-agent"]'>${esc(draft.agentArgv)}</textarea>
    <label for="loop-agent-timeout-seconds">Agent timeout seconds</label><input id="loop-agent-timeout-seconds" type="number" min="1" step="1" value="${esc(draft.agentTimeoutSeconds)}">
    <label for="loop-audit-argv">Audit argv JSON</label><textarea id="loop-audit-argv">${esc(draft.auditArgv)}</textarea>
    <label for="loop-max-iterations">Max iterations</label><input id="loop-max-iterations" type="number" min="1" step="1" value="${esc(draft.maxIterations)}">
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
  const body = new URLSearchParams({
    prompt: $('loop-prompt').value.trim(),
    prompt_slug: $('loop-prompt-slug').value.trim(),
    bundle: $('loop-bundle').value.trim(),
    prompt_role: $('loop-prompt-role').value.trim(),
    agent: agentArgv ? '' : $('loop-agent').value,
    agent_argv: agentArgv,
    agent_timeout_seconds: $('loop-agent-timeout-seconds').value.trim(),
    audit_argv: $('loop-audit-argv').value.trim(),
    dry_run: $('loop-dry-run').checked ? 'true' : 'false',
    stream_agent_output: $('loop-stream-agent-output').checked ? 'true' : 'false',
    max_iterations: $('loop-max-iterations').value.trim(),
    project_complete_requested: $('loop-project-complete').checked ? 'true' : 'false'
  });
  const response = await fetch('/api/loop/start', {method: 'POST', headers: controlHeaders(), body});
  if (!response.ok) {
    statusNode.textContent = await apiErrorMessage(response);
    return;
  }
  const result = await response.json();
  statusNode.textContent = `${result.message || 'Loop process started.'} pid ${result.pid}.`;
  if (document.activeElement) document.activeElement.blur();
  lastSnapshotJson = '';
  await load();
}

async function requestIntervention(action) {
  const reason = $('control-reason').value.trim();
  const instruction = $('control-instruction').value.trim();
  const body = new URLSearchParams({reason});
  if (action === 'steer') body.set('instruction', instruction);
  await postControl(`/api/loop/interventions/${action}`, body);
}

async function clearIntervention(id) {
  const reason = $('control-reason').value.trim() || 'Operator cleared from cockpit';
  await postControl(`/api/loop/interventions/clear/${id}`, new URLSearchParams({reason}));
}

async function postControl(url, body) {
  const statusNode = $('control-status');
  statusNode.textContent = 'Writing control event...';
  const response = await fetch(url, {method: 'POST', headers: controlHeaders(), body});
  if (!response.ok) {
    statusNode.textContent = await apiErrorMessage(response);
    return;
  }
  statusNode.textContent = 'Control event recorded.';
  if (document.activeElement) document.activeElement.blur();
  lastSnapshotJson = '';
  await load();
}

function eventTitle(event) {
  const entity = `${event.entity_type}:${event.entity_id}`;
  if (event.entity_type === 'loop_intervention') return `Loop intervention ${event.entity_id} ${event.event_type}`;
  if (event.entity_type === 'global_observation') return `Global observation ${event.entity_id} ${event.event_type}`;
  if (event.entity_type === 'run') return `Run ${event.entity_id} ${event.event_type}`;
  if (event.entity_type === 'work_item') return `Work item ${event.entity_id} ${event.event_type}`;
  return `event ${event.event_id} | ${entity} | ${event.event_type}`;
}

function renderEventPayload(event) {
  const payload = parsePayload(event.payload_json);
  if (!payload || Object.keys(payload).length === 0) return '<p class="muted">No event details.</p>';
  const fields = Object.entries(payload)
    .filter(([, value]) => value !== null && value !== undefined)
    .map(([key, value]) => `<div><span class="muted">${esc(labelize(key))}</span>: ${esc(formatValue(value))}</div>`)
    .join('');
  return `<div class="event-fields">${fields}</div>`;
}

function parsePayload(payloadJson) {
  try {
    return JSON.parse(payloadJson || '{}');
  } catch {
    return {raw: payloadJson};
  }
}

function labelize(key) {
  return key.replaceAll('_', ' ');
}

function formatValue(value) {
  if (Array.isArray(value)) return value.join(', ');
  if (typeof value === 'object') return JSON.stringify(value);
  return value;
}

function list(items, renderItem) {
  return items && items.length
    ? items.map(item => `<div class="item">${renderItem(item)}</div>`).join('')
    : '<p class="muted">None recorded.</p>';
}

function artifactName(path) {
  const parts = text(path).split('/');
  return parts[parts.length - 1] || text(path);
}

function setView(view) {
  currentView = view;
  document.querySelectorAll('.nav-button').forEach(button => button.classList.toggle('active', button.dataset.view === view));
  document.querySelectorAll('.view').forEach(section => section.classList.toggle('active-view', section.id === `view-${view}`));
  if (location.pathname === '/' || location.pathname === '/index.html') $('detail-panel').classList.remove('visible');
  if (location.hash !== `#${view}`) history.replaceState(null, '', `#${view}`);
  if (view === 'waves') {
    renderWaveManagement(lastConduct || {available: false, message: 'Loading conduct wave state...', batches: [], feeds: []});
    loadConduct();
  }
}

function initialViewFromHash() {
  const candidate = location.hash.replace('#', '');
  return ['current', 'decisions', 'artifacts', 'waves'].includes(candidate) ? candidate : 'current';
}

function renderMissionLog() {
  renderDecisionTrail(lastMissionLog || {entries: []});
  renderArtifactView(lastMissionLog || {entries: []});
}

function setInspector(target) {
  selectedInspector = target || 'overview';
  setView('current');
  if (lastContext && lastMissionLog) renderCurrent(lastContext, lastMissionLog);
}

function updateSearch(id, assign) {
  const node = $(id);
  if (!node) return;
  node.addEventListener('input', () => {
    assign(node.value.trim());
    render();
  });
}

async function renderRoute() {
  const path = location.pathname;
  if (path === '/' || path === '/index.html') {
    currentDetailRoute = '';
    return;
  }
  $('detail-panel').classList.add('visible');
  if (path === currentDetailRoute) return;
  currentDetailRoute = path;
  if (path === '/logs') return detailJson('/api/logs', 'Durable event log');
  if (path.startsWith('/runs/')) return detailJson('/api/runs/' + encodedRouteSegment('/runs/'), 'Run detail');
  if (path.startsWith('/work/')) return detailJson('/api/work/' + encodedRouteSegment('/work/'), 'Work item detail');
  if (path.startsWith('/artifacts/')) return openArtifact(encodedRouteSegment('/artifacts/'), {showDetail: true});
}

async function detailJson(url, title) {
  try {
    const data = await apiJson(url);
    $('detail-log').innerHTML = `<h3>${esc(title)}</h3><pre>${esc(JSON.stringify(data, null, 2))}</pre>`;
  } catch (error) {
    $('detail-log').innerHTML = `<h3>${esc(title)}</h3><pre>${esc(error.message || error)}</pre>`;
  }
}

document.addEventListener('click', event => {
  const button = event.target.closest('[data-action]');
  if (!button) return;
  const action = button.dataset.action;
  if (action === 'set-view') setView(button.dataset.view);
  else if (action === 'set-inspector') setInspector(button.dataset.inspector);
  else if (action === 'select-artifact' || action === 'open-artifact') {
    setView('artifacts');
    openArtifact(button.dataset.artifactId, {replaceHistory: false});
  } else if (action === 'set-mission-filter') {
    setView('decisions');
  } else if (action === 'request-intervention') requestIntervention(button.dataset.intervention);
  else if (action === 'start-loop') startLoop();
  else if (action === 'clear-intervention') clearIntervention(button.dataset.interventionId);
});

document.addEventListener('keydown', event => {
  if (event.key !== 'Enter' && event.key !== ' ') return;
  const panel = event.target.closest('.selectable-panel[data-inspector]');
  if (!panel) return;
  event.preventDefault();
  setInspector(panel.dataset.inspector);
});

updateSearch('artifact-search', value => { artifactSearchTerm = value; });
updateSearch('decision-filter', value => { decisionFilterTerm = value; });

load().catch(error => {
  $('status-summary').innerHTML = '<span class="muted">Failed to load cockpit: ' + esc(error) + '</span>';
});
setView(currentView);
window.addEventListener('hashchange', () => setView(initialViewFromHash()));
setInterval(load, 5000);
