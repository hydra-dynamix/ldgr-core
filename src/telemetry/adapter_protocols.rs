//! Released numerical protocols owned by published adapters.
//!
//! Core owns queue discovery and transport, so every adapter protocol that may
//! write under `telemetry-pending` must be declared here before release. The
//! collector imports the same constants and therefore validates the identical
//! finite state machines.

use super::transition::{
    NumericalProtocol, StateCode, CANCELLED, COMPLETED_INCONCLUSIVE, COMPLETED_NEGATIVE,
    COMPLETED_POSITIVE, OPERATIONAL_FAILURE, PENDING, RUNNING,
};

macro_rules! independent_step_transitions {
    ($($state:expr),+ $(,)?) => {
        &[
            (PENDING, RUNNING),
            (PENDING, OPERATIONAL_FAILURE),
            (PENDING, CANCELLED),
            (RUNNING, OPERATIONAL_FAILURE),
            (RUNNING, CANCELLED),
            $(
                (RUNNING, $state),
                ($state, COMPLETED_POSITIVE),
                ($state, COMPLETED_NEGATIVE),
                ($state, COMPLETED_INCONCLUSIVE),
                ($state, OPERATIONAL_FAILURE),
                ($state, CANCELLED),
            )+
        ]
    };
}

const CONDUCT_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    21,
    22,
    23,
    24,
    25,
    26,
    27,
    28,
    29,
    30,
    31,
    32,
    33,
    34,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const CONDUCT_TRANSITIONS: &[(StateCode, StateCode)] = independent_step_transitions!(
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32, 33, 34,
);
pub const CONDUCT_ORCHESTRATION_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/conduct-orchestration/v1",
    PENDING,
    CONDUCT_STATES,
    CONDUCT_TRANSITIONS,
    8,
);

const EXAMPLE_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const EXAMPLE_TRANSITIONS: &[(StateCode, StateCode)] = independent_step_transitions!(8, 9, 10, 11);
pub const EXAMPLE_ADAPTER_LIFECYCLE_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/example-adapter-lifecycle/v1",
    PENDING,
    EXAMPLE_STATES,
    EXAMPLE_TRANSITIONS,
    8,
);

const PROGRAMBENCH_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const PROGRAMBENCH_TRANSITIONS: &[(StateCode, StateCode)] = &[
    (PENDING, RUNNING),
    (PENDING, OPERATIONAL_FAILURE),
    (PENDING, CANCELLED),
    (RUNNING, 8),
    (RUNNING, OPERATIONAL_FAILURE),
    (RUNNING, CANCELLED),
    (8, 9),
    (8, OPERATIONAL_FAILURE),
    (8, CANCELLED),
    (9, 10),
    (9, OPERATIONAL_FAILURE),
    (9, CANCELLED),
    (10, 11),
    (10, OPERATIONAL_FAILURE),
    (10, CANCELLED),
    (11, COMPLETED_POSITIVE),
    (11, COMPLETED_NEGATIVE),
    (11, COMPLETED_INCONCLUSIVE),
    (11, OPERATIONAL_FAILURE),
    (11, CANCELLED),
];
pub const PROGRAMBENCH_REPRODUCTION_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/programbench-reproduction/v1",
    PENDING,
    PROGRAMBENCH_STATES,
    PROGRAMBENCH_TRANSITIONS,
    32,
);

const CODE_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
macro_rules! linear_code_transitions {
    ($($from:expr => $to:expr),+ $(,)?) => {
        &[
            (PENDING, RUNNING),
            (PENDING, OPERATIONAL_FAILURE),
            (PENDING, CANCELLED),
            (RUNNING, 8),
            (RUNNING, OPERATIONAL_FAILURE),
            (RUNNING, CANCELLED),
            $(
                ($from, $to),
                ($from, OPERATIONAL_FAILURE),
                ($from, CANCELLED),
            )+
            (20, COMPLETED_POSITIVE),
            (20, COMPLETED_NEGATIVE),
            (20, COMPLETED_INCONCLUSIVE),
            (20, OPERATIONAL_FAILURE),
            (20, CANCELLED),
        ]
    };
}
const CODE_TRANSITIONS: &[(StateCode, StateCode)] = linear_code_transitions!(
    8 => 9,
    9 => 10,
    10 => 11,
    11 => 12,
    12 => 13,
    13 => 14,
    14 => 15,
    15 => 16,
    16 => 17,
    17 => 18,
    18 => 19,
    19 => 20,
);
pub const CODE_WORKFLOW_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/code-workflow/v1",
    PENDING,
    CODE_STATES,
    CODE_TRANSITIONS,
    64,
);

