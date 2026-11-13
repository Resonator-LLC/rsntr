//! Transformation of concurrent wavelet operations and deltas.

use super::delta::{TransformedWaveletDelta, WaveletDelta};
use super::error::{SubmitTransformError, WaveTransformError};
use super::wave_op::{BlipOperation, WaveletOperation, WaveletOperationKind};
use crate::docop;
use crate::version::HashedVersion;

/// A client operation and a server operation, each transformed against the
/// other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationPair<O> {
    pub client_op: O,
    pub server_op: O,
}

/// Transforms a pair of concurrent wavelet operations (`Transform.transform`).
///
/// Operations on different documents, and all pairs not mentioned below,
/// transform to themselves. Content operations on the same document are
/// transformed as document operations. The same participant added by both,
/// or removed by both, turns both operations into no-ops.
///
/// Fails with [`WaveTransformError::RemovedAuthor`] if the server operation
/// removes the creator of the client operation, and with
/// [`WaveTransformError::Participant`] if one side adds the participant the
/// other removes. The first check is skipped when both operations are blip
/// operations, as in the original.
pub fn transform(
    client_op: &WaveletOperation,
    server_op: &WaveletOperation,
) -> Result<OperationPair<WaveletOperation>, WaveTransformError> {
    use WaveletOperationKind::*;
    let identity = || OperationPair {
        client_op: client_op.clone(),
        server_op: server_op.clone(),
    };
    let no_ops = || OperationPair {
        client_op: WaveletOperation::no_op(client_op.context().clone()),
        server_op: WaveletOperation::no_op(server_op.context().clone()),
    };
    let involving = |participant: &crate::id::ParticipantId| {
        Err(WaveTransformError::Participant(
            participant.address().to_owned(),
        ))
    };
    match (client_op.kind(), server_op.kind()) {
        (
            Blip {
                blip_id: client_blip,
                op: client_blip_op,
            },
            Blip {
                blip_id: server_blip,
                op: server_blip_op,
            },
        ) => {
            if client_blip != server_blip {
                // Different blips don't conflict.
                return Ok(identity());
            }
            let pair = transform_blip_ops(client_blip_op, server_blip_op)?;
            Ok(OperationPair {
                client_op: WaveletOperation::blip(
                    client_op.context().clone(),
                    client_blip.clone(),
                    pair.client_op,
                ),
                server_op: WaveletOperation::blip(
                    server_op.context().clone(),
                    server_blip.clone(),
                    pair.server_op,
                ),
            })
        }
        (client, RemoveParticipant { participant }) => {
            if participant == client_op.context().creator() {
                return Err(WaveTransformError::RemovedAuthor(
                    participant.address().to_owned(),
                ));
            }
            match client {
                RemoveParticipant {
                    participant: client_participant,
                } if client_participant == participant => Ok(no_ops()),
                AddParticipant {
                    participant: client_participant,
                    ..
                } if client_participant == participant => involving(participant),
                _ => Ok(identity()),
            }
        }
        (client, AddParticipant { participant, .. }) => match client {
            AddParticipant {
                participant: client_participant,
                ..
            } if client_participant == participant => Ok(no_ops()),
            RemoveParticipant {
                participant: client_participant,
            } if client_participant == participant => involving(participant),
            _ => Ok(identity()),
        },
        _ => Ok(identity()),
    }
}

/// Transforms a pair of concurrent operations on the same document. Two
/// content operations are transformed as document operations, and (as in
/// the original) both results get the default contributor method; any other
/// pair transforms to itself.
pub fn transform_blip_ops(
    client_op: &BlipOperation,
    server_op: &BlipOperation,
) -> Result<OperationPair<BlipOperation>, WaveTransformError> {
    match (client_op, server_op) {
        (BlipOperation::Content { op: client, .. }, BlipOperation::Content { op: server, .. }) => {
            let pair = docop::transform(client, server)?;
            Ok(OperationPair {
                client_op: BlipOperation::content(pair.client_op),
                server_op: BlipOperation::content(pair.server_op),
            })
        }
        _ => Ok(OperationPair {
            client_op: client_op.clone(),
            server_op: server_op.clone(),
        }),
    }
}

/// A client delta and a server delta that apply at the same version
/// (`DeltaPair`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaPair {
    pub client: Vec<WaveletOperation>,
    pub server: Vec<WaveletOperation>,
}

impl DeltaPair {
    pub fn new(client: Vec<WaveletOperation>, server: Vec<WaveletOperation>) -> Self {
        DeltaPair { client, server }
    }

    /// Transforms each delta against the other, so that applying the
    /// client delta and then the transformed server delta gives the same
    /// state as the server delta and then the transformed client delta.
    ///
    /// Identical deltas (same operations by the same creators) nullify each
    /// other: the client delta becomes empty and each server operation
    /// becomes a version update.
    pub fn transform(&self) -> Result<DeltaPair, WaveTransformError> {
        if Self::are_same(&self.client, &self.server) {
            let server = self
                .server
                .iter()
                .map(|op| {
                    op.create_version_update_op(
                        op.context().version_increment(),
                        op.context().hashed_version().cloned(),
                    )
                })
                .collect();
            return Ok(DeltaPair::new(Vec::new(), server));
        }
        let (client, server) = transform_op_lists(&self.client, &self.server)?;
        Ok(DeltaPair::new(client, server))
    }

    /// Whether two deltas have equal operations by the same creators.
    pub fn are_same(a: &[WaveletOperation], b: &[WaveletOperation]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(a, b)| a.context().creator() == b.context().creator() && a == b)
    }
}

