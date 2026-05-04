//! Conservative synchronous policy evaluation for Secretary actions.

use crate::domain::{
    Actor, LedgerTimestamp, PolicyDecision, PolicyDecisionId, PolicyOutcome, RepositoryId,
    RepositoryTrustState, ResourceScope, RiskTier,
};

const POLICY_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_POLICY_VERSION: &str = "policy-stub-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInput {
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub requested_capability: String,
    pub resource_scope: ResourceScope,
    pub tool_id: Option<String>,
    pub provider_id: Option<String>,
    pub declared_effect: String,
    pub current_trust_state: RepositoryTrustState,
    pub approval_available: bool,
    pub policy_version: String,
    pub registered_scope: bool,
    pub broad_or_unbounded: bool,
}

impl PolicyInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        requester: Actor,
        repository_id: RepositoryId,
        requested_capability: impl Into<String>,
        resource_scope: ResourceScope,
        declared_effect: impl Into<String>,
        current_trust_state: RepositoryTrustState,
        approval_available: bool,
        policy_version: impl Into<String>,
    ) -> Self {
        Self {
            requester,
            repository_id,
            requested_capability: requested_capability.into(),
            resource_scope,
            tool_id: None,
            provider_id: None,
            declared_effect: declared_effect.into(),
            current_trust_state,
            approval_available,
            policy_version: policy_version.into(),
            registered_scope: true,
            broad_or_unbounded: false,
        }
    }

    pub fn with_tool_id(mut self, tool_id: impl Into<String>) -> Self {
        self.tool_id = Some(tool_id.into());
        self
    }

    pub fn with_provider_id(mut self, provider_id: impl Into<String>) -> Self {
        self.provider_id = Some(provider_id.into());
        self
    }

    pub fn outside_registered_scope(mut self) -> Self {
        self.registered_scope = false;
        self
    }

    pub fn broad_or_unbounded(mut self) -> Self {
        self.broad_or_unbounded = true;
        self
    }
}

impl Default for PolicyInput {
    fn default() -> Self {
        Self {
            requester: Actor::System {
                id: "policy-stub".to_string(),
            },
            repository_id: RepositoryId::new(),
            requested_capability: "filesystem.read".to_string(),
            resource_scope: ResourceScope {
                kind: "path".to_string(),
                value: ".".to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "read workspace data".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: true,
            policy_version: DEFAULT_POLICY_VERSION.to_string(),
            registered_scope: true,
            broad_or_unbounded: false,
        }
    }
}

pub trait PolicyEngine {
    fn evaluate(&self, input: PolicyInput) -> PolicyDecision;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultPolicyEngine;

impl DefaultPolicyEngine {
    pub fn new() -> Self {
        Self
    }

