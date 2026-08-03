use anyhow::{bail, ensure, Context};

use super::serializer::{parse_exact_sequence, serialize_sequence, validate_sequence};
use super::transition::{
    CommittedSequence, NormalizedTerminal, NumericalProtocol, StateCode, TransitionAcceptance,
    CANCELLED, COMPLETED_INCONCLUSIVE, COMPLETED_NEGATIVE, COMPLETED_POSITIVE, OPERATIONAL_FAILURE,
};

const TERMINAL_CODES: &[(NormalizedTerminal, StateCode, &str)] = &[
    (
        NormalizedTerminal::CompletedPositive,
        COMPLETED_POSITIVE,
        "completed-positive",
    ),
    (
        NormalizedTerminal::CompletedNegative,
        COMPLETED_NEGATIVE,
        "completed-negative",
    ),
    (
        NormalizedTerminal::CompletedInconclusive,
        COMPLETED_INCONCLUSIVE,
        "completed-inconclusive",
    ),
    (
        NormalizedTerminal::OperationalFailure,
        OPERATIONAL_FAILURE,
        "operational-failure",
    ),
    (NormalizedTerminal::Cancelled, CANCELLED, "cancelled"),
];

/// One adapter-owned committed state path ending in a normalized terminal.
///
/// Adapter tests should provide at least one path for each normalized terminal.
/// The path must include the protocol initial state as the first element and the
/// terminal code as the final element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPath<'states> {
    terminal: NormalizedTerminal,
    states: &'states [StateCode],
}

impl<'states> TerminalPath<'states> {
    pub const fn new(terminal: NormalizedTerminal, states: &'states [StateCode]) -> Self {
        Self { terminal, states }
    }

    pub fn terminal(self) -> NormalizedTerminal {
        self.terminal
    }

    pub fn states(self) -> &'states [StateCode] {
        self.states
    }
}

/// Canonical Core payload produced while checking one terminal path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalPayload {
    pub terminal: NormalizedTerminal,
    pub states: Vec<StateCode>,
    pub payload: Vec<u8>,
}

/// Summary returned by the adapter telemetry conformance fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterTelemetryConformanceReport {
    pub endpoint: &'static str,
    pub terminal_payloads: Vec<TerminalPayload>,
}

/// Verify that an adapter telemetry protocol stays inside Core's numerical
/// transition boundary.
///
/// This fixture is intended for adapter unit tests. It validates the public
/// numerical protocol declaration, submits every example path through Core's
/// `CommittedSequence::submit_committed`, checks normalized terminal semantics,
/// and proves Core serialization is a bare compact integer array that rejects
/// adapter envelopes, routing fields, and string labels.
///
/// The fixture has no URL, header, request, or transport hook; adapters that
/// pass it are exercising Core-owned transition submission and payload shaping
/// rather than an adapter-owned telemetry network surface.
pub fn verify_adapter_telemetry_conformance(
    protocol: &NumericalProtocol,
    terminal_paths: &[TerminalPath<'_>],
) -> anyhow::Result<AdapterTelemetryConformanceReport> {
    protocol.validate().with_context(|| {
        format!(
            "adapter protocol {} does not declare a valid numerical alphabet",
            protocol.endpoint()
        )
    })?;

    let mut seen_terminals = [false; TERMINAL_CODES.len()];
    let mut terminal_payloads = Vec::with_capacity(terminal_paths.len());

    for (case_index, path) in terminal_paths.iter().copied().enumerate() {
        let terminal_index = terminal_index(path.terminal);
        seen_terminals[terminal_index] = true;

        verify_terminal_path(protocol, path, case_index)?;
        verify_core_transition_submission(protocol, path, case_index)?;
        let payload = verify_minimized_payload(protocol, path, case_index)?;
        terminal_payloads.push(TerminalPayload {
            terminal: path.terminal,
            states: path.states.to_vec(),
            payload,
        });
    }

    let missing = TERMINAL_CODES
        .iter()
        .enumerate()
        .filter_map(|(index, (_, _, name))| (!seen_terminals[index]).then_some(*name))
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty(),
        "terminal examples missing normalized terminal(s): {}",
        missing.join(", ")
    );

    Ok(AdapterTelemetryConformanceReport {
        endpoint: protocol.endpoint(),
        terminal_payloads,
    })
}

