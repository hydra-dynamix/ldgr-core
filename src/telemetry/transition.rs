use std::collections::HashSet;

use anyhow::{bail, ensure, Context};

pub type StateCode = u16;

pub const PENDING: StateCode = 0;
pub const RUNNING: StateCode = 1;
pub const HELD: StateCode = 2;
pub const COMPLETED_POSITIVE: StateCode = 3;
pub const COMPLETED_NEGATIVE: StateCode = 4;
pub const COMPLETED_INCONCLUSIVE: StateCode = 5;
pub const OPERATIONAL_FAILURE: StateCode = 6;
pub const CANCELLED: StateCode = 7;

pub const NORMALIZED_TERMINALS: &[StateCode] = &[
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];

const CORE_WORK_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    HELD,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];

const CORE_WORK_TRANSITIONS: &[(StateCode, StateCode)] = &[
    (PENDING, RUNNING),
    (PENDING, OPERATIONAL_FAILURE),
    (PENDING, CANCELLED),
    (RUNNING, HELD),
    (RUNNING, COMPLETED_POSITIVE),
    (RUNNING, COMPLETED_NEGATIVE),
    (RUNNING, COMPLETED_INCONCLUSIVE),
    (RUNNING, OPERATIONAL_FAILURE),
    (RUNNING, CANCELLED),
    (HELD, RUNNING),
    (HELD, OPERATIONAL_FAILURE),
    (HELD, CANCELLED),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NumericalProtocol {
    endpoint: &'static str,
    initial_state: StateCode,
    declared_states: &'static [StateCode],
    allowed_transitions: &'static [(StateCode, StateCode)],
    max_sequence_len: usize,
}

impl NumericalProtocol {
    pub const fn new(
        endpoint: &'static str,
        initial_state: StateCode,
        declared_states: &'static [StateCode],
        allowed_transitions: &'static [(StateCode, StateCode)],
        max_sequence_len: usize,
    ) -> Self {
        Self {
            endpoint,
            initial_state,
            declared_states,
            allowed_transitions,
            max_sequence_len,
        }
    }

    pub fn endpoint(&self) -> &'static str {
        self.endpoint
    }

    pub fn initial_state(&self) -> StateCode {
        self.initial_state
    }

    pub fn declared_states(&self) -> &'static [StateCode] {
        self.declared_states
    }

    pub fn allowed_transitions(&self) -> &'static [(StateCode, StateCode)] {
        self.allowed_transitions
    }

    pub fn max_sequence_len(&self) -> usize {
        self.max_sequence_len
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        validate_endpoint(self.endpoint)?;
        ensure!(
            (2..=256).contains(&self.max_sequence_len),
            "protocol maximum sequence length must be between 2 and 256"
        );
        ensure!(
            self.initial_state == PENDING,
            "protocol initial state must be pending (0)"
        );

        let mut states = HashSet::with_capacity(self.declared_states.len());
        for state in self.declared_states {
            ensure!(states.insert(*state), "duplicate declared state {state}");
        }
        ensure!(
            states.contains(&self.initial_state),
            "initial state {} is not declared",
            self.initial_state
        );
        for terminal in NORMALIZED_TERMINALS {
            ensure!(
                states.contains(terminal),
                "normalized terminal state {terminal} is not declared"
            );
        }

        let mut transitions = HashSet::with_capacity(self.allowed_transitions.len());
        for (from, to) in self.allowed_transitions {
            ensure!(
                states.contains(from),
                "transition source state {from} is not declared"
            );
            ensure!(
                states.contains(to),
                "transition target state {to} is not declared"
            );
            ensure!(
                from != to,
                "self-transition {from} -> {to} is not permitted"
            );
            ensure!(
                !is_terminal(*from),
                "terminal state {from} cannot have an outgoing transition"
            );
            ensure!(
                transitions.insert((*from, *to)),
                "duplicate transition {from} -> {to}"
            );
        }
        Ok(())
    }

    fn declares(&self, state: StateCode) -> bool {
        self.declared_states.contains(&state)
    }

    fn permits(&self, from: StateCode, to: StateCode) -> bool {
        self.allowed_transitions.contains(&(from, to))
    }
}