const SECURITY_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const SECURITY_TRANSITIONS: &[(StateCode, StateCode)] = &[
    (PENDING, RUNNING),
    (PENDING, OPERATIONAL_FAILURE),
    (PENDING, CANCELLED),
    (RUNNING, OPERATIONAL_FAILURE),
    (RUNNING, CANCELLED),
    (RUNNING, 8),
    (8, COMPLETED_POSITIVE),
    (8, COMPLETED_NEGATIVE),
    (8, COMPLETED_INCONCLUSIVE),
    (8, OPERATIONAL_FAILURE),
    (8, CANCELLED),
    (RUNNING, 9),
    (9, COMPLETED_POSITIVE),
    (9, COMPLETED_NEGATIVE),
    (9, COMPLETED_INCONCLUSIVE),
    (9, OPERATIONAL_FAILURE),
    (9, CANCELLED),
    (RUNNING, 10),
    (10, COMPLETED_POSITIVE),
    (10, COMPLETED_NEGATIVE),
    (10, COMPLETED_INCONCLUSIVE),
    (10, OPERATIONAL_FAILURE),
    (10, CANCELLED),
    (RUNNING, 11),
    (11, COMPLETED_POSITIVE),
    (11, COMPLETED_NEGATIVE),
    (11, COMPLETED_INCONCLUSIVE),
    (11, OPERATIONAL_FAILURE),
    (11, CANCELLED),
    (RUNNING, 12),
    (12, COMPLETED_POSITIVE),
    (12, COMPLETED_NEGATIVE),
    (12, COMPLETED_INCONCLUSIVE),
    (12, OPERATIONAL_FAILURE),
    (12, CANCELLED),
    (RUNNING, 13),
    (13, COMPLETED_POSITIVE),
    (13, COMPLETED_NEGATIVE),
    (13, COMPLETED_INCONCLUSIVE),
    (13, OPERATIONAL_FAILURE),
    (13, CANCELLED),
    (RUNNING, 14),
    (14, COMPLETED_POSITIVE),
    (14, COMPLETED_NEGATIVE),
    (14, COMPLETED_INCONCLUSIVE),
    (14, OPERATIONAL_FAILURE),
    (14, CANCELLED),
    (RUNNING, 15),
    (15, COMPLETED_POSITIVE),
    (15, COMPLETED_NEGATIVE),
    (15, COMPLETED_INCONCLUSIVE),
    (15, OPERATIONAL_FAILURE),
    (15, CANCELLED),
    (RUNNING, 16),
    (16, COMPLETED_POSITIVE),
    (16, COMPLETED_NEGATIVE),
    (16, COMPLETED_INCONCLUSIVE),
    (16, OPERATIONAL_FAILURE),
    (16, CANCELLED),
    (RUNNING, 17),
    (17, COMPLETED_POSITIVE),
    (17, COMPLETED_NEGATIVE),
    (17, COMPLETED_INCONCLUSIVE),
    (17, OPERATIONAL_FAILURE),
    (17, CANCELLED),
    (RUNNING, 18),
    (18, COMPLETED_POSITIVE),
    (18, COMPLETED_NEGATIVE),
    (18, COMPLETED_INCONCLUSIVE),
    (18, OPERATIONAL_FAILURE),
    (18, CANCELLED),
    (RUNNING, 19),
    (19, COMPLETED_POSITIVE),
    (19, COMPLETED_NEGATIVE),
    (19, COMPLETED_INCONCLUSIVE),
    (19, OPERATIONAL_FAILURE),
    (19, CANCELLED),
    (RUNNING, 20),
    (20, COMPLETED_POSITIVE),
    (20, COMPLETED_NEGATIVE),
    (20, COMPLETED_INCONCLUSIVE),
    (20, OPERATIONAL_FAILURE),
    (20, CANCELLED),
    (10, 11),
    (10, 12),
    (11, 12),
    (12, 13),
    (13, 14),
    (13, 15),
    (14, 15),
    (15, 16),
    (16, 17),
    (17, 18),
];
pub const SECURITY_WORKFLOW_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/security-workflow/v1",
    PENDING,
    SECURITY_STATES,
    SECURITY_TRANSITIONS,
    32,
);

