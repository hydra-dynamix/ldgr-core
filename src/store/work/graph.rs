#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkGraph {
    pub nodes: Vec<WorkItemView>,
    pub edges: Vec<WorkGraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkGraphEdge {
    pub child: String,
    pub prerequisite: String,
    pub satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkAudit {
    pub ok: bool,
    pub roots: Vec<String>,
    pub terminal_nodes: Vec<String>,
    pub findings: Vec<WorkAuditFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkAuditFinding {
    pub code: String,
    pub message: String,
    pub work_items: Vec<String>,
}

pub fn build_work_graph(connection: &Connection) -> anyhow::Result<WorkGraph> {
    let nodes = list_work_item_views_filtered(connection, None, None, None)?;
    let edges = nodes
        .iter()
        .flat_map(|node| {
            node.dependencies
                .iter()
                .map(|dependency| WorkGraphEdge {
                    child: node.work_item.slug.clone(),
                    prerequisite: dependency.slug.clone(),
                    satisfied: dependency.satisfied,
                })
        })
        .collect();
    Ok(WorkGraph { nodes, edges })
}

pub fn audit_work_graph(connection: &Connection) -> anyhow::Result<WorkAudit> {
    let graph = build_work_graph(connection)?;
    let active_nodes = graph
        .nodes
        .iter()
        .filter(|node| node.work_item.status != WorkItemStatus::Canceled)
        .collect::<Vec<_>>();
    let mut roots = active_nodes
        .iter()
        .filter(|node| {
            node.dependencies.iter().all(|dependency| {
                graph.nodes.iter().any(|candidate| {
                    candidate.work_item.slug == dependency.slug
                        && candidate.work_item.status == WorkItemStatus::Canceled
                })
            })
        })
        .map(|node| node.work_item.slug.clone())
        .collect::<Vec<_>>();
    let mut terminal_nodes = active_nodes
        .iter()
        .filter(|node| {
            node.dependents.iter().all(|dependent| {
                graph.nodes.iter().any(|candidate| {
                    candidate.work_item.slug == dependent.slug
                        && candidate.work_item.status == WorkItemStatus::Canceled
                })
            })
        })
        .map(|node| node.work_item.slug.clone())
        .collect::<Vec<_>>();
    roots.sort();
    terminal_nodes.sort();

    let mut findings = Vec::new();
    for node in &active_nodes {
        if node.work_item.status == WorkItemStatus::Done
            && node.work_item.acceptance_criteria.is_some()
            && list_validation_records_for_work(connection, &node.work_item.slug, 1)?.is_empty()
        {
            findings.push(WorkAuditFinding {
                code: "completed_without_validation".to_owned(),
                message: format!(
                    "{} is done and has acceptance criteria but no validation record",
                    node.work_item.slug
                ),
                work_items: vec![node.work_item.slug.clone()],
            });
        }
        for dependency in &node.dependencies {
            if dependency.status == WorkItemStatus::Canceled {
                findings.push(WorkAuditFinding {
                    code: "canceled_dependency".to_owned(),
                    message: format!(
                        "{} depends on canceled work item {}",
                        node.work_item.slug, dependency.slug
                    ),
                    work_items: vec![node.work_item.slug.clone(), dependency.slug.clone()],
                });
            }
            if priority_rank(node.work_item.priority.as_deref())
                < priority_rank(
                    graph
                        .nodes
                        .iter()
                        .find(|candidate| candidate.work_item.slug == dependency.slug)
                        .and_then(|candidate| candidate.work_item.priority.as_deref()),
                )
            {
                findings.push(WorkAuditFinding {
                    code: "priority_inversion".to_owned(),
                    message: format!(
                        "higher-priority {} depends on lower-priority {}",
                        node.work_item.slug, dependency.slug
                    ),
                    work_items: vec![node.work_item.slug.clone(), dependency.slug.clone()],
                });
            }
        }
    }

    if roots.len() > 1 {
        findings.push(WorkAuditFinding {
            code: "multiple_roots".to_owned(),
            message: format!("schedule has multiple root work items: {}", roots.join(", ")),
            work_items: roots.clone(),
        });
    }
    if terminal_nodes.len() > 1 {
        findings.push(WorkAuditFinding {
            code: "multiple_terminal_nodes".to_owned(),
            message: format!(
                "schedule has multiple terminal work items: {}",
                terminal_nodes.join(", ")
            ),
            work_items: terminal_nodes.clone(),
        });
    }

    if has_dependency_cycle(&graph) {
        findings.push(WorkAuditFinding {
            code: "dependency_cycle".to_owned(),
            message: "schedule contains a dependency cycle".to_owned(),
            work_items: Vec::new(),
        });
    }

    if let [terminal] = terminal_nodes.as_slice() {
        for node in &active_nodes {
            if node.work_item.slug != *terminal
                && !has_path_to_terminal(&graph, &node.work_item.slug, terminal)
            {
                findings.push(WorkAuditFinding {
                    code: "no_path_to_terminal".to_owned(),
                    message: format!(
                        "{} has no dependency path to terminal work item {}",
                        node.work_item.slug, terminal
                    ),
                    work_items: vec![node.work_item.slug.clone(), terminal.clone()],
                });
            }
        }
    }

    Ok(WorkAudit {
        ok: findings.is_empty(),
        roots,
        terminal_nodes,
        findings,
    })
}

fn priority_rank(priority: Option<&str>) -> u64 {
    priority
        .and_then(|value| value.strip_prefix('P'))
        .and_then(|value| value.parse().ok())
        .unwrap_or(u64::MAX)
}

fn has_dependency_cycle(graph: &WorkGraph) -> bool {
    let mut remaining = graph
        .nodes
        .iter()
        .map(|node| {
            (
                node.work_item.slug.clone(),
                node.dependencies.len(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut ready = remaining
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(slug, _)| slug.clone())
        .collect::<Vec<_>>();
    let mut visited = 0;
    while let Some(slug) = ready.pop() {
        visited += 1;
        for edge in graph.edges.iter().filter(|edge| edge.prerequisite == slug) {
            if let Some(degree) = remaining.get_mut(&edge.child) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push(edge.child.clone());
                }
            }
        }
    }
    visited != remaining.len()
}

fn has_path_to_terminal(graph: &WorkGraph, start: &str, terminal: &str) -> bool {
    let mut pending = vec![start.to_owned()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(slug) = pending.pop() {
        if !seen.insert(slug.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.prerequisite == slug) {
            if edge.child == terminal {
                return true;
            }
            pending.push(edge.child.clone());
        }
    }
    false
}
