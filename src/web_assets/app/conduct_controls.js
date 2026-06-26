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
  if (!batches.length) return '<article class="panel"><p class="muted">No .ldgr-conduct worker batches found.</p></article>';
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