const EXPLORE_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const EXPLORE_TRANSITIONS: &[(StateCode, StateCode)] =
    independent_step_transitions!(8, 9, 10, 11, 12, 13, 14, 15, 16, 17);
pub const EXPLORE_WORKFLOW_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/explore-workflow/v1",
    PENDING,
    EXPLORE_STATES,
    EXPLORE_TRANSITIONS,
    8,
);

const BENCH_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
macro_rules! bench_transitions {
    ($($state:expr => $next:expr),+ $(,)?) => {
        &[
            (PENDING, RUNNING),
            (PENDING, OPERATIONAL_FAILURE),
            (PENDING, CANCELLED),
            (RUNNING, 8),
            (RUNNING, OPERATIONAL_FAILURE),
            (RUNNING, CANCELLED),
            $(
                ($state, $next),
                ($state, COMPLETED_POSITIVE),
                ($state, COMPLETED_NEGATIVE),
                ($state, COMPLETED_INCONCLUSIVE),
                ($state, OPERATIONAL_FAILURE),
                ($state, CANCELLED),
            )+
            (18, COMPLETED_POSITIVE),
            (18, COMPLETED_NEGATIVE),
            (18, COMPLETED_INCONCLUSIVE),
            (18, OPERATIONAL_FAILURE),
            (18, CANCELLED),
        ]
    };
}
const BENCH_TRANSITIONS: &[(StateCode, StateCode)] = bench_transitions!(
    8 => 9,
    9 => 10,
    10 => 11,
    11 => 12,
    12 => 13,
    13 => 14,
    14 => 15,
    15 => 16,
    16 => 17,
    17 => 18,
);
pub const BENCH_WORKFLOW_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/bench-workflow/v1",
    PENDING,
    BENCH_STATES,
    BENCH_TRANSITIONS,
    64,
);

const EVIDENCE_STATES: &[StateCode] = &[
    PENDING,
    RUNNING,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    21,
    22,
    23,
    24,
    25,
    26,
    27,
    28,
    29,
    30,
    31,
    32,
    COMPLETED_POSITIVE,
    COMPLETED_NEGATIVE,
    COMPLETED_INCONCLUSIVE,
    OPERATIONAL_FAILURE,
    CANCELLED,
];
const EVIDENCE_TRANSITIONS: &[(StateCode, StateCode)] = independent_step_transitions!(
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32,
);
pub const EVIDENCE_WORKFLOW_V1: NumericalProtocol = NumericalProtocol::new(
    "/sequences/evidence-workflow/v1",
    PENDING,
    EVIDENCE_STATES,
    EVIDENCE_TRANSITIONS,
    8,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::serializer::parse_exact_sequence;

    const RELEASED_ADAPTER_PROTOCOLS: &[(&NumericalProtocol, &[u8])] = &[
        (&CONDUCT_ORCHESTRATION_V1, b"[0,1,33,3]"),
        (&EXAMPLE_ADAPTER_LIFECYCLE_V1, b"[0,1,9,3]"),
        (&PROGRAMBENCH_REPRODUCTION_V1, b"[0,1,8,9,10,11,3]"),
        (
            &CODE_WORKFLOW_V1,
            b"[0,1,8,9,10,11,12,13,14,15,16,17,18,19,20,3]",
        ),
        (&SECURITY_WORKFLOW_V1, b"[0,1,8,3]"),
        (&EXPLORE_WORKFLOW_V1, b"[0,1,14,3]"),
        (&BENCH_WORKFLOW_V1, b"[0,1,8,9,10,11,12,6]"),
        (&EVIDENCE_WORKFLOW_V1, b"[0,1,9,3]"),
    ];

    #[test]
    fn released_adapter_protocols_validate_real_pending_shapes() -> anyhow::Result<()> {
        for (protocol, payload) in RELEASED_ADAPTER_PROTOCOLS {
            protocol.validate()?;
            parse_exact_sequence(protocol, payload)?;
        }
        Ok(())
    }
}