pub const CORE_WORK_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/core-work/v1",
    PENDING,
    CORE_WORK_STATES,
    CORE_WORK_TRANSITIONS,
    256,
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionAcceptance {
    Intermediate,
    Terminal(NormalizedTerminal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NormalizedTerminal {
    CompletedPositive,
    CompletedNegative,
    CompletedInconclusive,
    OperationalFailure,
    Cancelled,
}

impl TryFrom<StateCode> for NormalizedTerminal {
    type Error = anyhow::Error;

    fn try_from(state: StateCode) -> Result<Self, Self::Error> {
        match state {
            COMPLETED_POSITIVE => Ok(Self::CompletedPositive),
            COMPLETED_NEGATIVE => Ok(Self::CompletedNegative),
            COMPLETED_INCONCLUSIVE => Ok(Self::CompletedInconclusive),
            OPERATIONAL_FAILURE => Ok(Self::OperationalFailure),
            CANCELLED => Ok(Self::Cancelled),
            _ => bail!("state {state} is not a normalized terminal"),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct CommittedSequence<'protocol> {
    protocol: &'protocol NumericalProtocol,
    states: Vec<StateCode>,
}

impl<'protocol> CommittedSequence<'protocol> {
    /// Start a numerical sequence after the initial state has committed locally.
    pub fn begin_after_commit(protocol: &'protocol NumericalProtocol) -> anyhow::Result<Self> {
        protocol.validate().context("invalid numerical protocol")?;
        Ok(Self {
            protocol,
            states: vec![protocol.initial_state],
        })
    }

    /// Submit a state only after the corresponding local ledger commit succeeds.
    pub fn submit_committed(&mut self, state: StateCode) -> anyhow::Result<TransitionAcceptance> {
        ensure!(
            self.states.len() < self.protocol.max_sequence_len,
            "sequence exceeds protocol maximum length {}",
            self.protocol.max_sequence_len
        );
        ensure!(
            self.protocol.declares(state),
            "state {state} is not declared by protocol {}",
            self.protocol.endpoint
        );
        let previous = *self
            .states
            .last()
            .expect("a committed sequence always contains its initial state");
        ensure!(
            self.protocol.permits(previous, state),
            "transition {previous} -> {state} is not declared by protocol {}",
            self.protocol.endpoint
        );
        self.states.push(state);
        match NormalizedTerminal::try_from(state) {
            Ok(terminal) => Ok(TransitionAcceptance::Terminal(terminal)),
            Err(_) => Ok(TransitionAcceptance::Intermediate),
        }
    }

    pub fn protocol_endpoint(&self) -> &'static str {
        self.protocol.endpoint
    }

    pub fn numerical_states(&self) -> &[StateCode] {
        &self.states
    }

    pub fn terminal(&self) -> Option<NormalizedTerminal> {
        self.states
            .last()
            .copied()
            .and_then(|state| NormalizedTerminal::try_from(state).ok())
    }
}

fn is_terminal(state: StateCode) -> bool {
    NORMALIZED_TERMINALS.contains(&state)
}

fn validate_endpoint(endpoint: &str) -> anyhow::Result<()> {
    ensure!(
        endpoint.starts_with("/sequences/") && endpoint.len() > "/sequences/".len(),
        "protocol endpoint must start with /sequences/"
    );
    ensure!(
        endpoint.bytes().all(|byte| byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || b"-_/".contains(&byte)),
        "protocol endpoint contains an invalid character"
    );
    let (name, version) = endpoint["/sequences/".len()..]
        .rsplit_once('/')
        .context("protocol endpoint must end with a version segment")?;
    ensure!(
        !name.is_empty(),
        "protocol endpoint state-machine name is empty"
    );
    ensure!(
        version.strip_prefix('v').is_some_and(
            |value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        ),
        "protocol endpoint must end with a numeric vN version"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_protocol_accepts_positive_and_negative_terminal_sequences() -> anyhow::Result<()> {
        let mut positive = CommittedSequence::begin_after_commit(&CORE_WORK_V1)?;
        assert_eq!(
            positive.submit_committed(RUNNING)?,
            TransitionAcceptance::Intermediate
        );
        assert_eq!(
            positive.submit_committed(COMPLETED_POSITIVE)?,
            TransitionAcceptance::Terminal(NormalizedTerminal::CompletedPositive)
        );
        assert_eq!(positive.numerical_states(), &[0, 1, 3]);

        let mut negative = CommittedSequence::begin_after_commit(&CORE_WORK_V1)?;
        negative.submit_committed(RUNNING)?;
        negative.submit_committed(HELD)?;
        negative.submit_committed(RUNNING)?;
        assert_eq!(
            negative.submit_committed(COMPLETED_NEGATIVE)?,
            TransitionAcceptance::Terminal(NormalizedTerminal::CompletedNegative)
        );
        assert_eq!(negative.numerical_states(), &[0, 1, 2, 1, 4]);
        Ok(())
    }

    #[test]
    fn undeclared_and_invalid_transitions_are_rejected_without_mutation() -> anyhow::Result<()> {
        let mut sequence = CommittedSequence::begin_after_commit(&CORE_WORK_V1)?;
        let undeclared = sequence
            .submit_committed(8)
            .expect_err("undeclared state must fail");
        assert!(undeclared.to_string().contains("state 8 is not declared"));
        let invalid = sequence
            .submit_committed(HELD)
            .expect_err("pending cannot transition to held");
        assert!(invalid.to_string().contains("transition 0 -> 2"));
        assert_eq!(sequence.numerical_states(), &[PENDING]);
        Ok(())
    }

    #[test]
    fn terminal_states_cannot_accept_more_transitions() -> anyhow::Result<()> {
        let mut sequence = CommittedSequence::begin_after_commit(&CORE_WORK_V1)?;
        sequence.submit_committed(RUNNING)?;
        sequence.submit_committed(COMPLETED_NEGATIVE)?;
        let error = sequence
            .submit_committed(RUNNING)
            .expect_err("terminal must be final");
        assert!(error.to_string().contains("transition 4 -> 1"));
        assert_eq!(
            sequence.terminal(),
            Some(NormalizedTerminal::CompletedNegative)
        );
        Ok(())
    }

    #[test]
    fn malformed_adapter_protocol_declarations_fail_closed() {
        const BAD_STATES: &[StateCode] = &[0, 1, 3, 4, 5, 6, 7, 8];
        const BAD_TRANSITIONS: &[(StateCode, StateCode)] = &[(0, 8), (8, 8)];
        let protocol = NumericalProtocol::new(
            "/sequences/adapter-work/v1",
            PENDING,
            BAD_STATES,
            BAD_TRANSITIONS,
            256,
        );
        let error = CommittedSequence::begin_after_commit(&protocol)
            .expect_err("self-transition must fail");
        assert!(error.to_string().contains("invalid numerical protocol"));
        assert!(format!("{error:#}").contains("self-transition 8 -> 8"));
    }
}
