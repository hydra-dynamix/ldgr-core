use anyhow::{ensure, Context};

use super::transition::{NormalizedTerminal, NumericalProtocol, StateCode};

pub fn validate_sequence(protocol: &NumericalProtocol, states: &[StateCode]) -> anyhow::Result<()> {
    protocol.validate().context("invalid numerical protocol")?;
    ensure!(states.len() >= 2, "sequence must contain at least 2 states");
    ensure!(
        states.len() <= protocol.max_sequence_len(),
        "sequence length {} exceeds protocol maximum {}",
        states.len(),
        protocol.max_sequence_len()
    );
    ensure!(
        states.first() == Some(&protocol.initial_state()),
        "sequence must begin with state {}",
        protocol.initial_state()
    );
    for state in states {
        ensure!(
            protocol.declares(*state),
            "state {state} is not declared by protocol {}",
            protocol.endpoint()
        );
    }

    let terminal_positions = states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| NormalizedTerminal::try_from(*state).ok().map(|_| index))
        .collect::<Vec<_>>();
    ensure!(
        terminal_positions.len() == 1,
        "sequence must contain exactly one terminal state; found {}",
        terminal_positions.len()
    );
    ensure!(
        terminal_positions[0] == states.len() - 1,
        "terminal state must be the final state"
    );

    for transition in states.windows(2) {
        let from = transition[0];
        let to = transition[1];
        ensure!(
            from != to,
            "adjacent self-transition {from} -> {to} is invalid"
        );
        ensure!(
            protocol.permits(from, to),
            "transition {from} -> {to} is not declared by protocol {}",
            protocol.endpoint()
        );
    }
    Ok(())
}

pub fn serialize_sequence(
    protocol: &NumericalProtocol,
    states: &[StateCode],
) -> anyhow::Result<Vec<u8>> {
    validate_sequence(protocol, states)?;
    serde_json::to_vec(states).context("failed to serialize numerical sequence")
}

pub fn parse_exact_sequence(
    protocol: &NumericalProtocol,
    payload: &[u8],
) -> anyhow::Result<Vec<StateCode>> {
    let states: Vec<StateCode> =
        serde_json::from_slice(payload).context("payload is not a JSON integer array")?;
    let canonical = serialize_sequence(protocol, &states)?;
    ensure!(
        canonical == payload,
        "payload is not the canonical compact numerical array"
    );
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::transition::CORE_WORK_V1;

    #[test]
    fn golden_payloads_are_exact_bare_compact_arrays() -> anyhow::Result<()> {
        let cases: &[(&[StateCode], &[u8])] = &[
            (&[0, 1, 3], b"[0,1,3]"),
            (&[0, 1, 2, 1, 4], b"[0,1,2,1,4]"),
            (&[0, 1, 5], b"[0,1,5]"),
            (&[0, 6], b"[0,6]"),
            (&[0, 1, 7], b"[0,1,7]"),
        ];
        for (states, expected) in cases {
            let payload = serialize_sequence(&CORE_WORK_V1, states)?;
            assert_eq!(&payload, expected);
            assert_eq!(parse_exact_sequence(&CORE_WORK_V1, &payload)?, *states);
        }
        Ok(())
    }

    #[test]
    fn malformed_sequences_fail_every_protocol_invariant() {
        let cases: &[(&[StateCode], &str)] = &[
            (&[0], "at least 2 states"),
            (&[0, 1], "exactly one terminal"),
            (&[0, 1, 4, 3], "exactly one terminal"),
            (&[0, 1, 4, 1], "terminal state must be the final state"),
            (&[0, 2, 1, 3], "transition 0 -> 2"),
            (&[0, 1, 1, 3], "self-transition 1 -> 1"),
            (&[0, 1, 8], "state 8 is not declared"),
            (&[1, 3], "must begin with state 0"),
        ];
        for (states, expected) in cases {
            let error = serialize_sequence(&CORE_WORK_V1, states)
                .expect_err("invalid sequence should fail");
            assert!(error.to_string().contains(expected), "{error:#}");
        }

        let over_bound = std::iter::once(0)
            .chain(std::iter::repeat_n(1, 255))
            .chain(std::iter::once(3))
            .collect::<Vec<_>>();
        let error = serialize_sequence(&CORE_WORK_V1, &over_bound)
            .expect_err("over-bound sequence should fail");
        assert!(error.to_string().contains("exceeds protocol maximum"));
    }

    #[test]
    fn parser_rejects_envelopes_types_and_noncanonical_json() {
        let cases: &[&[u8]] = &[
            br#"{"sequence":[0,1,3]}"#,
            br#"[0,1,"negative"]"#,
            b"[0,-1,3]",
            b"[0,1.0,3]",
            b"[0,65536,3]",
            b"[0, 1, 3]",
            b"[0,1,3]\n",
            b"[0,1,3][0,1,4]",
        ];
        for payload in cases {
            assert!(parse_exact_sequence(&CORE_WORK_V1, payload).is_err());
        }
    }
}
