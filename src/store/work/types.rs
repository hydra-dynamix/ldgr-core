use super::*;

#[derive(Debug, Clone, Copy)]
pub struct NextWorkSpec<'a> {
    pub slug: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkItemMetadata<'a> {
    pub priority: Option<&'a str>,
    pub program: Option<&'a str>,
    pub group: Option<&'a str>,
    pub acceptance_criteria: Option<&'a str>,
    pub dependencies: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub struct WorkItemPatch<'a> {
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub priority: Option<Option<&'a str>>,
    pub program: Option<Option<&'a str>>,
    pub group: Option<Option<&'a str>>,
    pub acceptance_criteria: Option<Option<&'a str>>,
    pub dependencies: Option<&'a [String]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkReadiness {
    pub ready: bool,
    pub blocked_by: Vec<String>,
    pub dependencies: Vec<String>,
    pub unblocks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleFile {
    pub format: String,
    pub work_items: Vec<ScheduleWorkItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleWorkItem {
    pub slug: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportScheduleResult {
    pub created: usize,
    pub updated: usize,
    pub dependencies: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseRunResult {
    pub run: InvestigationRun,
    pub decision: Decision,
    pub work_item: WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimedRun {
    pub work_item: WorkItem,
    pub run: InvestigationRun,
}