    fn evaluate_rule(&self, input: &PolicyInput) -> RuleDecision {
        let capability = Capability::from_name(&input.requested_capability);

        if input.current_trust_state == RepositoryTrustState::Blocked {
            return RuleDecision::blocked(
                RiskTier::R4,
                "repository_blocked",
                "The repository is blocked by policy, so the requested action cannot run.",
            );
        }

        if !input.registered_scope {
            return RuleDecision::blocked(
                RiskTier::R4,
                "outside_registered_scope",
                "The requested resource is outside the registered repository scope.",
            );
        }

        if capability.is_repository_mutating()
            && input.current_trust_state == RepositoryTrustState::ReadOnly
        {
            return RuleDecision::blocked(
                RiskTier::R4,
                "repository_read_only",
                "The repository is read-only, so repository-mutating actions cannot run.",
            );
        }

        match capability {
            Capability::Informational => RuleDecision::allowed(
                RiskTier::R0,
                "informational_allowed",
                "Informational status or capability discovery is allowed.",
            ),
            Capability::FilesystemRead => {
                if input.current_trust_state == RepositoryTrustState::ReadOnly {
                    RuleDecision::audited(
                        RiskTier::R1,
                        "bounded_read_audited_read_only",
                        "Bounded filesystem read is allowed in a read-only repository with audit evidence.",
                    )
                } else {
                    RuleDecision::allowed(
                        RiskTier::R1,
                        "bounded_read_allowed",
                        "Bounded filesystem read is allowed inside the registered repository scope.",
                    )
                }
            }
            Capability::FilesystemWrite => {
                if input.broad_or_unbounded {
                    RuleDecision::needs_approval(
                        RiskTier::R3,
                        "filesystem_write_broad_or_unbounded_needs_approval",
                        "Broad or unbounded filesystem write needs approval before it can run.",
                    )
                } else {
                    RuleDecision::audited(
                        RiskTier::R2,
                        "bounded_write_audited",
                        "Filesystem modifications inside the registered repository scope require audit evidence.",
                    )
                }
            }
            Capability::ProcessExec => {
                if input.broad_or_unbounded {
                    RuleDecision::needs_approval(
                        RiskTier::R3,
                        "process_broad_or_unbounded_needs_approval",
                        "Process execution is broad or unbounded and needs approval before it can run.",
                    )
                } else {
                    RuleDecision::audited(
                        RiskTier::R2,
                        "bounded_process_audited",
                        "Process execution with explicit argv, cwd, timeout, and env allowlist requires audit evidence.",
                    )
                }
            }
            Capability::BroadRepositoryMutation => RuleDecision::needs_approval(
                RiskTier::R3,
                "broad_repository_mutation_needs_approval",
                "Broad repository mutation needs approval before it can run.",
            ),
            Capability::DestructiveRepositoryAction => RuleDecision::blocked(
                RiskTier::R4,
                "destructive_repository_action_blocked",
                "Destructive repository actions are blocked until explicit policy support exists.",
            ),
            Capability::ExternalNetworkOrService => RuleDecision::needs_approval(
                RiskTier::R3,
                "external_network_or_service_needs_approval",
                "External network or service access needs approval before it can run.",
            ),
            Capability::SecretAccess => RuleDecision::needs_approval(
                RiskTier::R3,
                "secret_access_needs_approval",
                "Secret access needs approval before it can run.",
            ),
            Capability::Unsupported => RuleDecision::blocked(
                RiskTier::R4,
                "unsupported_capability_blocked",
                "The requested capability is not supported by the policy stub.",
            ),
        }
    }
}

impl PolicyEngine for DefaultPolicyEngine {
    fn evaluate(&self, input: PolicyInput) -> PolicyDecision {
        let rule = self.evaluate_rule(&input);

        PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: POLICY_SCHEMA_VERSION,
            created_at: LedgerTimestamp::now(),
            requester: input.requester,
            repository_id: input.repository_id,
            requested_capability: input.requested_capability,
            resource_scope: input.resource_scope,
            tool_id: input.tool_id,
            provider_id: input.provider_id,
            declared_effect: input.declared_effect,
            current_trust_state: input.current_trust_state,
            approval_available: input.approval_available,
            policy_version: input.policy_version,
            outcome: rule.outcome,
            risk_tier: rule.risk_tier,
            reason_code: rule.reason_code.to_string(),
            user_reason: rule.user_reason.to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleDecision {
    outcome: PolicyOutcome,
    risk_tier: RiskTier,
    reason_code: &'static str,
    user_reason: &'static str,
}

impl RuleDecision {
    const fn allowed(
        risk_tier: RiskTier,
        reason_code: &'static str,
        user_reason: &'static str,
    ) -> Self {
        Self {
            outcome: PolicyOutcome::Allowed,
            risk_tier,
            reason_code,
            user_reason,
        }
    }

    const fn audited(
        risk_tier: RiskTier,
        reason_code: &'static str,
        user_reason: &'static str,
    ) -> Self {
        Self {
            outcome: PolicyOutcome::Audited,
            risk_tier,
            reason_code,
            user_reason,
        }
    }

    const fn needs_approval(
        risk_tier: RiskTier,
        reason_code: &'static str,
        user_reason: &'static str,
    ) -> Self {
        Self {
            outcome: PolicyOutcome::NeedsApproval,
            risk_tier,
            reason_code,
            user_reason,
        }
    }

    const fn blocked(
        risk_tier: RiskTier,
        reason_code: &'static str,
        user_reason: &'static str,
    ) -> Self {
        Self {
            outcome: PolicyOutcome::Blocked,
            risk_tier,
            reason_code,
            user_reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    Informational,
    FilesystemRead,
    FilesystemWrite,
    ProcessExec,
    BroadRepositoryMutation,
    DestructiveRepositoryAction,
    ExternalNetworkOrService,
    SecretAccess,
    Unsupported,
}

impl Capability {
    fn from_name(name: &str) -> Self {
        let normalized = name
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-', ':', '/'], ".");

        match normalized.as_str() {
            "informational"
            | "status"
            | "health"
            | "capability.discovery"
            | "capabilities.list"
            | "policy.preview"
            | "policy.check" => Self::Informational,
            "filesystem.read" | "filesystem.list" | "filesystem.search" | "filesystem.stat"
            | "filesystem.diff" | "fs.read" | "fs.list" | "fs.search" | "fs.stat" | "fs.diff" => {
                Self::FilesystemRead
            }
            "filesystem.write" | "filesystem.patch" | "filesystem.delete" | "filesystem.move"
            | "fs.write" | "fs.patch" | "fs.delete" | "fs.move" => Self::FilesystemWrite,
            "process.exec" | "process.execute" | "process.run" | "proc.exec" | "proc.run" => {
                Self::ProcessExec
            }
            "repository.mutate.broad"
            | "repo.mutate.broad"
            | "repository.broad.mutation"
            | "repo.broad.mutation" => Self::BroadRepositoryMutation,
            "repository.destructive"
            | "repo.destructive"
            | "repository.delete"
            | "repo.delete"
            | "repository.reset.hard"
            | "repo.reset.hard" => Self::DestructiveRepositoryAction,
            "network.external" | "external.network" | "service.external" | "external.service"
            | "network.call" | "service.call" => Self::ExternalNetworkOrService,
            "secret.access" | "secrets.access" | "secret.read" | "secrets.read" => {
                Self::SecretAccess
            }
            _ => Self::Unsupported,
        }
    }

    const fn is_repository_mutating(self) -> bool {
        matches!(
            self,
            Self::FilesystemWrite
                | Self::ProcessExec
                | Self::BroadRepositoryMutation
                | Self::DestructiveRepositoryAction
        )
    }
}

/// Canonicalize a job-submission capability hint into the daemon's supported
/// job capability when the hint is informational.
pub fn canonicalize_job_requested_capability(name: &str) -> Option<&'static str> {
    match Capability::from_name(name) {
        Capability::Informational => Some("capability.discovery"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(capability: &str) -> PolicyInput {
        PolicyInput {
            requester: Actor::Agent {
                id: "agent:test".to_string(),
                display_name: Some("Test Agent".to_string()),
            },
            repository_id: RepositoryId::new(),
            requested_capability: capability.to_string(),
            resource_scope: ResourceScope {
                kind: "path".to_string(),
                value: "src/lib.rs".to_string(),
            },
            tool_id: Some("tool:test".to_string()),
            provider_id: None,
            declared_effect: "test effect".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: true,
            policy_version: DEFAULT_POLICY_VERSION.to_string(),
            registered_scope: true,
            broad_or_unbounded: false,
        }
    }

    fn decide(input: PolicyInput) -> PolicyDecision {
        DefaultPolicyEngine::new().evaluate(input)
    }

    #[test]
    fn r1_bounded_filesystem_read_is_allowed() {
        for capability in ["filesystem.read", "filesystem.diff", "fs.diff"] {
            let decision = decide(input(capability));

            assert_eq!(PolicyOutcome::Allowed, decision.outcome, "{capability}");
            assert_eq!(RiskTier::R1, decision.risk_tier, "{capability}");
            assert_eq!("bounded_read_allowed", decision.reason_code, "{capability}");
        }
    }

    #[test]
    fn r0_informational_status_is_allowed() {
        let decision = decide(input("capability.discovery"));

        assert_eq!(PolicyOutcome::Allowed, decision.outcome);
        assert_eq!(RiskTier::R0, decision.risk_tier);
        assert_eq!("informational_allowed", decision.reason_code);
    }

    #[test]
    fn r1_read_only_filesystem_read_is_audited() {
        let mut input = input("filesystem.search");
        input.current_trust_state = RepositoryTrustState::ReadOnly;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::Audited, decision.outcome);
        assert_eq!(RiskTier::R1, decision.risk_tier);
        assert_eq!("bounded_read_audited_read_only", decision.reason_code);
    }

    #[test]
    fn r2_filesystem_write_is_audited() {
        for capability in [
            "filesystem.write",
            "filesystem.patch",
            "filesystem.delete",
            "filesystem.move",
            "fs.write",
            "fs.delete",
            "fs.move",
        ] {
            let decision = decide(input(capability));

            assert_eq!(PolicyOutcome::Audited, decision.outcome, "{capability}");
            assert_eq!(RiskTier::R2, decision.risk_tier, "{capability}");
            assert_eq!(
                "bounded_write_audited", decision.reason_code,
                "{capability}"
            );
        }
    }

    #[test]
    fn r3_broad_filesystem_write_needs_approval() {
        let decision = decide(input("filesystem.write").broad_or_unbounded());

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!(
            "filesystem_write_broad_or_unbounded_needs_approval",
            decision.reason_code
        );
    }

    #[test]
    fn r2_bounded_process_execution_is_audited() {
        let decision = decide(input("process.exec"));

        assert_eq!(PolicyOutcome::Audited, decision.outcome);
        assert_eq!(RiskTier::R2, decision.risk_tier);
        assert_eq!("bounded_process_audited", decision.reason_code);
    }

    #[test]
    fn r3_broad_process_execution_needs_approval() {
        let decision = decide(input("process.exec").broad_or_unbounded());

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!(
            "process_broad_or_unbounded_needs_approval",
            decision.reason_code
        );
    }

    #[test]
    fn r3_broad_repository_mutation_needs_approval() {
        let decision = decide(input("repository.mutate.broad"));

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!(
            "broad_repository_mutation_needs_approval",
            decision.reason_code
        );
    }

    #[test]
    fn r3_approval_unavailable_still_needs_approval() {
        let mut input = input("external.network");
        input.approval_available = false;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert!(!decision.approval_available);
        assert_eq!(
            "external_network_or_service_needs_approval",
            decision.reason_code
        );
    }

    #[test]
    fn r3_secret_access_needs_approval() {
        let decision = decide(input("secret.access"));

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!("secret_access_needs_approval", decision.reason_code);
    }

    #[test]
    fn r3_read_only_repository_external_network_still_needs_approval() {
        let mut input = input("external.network");
        input.current_trust_state = RepositoryTrustState::ReadOnly;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!(
            "external_network_or_service_needs_approval",
            decision.reason_code
        );
    }

    #[test]
    fn r3_read_only_repository_secret_access_still_needs_approval() {
        let mut input = input("secret.access");
        input.current_trust_state = RepositoryTrustState::ReadOnly;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::NeedsApproval, decision.outcome);
        assert_eq!(RiskTier::R3, decision.risk_tier);
        assert_eq!("secret_access_needs_approval", decision.reason_code);
    }

    #[test]
    fn r4_destructive_repository_action_is_blocked() {
        let decision = decide(input("repository.reset.hard"));

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!(
            "destructive_repository_action_blocked",
            decision.reason_code
        );
    }

    #[test]
    fn r4_unsupported_capability_is_blocked() {
        let decision = decide(input("calendar.invite"));

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("unsupported_capability_blocked", decision.reason_code);
    }

    #[test]
    fn r4_outside_registered_scope_is_blocked() {
        let decision = decide(input("filesystem.read").outside_registered_scope());

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("outside_registered_scope", decision.reason_code);
    }

    #[test]
    fn r4_blocked_repository_blocks_reads() {
        let mut input = input("filesystem.read");
        input.current_trust_state = RepositoryTrustState::Blocked;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("repository_blocked", decision.reason_code);
    }

    #[test]
    fn r4_blocked_repository_blocks_informational() {
        let mut input = input("capability.discovery");
        input.current_trust_state = RepositoryTrustState::Blocked;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("repository_blocked", decision.reason_code);
    }

    #[test]
    fn r4_read_only_repository_blocks_mutation() {
        let mut input = input("filesystem.write");
        input.current_trust_state = RepositoryTrustState::ReadOnly;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("repository_read_only", decision.reason_code);
    }

    #[test]
    fn r4_read_only_repository_blocks_process_execution() {
        let mut input = input("process.exec");
        input.current_trust_state = RepositoryTrustState::ReadOnly;

        let decision = decide(input);

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("repository_read_only", decision.reason_code);
    }

    #[test]
    fn capability_names_are_normalized() {
        let cases = [
            (" FILESYSTEM_READ ", "bounded_read_allowed"),
            ("filesystem-read", "bounded_read_allowed"),
            ("filesystem/read", "bounded_read_allowed"),
            ("filesystem-delete", "bounded_write_audited"),
            ("fs/move", "bounded_write_audited"),
            (
                "repository:reset:hard",
                "destructive_repository_action_blocked",
            ),
            ("SECRET-READ", "secret_access_needs_approval"),
        ];

        for (capability, reason_code) in cases {
            let decision = decide(input(capability));
            assert_eq!(reason_code, decision.reason_code, "{capability}");
        }
    }

    #[test]
    fn empty_capability_is_blocked() {
        let decision = decide(input("   "));

        assert_eq!(PolicyOutcome::Blocked, decision.outcome);
        assert_eq!(RiskTier::R4, decision.risk_tier);
        assert_eq!("unsupported_capability_blocked", decision.reason_code);
    }
}