fn verify_terminal_path(
    protocol: &NumericalProtocol,
    path: TerminalPath<'_>,
    case_index: usize,
) -> anyhow::Result<()> {
    ensure!(
        path.states.first() == Some(&protocol.initial_state()),
        "terminal path {case_index} for {} must begin with protocol initial state {}",
        terminal_name(path.terminal),
        protocol.initial_state()
    );
    let expected_terminal_code = terminal_code(path.terminal);
    ensure!(
        path.states.last() == Some(&expected_terminal_code),
        "terminal path {case_index} declares {} but ends in state {:?}; expected {}",
        terminal_name(path.terminal),
        path.states.last(),
        expected_terminal_code
    );
    validate_sequence(protocol, path.states).with_context(|| {
        format!(
            "terminal path {case_index} for {} is not a valid numerical sequence",
            terminal_name(path.terminal)
        )
    })?;
    Ok(())
}

fn verify_core_transition_submission(
    protocol: &NumericalProtocol,
    path: TerminalPath<'_>,
    case_index: usize,
) -> anyhow::Result<()> {
    let mut sequence = CommittedSequence::begin_after_commit(protocol).with_context(|| {
        format!(
            "terminal path {case_index} for {} could not start Core transition submission",
            terminal_name(path.terminal)
        )
    })?;

    for (position, state) in path.states.iter().copied().enumerate().skip(1) {
        let acceptance = sequence.submit_committed(state).with_context(|| {
            format!(
                "Core rejected committed transition at terminal path {case_index} position {position}: {} -> {state}",
                path.states[position - 1]
            )
        })?;
        let is_final = position == path.states.len() - 1;
        match (is_final, acceptance) {
            (false, TransitionAcceptance::Intermediate) => {}
            (false, TransitionAcceptance::Terminal(terminal)) => bail!(
                "terminal path {case_index} reached {} before its final state at position {position}",
                terminal_name(terminal)
            ),
            (true, TransitionAcceptance::Terminal(terminal)) => ensure!(
                terminal == path.terminal,
                "terminal path {case_index} returned {}; expected {}",
                terminal_name(terminal),
                terminal_name(path.terminal)
            ),
            (true, TransitionAcceptance::Intermediate) => bail!(
                "terminal path {case_index} final state {} was accepted as intermediate",
                state
            ),
        }
    }

    ensure!(
        sequence.numerical_states() == path.states,
        "Core transition submission mutated terminal path {case_index}: {:?} != {:?}",
        sequence.numerical_states(),
        path.states
    );
    ensure!(
        sequence.terminal() == Some(path.terminal),
        "Core transition submission did not preserve {} semantics for terminal path {case_index}",
        terminal_name(path.terminal)
    );

    let accepted_states = sequence.numerical_states().to_vec();
    let terminal_reopen = sequence.submit_committed(protocol.initial_state());
    ensure!(
        terminal_reopen.is_err(),
        "terminal path {case_index} accepted an additional transition after {}",
        terminal_name(path.terminal)
    );
    ensure!(
        sequence.numerical_states() == accepted_states.as_slice(),
        "failed post-terminal transition mutated terminal path {case_index}"
    );
    Ok(())
}

fn verify_minimized_payload(
    protocol: &NumericalProtocol,
    path: TerminalPath<'_>,
    case_index: usize,
) -> anyhow::Result<Vec<u8>> {
    let payload = serialize_sequence(protocol, path.states).with_context(|| {
        format!(
            "terminal path {case_index} for {} could not be serialized by Core",
            terminal_name(path.terminal)
        )
    })?;
    let parsed: serde_json::Value = serde_json::from_slice(&payload)
        .context("Core serializer emitted payload that is not JSON")?;
    let serde_json::Value::Array(values) = parsed else {
        bail!("Core serializer did not emit a bare integer array")
    };
    ensure!(
        values.len() == path.states.len(),
        "Core payload length for terminal path {case_index} does not match the submitted states"
    );
    ensure!(
        values.iter().all(|value| value.as_u64().is_some()),
        "Core payload for terminal path {case_index} contains a non-numeric state"
    );
    ensure!(
        parse_exact_sequence(protocol, &payload)? == path.states,
        "Core payload for terminal path {case_index} does not parse back to the submitted states"
    );

    reject_payload_extension(
        protocol,
        serde_json::json!({"sequence": path.states}),
        "sequence envelope",
        case_index,
    )?;
    reject_payload_extension(
        protocol,
        serde_json::json!({"endpoint": protocol.endpoint(), "sequence": path.states}),
        "routing envelope",
        case_index,
    )?;
    let mut labelled = path
        .states
        .iter()
        .copied()
        .map(serde_json::Value::from)
        .collect::<Vec<_>>();
    if let Some(last) = labelled.last_mut() {
        *last = serde_json::Value::from(terminal_name(path.terminal));
    }
    reject_payload_extension(protocol, labelled.into(), "terminal label", case_index)?;

    Ok(payload)
}

