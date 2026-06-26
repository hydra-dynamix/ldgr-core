use super::*;

#[derive(Debug, Clone, Copy)]
pub struct NextWorkSpec<'a> {
    pub slug: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
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

