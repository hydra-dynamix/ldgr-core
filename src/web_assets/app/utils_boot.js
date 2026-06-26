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