fn reject_payload_extension(
    protocol: &NumericalProtocol,
    value: serde_json::Value,
    label: &str,
    case_index: usize,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&value)
        .with_context(|| format!("failed to serialize {label} fixture"))?;
    ensure!(
        parse_exact_sequence(protocol, &bytes).is_err(),
        "adapter {label} payload was accepted for terminal path {case_index}"
    );
    Ok(())
}

fn terminal_code(terminal: NormalizedTerminal) -> StateCode {
    TERMINAL_CODES
        .iter()
        .find_map(|(candidate, code, _)| (*candidate == terminal).then_some(*code))
        .expect("all normalized terminals are covered")
}

fn terminal_index(terminal: NormalizedTerminal) -> usize {
    TERMINAL_CODES
        .iter()
        .position(|(candidate, _, _)| *candidate == terminal)
        .expect("all normalized terminals are covered")
}

fn terminal_name(terminal: NormalizedTerminal) -> &'static str {
    TERMINAL_CODES
        .iter()
        .find_map(|(candidate, _, name)| (*candidate == terminal).then_some(*name))
        .expect("all normalized terminals are covered")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::transition::{PENDING, RUNNING};

    const ADAPTER_STATES: &[StateCode] = &[
        PENDING,
        RUNNING,
        8,
        COMPLETED_POSITIVE,
        COMPLETED_NEGATIVE,
        COMPLETED_INCONCLUSIVE,
        OPERATIONAL_FAILURE,
        CANCELLED,
    ];
    const ADAPTER_TRANSITIONS: &[(StateCode, StateCode)] = &[
        (PENDING, RUNNING),
        (RUNNING, 8),
        (8, COMPLETED_POSITIVE),
        (8, COMPLETED_NEGATIVE),
        (8, COMPLETED_INCONCLUSIVE),
        (8, OPERATIONAL_FAILURE),
        (8, CANCELLED),
    ];
    const ADAPTER_V1: NumericalProtocol = NumericalProtocol::new(
        "/sequences/example-adapter-work/v1",
        PENDING,
        ADAPTER_STATES,
        ADAPTER_TRANSITIONS,
        32,
    );

    const COMPLETE_TERMINAL_PATHS: &[TerminalPath<'static>] = &[
        TerminalPath::new(
            NormalizedTerminal::CompletedPositive,
            &[PENDING, RUNNING, 8, COMPLETED_POSITIVE],
        ),
        TerminalPath::new(
            NormalizedTerminal::CompletedNegative,
            &[PENDING, RUNNING, 8, COMPLETED_NEGATIVE],
        ),
        TerminalPath::new(
            NormalizedTerminal::CompletedInconclusive,
            &[PENDING, RUNNING, 8, COMPLETED_INCONCLUSIVE],
        ),
        TerminalPath::new(
            NormalizedTerminal::OperationalFailure,
            &[PENDING, RUNNING, 8, OPERATIONAL_FAILURE],
        ),
        TerminalPath::new(
            NormalizedTerminal::Cancelled,
            &[PENDING, RUNNING, 8, CANCELLED],
        ),
    ];

    #[test]
    fn valid_adapter_protocol_passes_with_bare_array_payloads() -> anyhow::Result<()> {
        let report = verify_adapter_telemetry_conformance(&ADAPTER_V1, COMPLETE_TERMINAL_PATHS)?;
        assert_eq!(report.endpoint, "/sequences/example-adapter-work/v1");
        assert_eq!(report.terminal_payloads.len(), 5);
        assert!(report.terminal_payloads.iter().any(|payload| {
            payload.terminal == NormalizedTerminal::CompletedNegative
                && payload.states == vec![PENDING, RUNNING, 8, COMPLETED_NEGATIVE]
                && payload.payload == b"[0,1,8,4]"
        }));
        Ok(())
    }

    #[test]
    fn missing_terminal_examples_fail_conformance() {
        let error = verify_adapter_telemetry_conformance(
            &ADAPTER_V1,
            &COMPLETE_TERMINAL_PATHS[..COMPLETE_TERMINAL_PATHS.len() - 1],
        )
        .expect_err("cancelled coverage should be required");
        assert!(error.to_string().contains("missing normalized terminal"));
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn terminal_semantic_mismatches_fail_conformance() {
        let mismatched = [
            TerminalPath::new(
                NormalizedTerminal::CompletedPositive,
                &[PENDING, RUNNING, 8, COMPLETED_POSITIVE],
            ),
            TerminalPath::new(
                NormalizedTerminal::CompletedNegative,
                &[PENDING, RUNNING, 8, OPERATIONAL_FAILURE],
            ),
            TerminalPath::new(
                NormalizedTerminal::CompletedInconclusive,
                &[PENDING, RUNNING, 8, COMPLETED_INCONCLUSIVE],
            ),
            TerminalPath::new(
                NormalizedTerminal::OperationalFailure,
                &[PENDING, RUNNING, 8, OPERATIONAL_FAILURE],
            ),
            TerminalPath::new(
                NormalizedTerminal::Cancelled,
                &[PENDING, RUNNING, 8, CANCELLED],
            ),
        ];
        let error = verify_adapter_telemetry_conformance(&ADAPTER_V1, &mismatched)
            .expect_err("completed-negative must not be operational-failure");
        assert!(error.to_string().contains("declares completed-negative"));
        assert!(error.to_string().contains("expected 4"));
    }

    #[test]
    fn undeclared_transition_paths_fail_through_core_submission() {
        let invalid = [
            TerminalPath::new(
                NormalizedTerminal::CompletedPositive,
                &[PENDING, COMPLETED_POSITIVE],
            ),
            TerminalPath::new(
                NormalizedTerminal::CompletedNegative,
                &[PENDING, RUNNING, 8, COMPLETED_NEGATIVE],
            ),
            TerminalPath::new(
                NormalizedTerminal::CompletedInconclusive,
                &[PENDING, RUNNING, 8, COMPLETED_INCONCLUSIVE],
            ),
            TerminalPath::new(
                NormalizedTerminal::OperationalFailure,
                &[PENDING, RUNNING, 8, OPERATIONAL_FAILURE],
            ),
            TerminalPath::new(
                NormalizedTerminal::Cancelled,
                &[PENDING, RUNNING, 8, CANCELLED],
            ),
        ];
        let error = verify_adapter_telemetry_conformance(&ADAPTER_V1, &invalid)
            .expect_err("invalid path should fail Core sequence validation");
        assert!(format!("{error:#}").contains("transition 0 -> 3"));
    }

    #[test]
    fn malformed_protocol_declarations_fail_conformance() {
        const BAD_STATES: &[StateCode] = &[
            PENDING,
            RUNNING,
            COMPLETED_POSITIVE,
            COMPLETED_NEGATIVE,
            COMPLETED_INCONCLUSIVE,
            OPERATIONAL_FAILURE,
        ];
        const BAD_PROTOCOL: NumericalProtocol = NumericalProtocol::new(
            "/sequences/bad-adapter/v1",
            PENDING,
            BAD_STATES,
            &[(PENDING, RUNNING), (RUNNING, COMPLETED_POSITIVE)],
            32,
        );
        let error = verify_adapter_telemetry_conformance(&BAD_PROTOCOL, COMPLETE_TERMINAL_PATHS)
            .expect_err("missing cancelled state must fail the alphabet declaration");
        assert!(error
            .to_string()
            .contains("does not declare a valid numerical alphabet"));
        assert!(format!("{error:#}").contains("normalized terminal state 7 is not declared"));
    }
}
