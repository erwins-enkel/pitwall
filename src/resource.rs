use crate::model::{RunnerResource, SourceKind};

/// One poll result from a resource provider. `source` tags which provider
/// produced it so the app can keep docker and native last-known-good state
/// separate (one source failing never blanks the other).
pub struct ResourceUpdate {
    pub source: SourceKind,
    pub resources: Vec<RunnerResource>,
    /// Running containers whose name matched the prefix this poll (docker only;
    /// native leaves this 0). Non-zero with empty `resources` means matches
    /// exist but their stats weren't ready — distinct from a prefix mismatch.
    pub matched_seen: usize,
    /// Running containers whose name did NOT match the prefix (docker only).
    /// Drives the "N running, none match the prefix" hint when nothing matched.
    pub unmatched_seen: usize,
    pub error: Option<String>,
}
