//! Evaluations of whether a wavelet is wanted (`WantedEvaluation`).

use std::cmp::Ordering;

use crate::id::WaveletId;

/// One judgement, by an agent, of whether the user wants a wavelet
/// (`WantedEvaluation`, `SimpleWantedEvaluation`).
#[derive(Debug, Clone, PartialEq)]
pub struct WantedEvaluation {
    /// The wavelet judged; `None` when read from an element without a
    /// wavelet id.
    pub wavelet_id: Option<WaveletId>,
    /// Address of the participant who added the user to the wavelet.
    pub adder_address: String,
    pub wanted: bool,
    /// How sure the agent is, from 0 to 1.
    pub certainty: f64,
    /// When the judgement was made, in milliseconds since the epoch.
    pub timestamp: i64,
    pub agent_identity: String,
    pub ignored: bool,
    pub comment: String,
}

/// `String.hashCode`.
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, unit| {
        h.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

impl WantedEvaluation {
    /// The original's `hashCode`
    /// (`WantedEvaluation.ImplementationHelper.calculateHashCode`), which
    /// its ordering falls back on.
    pub fn java_hash_code(&self) -> i32 {
        let wavelet_hash = match &self.wavelet_id {
            None => 1,
            Some(id) => 31i32
                .wrapping_mul(31i32.wrapping_add(java_string_hash(id.domain())))
                .wrapping_add(java_string_hash(id.id())),
        };
        let certainty_bits = if self.certainty.is_nan() {
            0x7ff8_0000_0000_0000u64
        } else {
            self.certainty.to_bits()
        };
        let certainty_hash = (certainty_bits ^ (certainty_bits >> 32)) as i32;
        let mut result = 17i32;
        result = result.wrapping_mul(37i32.wrapping_add(wavelet_hash));
        result = result.wrapping_mul(37i32.wrapping_add(java_string_hash(&self.adder_address)));
        result = result.wrapping_mul(37 + i32::from(self.wanted));
        result = result.wrapping_mul(37i32.wrapping_add(certainty_hash));
        result = result.wrapping_mul(37i32.wrapping_add(java_string_hash(&self.comment)));
        // An int multiplied by a long, narrowed back to int.
        result = i64::from(result).wrapping_mul(37i64.wrapping_add(self.timestamp)) as i32;
        result = result.wrapping_mul(37 + i32::from(self.ignored));
        result.wrapping_mul(37i32.wrapping_add(java_string_hash(&self.agent_identity)))
    }

    /// `WantedEvaluation.COMPARATOR`: by certainty, then by timestamp, then
    /// by the difference of the hash codes. Like the original this is not a
    /// consistent order when that difference overflows.
    pub fn compare(&self, other: &WantedEvaluation) -> Ordering {
        let by_certainty = (self.certainty - other.certainty)
            .partial_cmp(&0.0)
            .unwrap_or(Ordering::Equal);
        by_certainty
            .then_with(|| self.timestamp.wrapping_sub(other.timestamp).cmp(&0))
            .then_with(|| {
                self.java_hash_code()
                    .wrapping_sub(other.java_hash_code())
                    .cmp(&0)
            })
    }
}

/// The evaluations of one wavelet (`WantedEvaluationSet`,
/// `SimpleWantedEvaluationSet`).
///
/// The original keeps the evaluations in a hash set, whose iteration order
/// decides between evaluations that tie; here they are kept in the order
/// given, without duplicates, and the first of a tie wins.
#[derive(Debug, Clone, PartialEq)]
pub struct WantedEvaluationSet {
    wavelet_id: WaveletId,
    evaluations: Vec<WantedEvaluation>,
}

impl WantedEvaluationSet {
    pub fn new(
        wavelet_id: WaveletId,
        evaluations: impl IntoIterator<Item = WantedEvaluation>,
    ) -> Self {
        let mut unique: Vec<WantedEvaluation> = Vec::new();
        for evaluation in evaluations {
            if !unique.contains(&evaluation) {
                unique.push(evaluation);
            }
        }
        WantedEvaluationSet {
            wavelet_id,
            evaluations: unique,
        }
    }

    fn default_evaluation(&self) -> WantedEvaluation {
        WantedEvaluation {
            wavelet_id: Some(self.wavelet_id.clone()),
            adder_address: "default address".to_owned(),
            wanted: true,
            certainty: 0.0,
            timestamp: 0,
            agent_identity: "default".to_owned(),
            ignored: false,
            comment: "default rule".to_owned(),
        }
    }

    pub fn wavelet_id(&self) -> &WaveletId {
        &self.wavelet_id
    }

    pub fn evaluations(&self) -> &[WantedEvaluation] {
        &self.evaluations
    }

    /// The greatest evaluation by [`WantedEvaluation::compare`], or a
    /// default one (wanted, certainty zero) if there is none
    /// (`getMostCertain`).
    pub fn most_certain(&self) -> WantedEvaluation {
        let mut result: Option<&WantedEvaluation> = None;
        for evaluation in &self.evaluations {
            if result.is_none_or(|best| best.compare(evaluation) == Ordering::Less) {
                result = Some(evaluation);
            }
        }
        result.cloned().unwrap_or_else(|| self.default_evaluation())
    }

    /// The evaluation with the latest timestamp, or a default one if there
    /// is none (`getMostRecent`).
    pub fn most_recent(&self) -> WantedEvaluation {
        let mut result: Option<&WantedEvaluation> = None;
        for evaluation in &self.evaluations {
            if result.is_none_or(|best| evaluation.timestamp > best.timestamp) {
                result = Some(evaluation);
            }
        }
        result.cloned().unwrap_or_else(|| self.default_evaluation())
    }

    /// Whether the most recent evaluation says to ignore the wavelet.
    pub fn is_ignored(&self) -> bool {
        self.most_recent().ignored
    }

    /// Whether the most certain evaluation says the wavelet is wanted.
    pub fn is_wanted(&self) -> bool {
        self.most_certain().wanted
    }
}