/// Transforms two operation lists that apply at the same state against
/// each other, returning the transformed client and server lists. Each
/// server operation is carried through the client operations in turn, so
/// every pairwise transform is between operations on the same state.
pub fn transform_op_lists(
    client_ops: &[WaveletOperation],
    server_ops: &[WaveletOperation],
) -> Result<(Vec<WaveletOperation>, Vec<WaveletOperation>), WaveTransformError> {
    let mut out_client = client_ops.to_vec();
    let mut out_server = Vec::with_capacity(server_ops.len());
    for server_op in server_ops {
        let mut new_server_op = server_op.clone();
        for client_op in &mut out_client {
            let pair = transform(client_op, &new_server_op)?;
            *client_op = pair.client_op;
            new_server_op = pair.server_op;
        }
        out_server.push(new_server_op);
    }
    Ok((out_client, out_server))
}

/// Transforms client operations against server operations the way the
/// server's `WaveletContainerImpl.transformOps` does.
///
/// Quirk kept from the original: each client operation is transformed
/// against the server operations as given, not against the server
/// operations transformed by the preceding client operations. For a client
/// delta of several operations this differs from [`transform_op_lists`]
/// when an earlier client operation affects how the server operations apply
/// to a later one, e.g. two content operations on a document that a server
/// operation also changes: the later one is then transformed against a
/// server operation for a different document state, which fails with a
/// document transform error or gives a wrong result (see the test in this
/// module).
pub fn transform_ops(
    client_ops: &[WaveletOperation],
    server_ops: &[WaveletOperation],
) -> Result<Vec<WaveletOperation>, WaveTransformError> {
    client_ops
        .iter()
        .map(|client_op| {
            let mut c = client_op.clone();
            for s in server_ops {
                c = transform(&c, s)?.client_op;
            }
            Ok(c)
        })
        .collect()
}

/// Transforms a submitted delta to apply at `current_version`
/// (`WaveletContainerImpl.maybeTransformSubmittedDelta`).
///
/// `server_deltas` must be the history from the delta's target version to
/// `current_version`; it is not looked at when the delta already targets
/// the current version.
///
/// If the delta transforms away, or duplicates a server delta by the same
/// author, it is returned with the target version at which that happened
/// instead of the current version, so the caller can treat resubmission as
/// idempotent: a result whose target version is not `current_version` must
/// not be applied.
pub fn transform_submitted_delta(
    delta: &WaveletDelta,
    current_version: &HashedVersion,
    server_deltas: &[TransformedWaveletDelta],
) -> Result<WaveletDelta, SubmitTransformError> {
    let mut target_version = delta.target_version().clone();
    if &target_version == current_version {
        // Submitted against head.
        return Ok(delta.clone());
    }
    if target_version.version() == current_version.version() {
        return Err(SubmitTransformError::InvalidHash {
            expected: current_version.clone(),
            target: target_version,
        });
    }
    let history_error = |target: &HashedVersion| SubmitTransformError::History {
        target: target.clone(),
        current: current_version.clone(),
    };
    if server_deltas.is_empty() {
        return Err(history_error(&target_version));
    }
    let author = delta.author();
    let mut client_ops = delta.ops().to_vec();
    for server_delta in server_deltas {
        let duplicate = author == server_delta.author() && client_ops == server_delta.ops();
        if client_ops.is_empty() || duplicate {
            return Ok(WaveletDelta::new(
                author.clone(),
                target_version,
                client_ops,
            ));
        }
        // Deliberate fix: Java's `transformOps` transforms each client op
        // against the untransformed server ops, which rejects valid
        // multi-op deltas. `transform_ops` keeps the original behaviour.
        client_ops = transform_op_lists(&client_ops, server_delta)?.0;
        target_version = server_delta.resulting_version().clone();
    }
    if &target_version != current_version {
        return Err(history_error(&target_version));
    }
    Ok(WaveletDelta::new(
        author.clone(),
        target_version,
        client_ops,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docop::testing::creator::insert_characters;
    use crate::id::ParticipantId;
    use crate::operation::{NO_TIMESTAMP, WaveletOperationContext};

    fn insert(who: &str, size: usize, location: usize, text: &str) -> WaveletOperation {
        let creator = ParticipantId::of(who).unwrap();
        WaveletOperation::blip_content(
            WaveletOperationContext::new(creator, NO_TIMESTAMP, 1),
            "b+1",
            insert_characters(size, location, text),
        )
    }

    /// Documents the quirk of `transform_ops`: the second client operation
    /// is transformed against a server operation for the wrong state.
    #[test]
    fn server_transform_ops_does_not_carry_server_ops_forward() {
        // Document of 5 items. The client inserts "a" and then "b" at the
        // start; the server concurrently inserts "s" at the end.
        let client = [
            insert("a@example.com", 5, 0, "a"),
            insert("a@example.com", 6, 0, "b"),
        ];
        let server = [insert("b@example.com", 5, 5, "s")];

        let (exact, _) = transform_op_lists(&client, &server).unwrap();
        assert_eq!(
            exact,
            [
                insert("a@example.com", 6, 0, "a"),
                insert("a@example.com", 7, 0, "b"),
            ]
        );
        // The second client op (for 6 items) meets the untransformed server
        // op (for 5 items), which the document transform rejects.
        assert!(matches!(
            transform_ops(&client, &server),
            Err(WaveTransformError::Document(_))
        ));
    }
}
