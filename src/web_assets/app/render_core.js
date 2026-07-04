function render() {
  if (!lastContext || !lastMissionLog) return;
  const context = lastContext;
  const missionLog = lastMissionLog;
  renderShell(context);
  renderCurrent(context, missionLog);
  renderDecisionTrail(missionLog);
  renderArtifactView(missionLog);
  renderWaveManagement(lastConduct);
  renderSidebar(context, missionLog, lastConduct);
  const pending = pendingInterventions(context);
  $('operator-controls').innerHTML = renderOperatorControls(controlDraft(), pending);
  $('controls-status').textContent = `${pending.length} pending`;
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
  const activeRun = (context.active_runs || [])[0];
  const latestDecision = context.latest_decision;
  const latestArtifact = context.latest_artifacts && context.latest_artifacts[0];
  const latestObservation = context.latest_observations && context.latest_observations[0];
  $('status-strip').innerHTML = [
    ['Active run', activeRun ? `run ${esc(activeRun.run_id)} · ${esc(activeRun.work_slug)}` : esc(loopState.current_phase || 'none'), 'active-run'],
    ['Next work', context.next_work_item ? `${esc(context.next_work_item.slug)} · ${esc(context.next_work_item.status)}` : 'none', 'next-work'],
    ['Latest decision', latestDecision ? `${esc(latestDecision.outcome)} · ${esc(latestDecision.work_slug)}` : 'none', 'recent-decision'],
    ['Latest artifact', latestArtifact ? `${esc(latestArtifact.kind)} · ${esc(artifactName(latestArtifact.path))}` : 'none', 'artifacts'],
    ['Latest observation', latestObservation ? `${esc(latestObservation.created_at)} · ${esc(latestObservation.body)}` : 'none', 'latest-observation']
  ].map(([label, value, inspector]) => `<button type="button" class="status-cell" data-action="set-inspector" data-inspector="${esc(inspector)}"><span>${esc(label)}</span><strong>${value}</strong></button>`).join('');
  $('inspector-state').className = `pill compact ${loopStatus.className}`;
  $('inspector-state').textContent = loopStatus.label;
  $('control-count').textContent = String(pendingInterventions(context).length);
}

function renderCurrent(context, missionLog) {
  renderCurrentInspector(context, missionLog);
}

function renderSidebar(context, missionLog, conduct) {
  renderSidebarCurrent(context);
  renderSidebarDecisions(missionLog);
  renderSidebarArtifacts(missionLog);
  renderSidebarWaves(conduct);
  renderSidebarRuns(missionLog);
}

function renderSidebarCurrent(context) {
  const activeRun = (context.active_runs || [])[0];
  const latestDecision = context.latest_decision;
  const latestArtifact = context.latest_artifacts && context.latest_artifacts[0];
  const latestObservation = context.latest_observations && context.latest_observations[0];
  $('nav-current').innerHTML = [
    navButton('Active run', activeRun ? `run ${activeRun.run_id}` : 'none', 'set-inspector', {'inspector': 'active-run'}),
    navButton('Next work', context.next_work_item ? context.next_work_item.slug : 'none', 'set-inspector', {'inspector': 'next-work'}),
    navButton('Latest decision', latestDecision ? latestDecision.work_slug : 'none', 'set-inspector', {'inspector': 'recent-decision'}),
    navButton('Latest artifact', latestArtifact ? artifactName(latestArtifact.path) : 'none', 'set-inspector', {'inspector': 'artifacts'}),
    navButton('Latest observation', latestObservation ? latestObservation.created_at : 'none', 'set-inspector', {'inspector': 'latest-observation'})
  ].join('');
}

function renderSidebarDecisions(missionLog) {
  const decisions = allDecisions(missionLog)
    .sort((left, right) => text(right.decision.created_at).localeCompare(text(left.decision.created_at)))
    .slice(0, 8);
  $('nav-decisions').innerHTML = decisions.length
    ? decisions.map(item => navButton(item.decision.outcome, item.decision.work_slug, 'set-mission-filter', {'filter': item.entry.slug})).join('')
    : '<p class="muted nav-empty">No decisions.</p>';
}

function renderSidebarArtifacts(missionLog) {
  const artifacts = allArtifacts(missionLog).slice(0, 12);
  $('nav-artifacts').innerHTML = artifacts.length
    ? artifacts.map(item => navButton(artifactName(item.artifact.path), `${item.artifact.kind} · run ${item.run.run_id}`, 'select-artifact', {'artifact-id': item.artifact.artifact_id})).join('')
    : '<p class="muted nav-empty">No artifacts.</p>';
}

function renderSidebarWaves(conduct) {
  if (!conduct || !conduct.available) {
    $('nav-waves').innerHTML = navButton('Wave management', 'loading', 'set-view', {'view': 'waves'});
    return;
  }
  const batches = (conduct.batches || []).slice(0, 8);
  $('nav-waves').innerHTML = batches.length
    ? batches.map(batch => navButton(batch.batch_id, `${batch.worker_count || (batch.workers || []).length} workers`, 'set-view', {'view': 'waves'})).join('')
    : '<p class="muted nav-empty">No waves.</p>';
}

function renderSidebarRuns(missionLog) {
  const runs = allRuns(missionLog).slice(0, 10);
  $('nav-runs').innerHTML = runs.length
    ? runs.map(item => navButton(`run ${item.run.run_id}`, item.entry.slug, 'open-run-detail', {'run-id': item.run.run_id})).join('')
    : '<p class="muted nav-empty">No runs.</p>';
}

function navButton(title, meta, action, dataset) {
  const attrs = Object.entries(dataset || {})
    .map(([key, value]) => ` data-${key}="${esc(value)}"`)
    .join('');
  return `<button type="button" class="nav-subitem" data-action="${esc(action)}"${attrs}><strong>${esc(title)}</strong><span>${esc(meta)}</span></button>`;
}

function allRuns(missionLog) {
  return (missionLog.entries || [])
    .flatMap(entry => (entry.runs || []).map(run => ({entry, run})))
    .sort((left, right) => text(right.run.started_at || right.run.finished_at).localeCompare(text(left.run.started_at || left.run.finished_at)));
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
  return `${inspectActiveRun(context, missionLog)}${inspectNextWork(context, missionLog)}${inspectArtifacts(context)}`;
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

