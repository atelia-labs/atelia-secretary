# Extensions Runtime

This document defines how Atelia Secretary validates, installs, executes,
audits, and rolls back Custom Extensions.

The normative AEP, extension, composition, hook, tool-output, and security
contracts live in the [`atelia`](https://github.com/atelia-labs/atelia)
repository:
[AEP](https://github.com/atelia-labs/atelia/blob/main/docs/aep.md),
[AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.md),
[Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md),
[Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.md),
[Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md),
[Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.md),
and [Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.md).
This document defines the daemon-side enforcement that gives Secretary a
trustworthy extension workplace. In AEP terms, this is the backend host
reference implementation slice.

## Extension Kinds

Extensions can declare multiple kinds. Each kind expands the visible permission
surface.

- `tool`: exposes callable tools to Secretary / agents
- `service`: exposes typed services other extensions can call through Secretary
- `tool_output_customizer`: controls result format, field order, omission,
  summaries, TOON/JSON defaults
- `hook_provider`: registers protected extension-created hooks
- `webhook_receiver`: owns external event endpoints and verification rules
- `workflow`: runs bounded multi-step jobs
- `notification`: sends or formats notifications
- `memory_provider`: provides scoped workplace memory or preference surfaces
- `memory_strategy`: controls how raw messages and compressed memory are
  maintained and passed into agent context
- `approval_agent`: reviews approval requests and submits bounded approval
  decisions
- `review`: participates in review, evidence, critique, or policy checks
- `review_agent`: acts as a review colleague for code, documents, or workflows
- `agent_provider`: connects external agent systems such as Codex, Claude,
  Devin, Jules, or CodeRabbit
- `delegated_agent`: adds a bounded colleague or subordinate agent that
  Secretary can assign work to
- `presentation`: changes human/agent-facing display
- `integration`: connects external services such as GitHub, Linear, or Slack

Semantic presentation is declared by AEP packages and rendered by presentation
hosts such as Atelia Mac and Atelia iOS. Secretary preserves the backend audit,
permission, and provenance facts that those hosts must display.

## Manifest

Secretary currently validates a backend-only compatibility slice. The public AEP
package schema is `aep.package.v0`; this daemon slice is still
`atelia.extension.v1` while implementation catches up.

```yaml
schema: atelia.extension.v1
id: com.example.extension
name: Example Extension
version: 1.2.3
publisher:
  name: Example Org
  url: https://example.com
description: Short purpose
types:
  - tool
  - hook_provider
compatibility:
  atelia_protocol: ">=0.1 <0.3"
  atelia_secretary: ">=0.1 <0.2"
entrypoints:
  realm: backend
  runtime: wasm-rust | wasm | process
  command: null
  image: null
  wasm: null
  protocol: atelia-extension-rpc.v1
permissions: {}
tools: []
services:
  provides: []
  consumes: []
tool_output: []
hooks: []
webhooks: []
composition:
  attachments: []
bundle:
  id: null
  required: null
failure:
  degrade: disable_extension | disable_feature | return_unavailable | require_human
  retry_policy: none | bounded
provenance:
  source: github | registry | local
  repository: null
  commit: null
  registry_identity: null
  artifact_digest: sha256:...
  manifest_digest: sha256:...
  signature: null
  signer: null
migration:
  from: []
  notes: null
```

`process` is local-development only and must be explicitly enabled. Docker,
remote runtime, and native client executable extension profiles are future or
special-purpose AEP profiles, not this beta backend slice.

These additive sections default to empty collections or null notes in the daemon
manifest model, so older manifests can still deserialize cleanly. Validation
only requires them when a manifest actually declares the corresponding
customized content, which keeps legacy installs working while still rejecting
section/type mismatches for new manifests.

The manifest is an enforceable contract. If runtime behavior exceeds the
manifest, Secretary blocks execution.

Initial backend enforcement lives in `atelia-core::extensions` as
`ExtensionManifest` validation plus an in-memory `ExtensionRegistry`. This
first slice accepts backend `wasm-rust` / `wasm` manifests, explicit
local-development process manifests, per-version provenance digests, blocklist
checks, rollback pointers, and brokered service-call authorization. It does not
execute WASM yet.

The initial implementation also reserves concrete id boundaries:

- official backend extensions use the `ai.atelia.*` namespace and the
  `atelia-official` registry identity
- local development extensions use the `local.*` namespace and require explicit
  unsigned dev-mode approval when unsigned
- third-party extensions cannot use the official or local namespaces

## Permission And Capability

Permissions and capabilities are distinct.

- `permission`: approved at install/update time
- `capability`: granted by Secretary for one execution

Examples:

- `repo.read`
- `repo.write`
- `repo.branch.create`
- `repo.pr.create`
- `repo.merge`
- `repo.destructive`
- `secret.read:name`
- `network.none`
- `network.allowlist`
- `network.github`
- `network.connect:host`
- `network.webhook.receive:source`
- `memory.read:scope`
- `memory.write:scope`
- `service.provide:name`
- `service.call:name`
- `notification.send:channel`
- `workflow.run`
- `workflow.run:workflow_id`
- `workflow.schedule`
- `workflow.delegate_agent`
- `review.comment`
- `review.approve`
- `review.request_changes`
- `hooks.create`
- `hooks.modify`
- `hooks.receive_external:source`
- `tool_output.customize:tool_id`
- `client.view.provide`
- `client.action.run`
- `client.settings.modify`
- `browser.use`
- `computer.use`

Capability grants include expiry, invocation id, actor, extension version,
requested operation, input digest, policy decision, and max effect.

Default lifecycle:

- R0/R1 grants can expire at task or session boundary.
- R2 grants expire at invocation boundary unless a workflow explicitly holds a
  short-lived task grant.
- R3/R4 grants expire at invocation boundary and carry approval refs.
- Grant renewal creates a new policy decision.
- Revocation happens on extension disable, blocklist hit, manifest mismatch,
  approval withdrawal, permission change, version change, policy update, or
  task cancellation.

## Tool Output Customization

Tool output customization is a security-sensitive AX extension.

Rules:

- raw observations remain immutable audit/evidence
- customizers can transform agent-facing rendering
- customizers receive only the scoped data they need
- error / blocked / approval state remains visible
- if customization fails, Secretary returns canonical output
- audit records include transformed output digest and customizer identity

Customizable:

- format: TOON / JSON
- field order
- omission
- redaction
- summarization
- token budget
- language
- per-tool default
- verbosity

## Service Broker

Secretary brokers service calls. Extensions do not communicate with each other
directly.

For each service call, Secretary verifies:

- caller extension id / version
- callee extension id / version
- service name, method, and schema version
- caller `services.consumes` declaration
- callee `services.provides` declaration
- required permission and capability grant
- input digest, output digest, redaction, and failure

Service calls inside the same bundle still require explicit provide / consume
declarations and permissions. The bundle install flow can group approval UI, but
it does not automatically elevate permissions.

When a service dependency is missing, Secretary treats the extension as
partially unavailable or as blocked composition and returns structured
unavailable status.

## Hook And Webhook Execution

Hooks are persisted objects.

```yaml
hook_id: hk_...
created_by:
  kind: user | extension | automation
  extension_id: null
trigger:
  source: atelia | github | external
  event: pull_request.opened
  condition: null
verification:
  method: hmac | github_signature | oidc | none_for_local_only
required_capabilities: []
action:
  kind: workflow | tool | notification | memory_update | extension_action
status: enabled | disabled | blocked | needs_approval
```

Rules:

- user-created and extension-created hooks are shown separately
- extension-created hooks are protected objects
- users can disable or block extension-created hooks
- behavior-changing edits fork the hook or require extension update
- trigger, event source, verification, or permission changes require re-approval
- external webhooks require signatures, timestamp windows, delivery id dedupe,
  and source allowlists
- hook executions record input digest, policy decision, state changes, failure
  reason, and block reason

Secretary can inspect, pause, disable, and reroute hooks to keep judgment
visible.

## Provenance And Signature

Before install/update, Secretary verifies per-version provenance:

- artifact digest
- manifest digest
- source repository
- commit / tag
- registry identity
- signer identity
- signature over manifest and artifact digest
- build provenance when available

Same-version different-digest extensions are blocked. Local unsigned extensions
require explicit dev-mode approval and visible client labeling.

## Runtime And Sandbox

Beta backend runtime matrix:

- WASM (Rust): first-class runtime for backend extensions
- WASM (non-Rust): reserved for reviewed cases that still fit the WASM host
  boundary
- process: local-development only and disabled unless policy explicitly enables
  it

Reserved future or special-purpose AEP runtime profiles:

- Docker: heavy work that does not fit WASM and requires stricter host policy
- remote: future / explicitly permissioned runtime
- native client executable extension: future, platform-specific, high-trust
  profile outside the Secretary backend host

Secretary-side backend extensions use Rust -> WASM as the primary target.
Official backend extensions are written in Rust by default. Community backend
extensions initially use or strongly prefer Rust / WASM.

Atelia does not treat arbitrary WASM as inherently safe. Rust provides a typed
SDK and memory-safe default, while WASM provides controlled host capabilities,
linear memory isolation, fuel / timeout limits, and auditable imports. Non-Rust
WASM, process, Docker, remote, and native client executable profiles require
stricter review, capability limits, provenance checks, and runtime policy.

Defaults:

- no host Docker socket mount
- no ambient filesystem / network / env / credentials / repo access
- repositories are read-only by default
- write access requires `repo.write`
- secrets are scoped short-lived handles
- CPU, memory, wall-clock, output-size, and network egress limits apply

Missing CLIs or API keys return structured unavailable status.

## AEP Bundles

Secretary can treat an `aep.bundle.v0` manifest as a meta install / update /
rollback unit for multiple AEP packages. A bundle is not the mechanism for
combining one product's backend, presentation, and resource components; those
belong inside one multi-component AEP package.

Bundle records include:

- bundle id / version
- included package ids and version ranges
- required / optional package status
- approved permission diff across included packages
- service dependency graph across included packages
- AEP profile / Atelia Protocol / Secretary / client compatibility range
- bundle provenance and signature
- rollback snapshot for package membership and approved versions

Bundle install, update, and rollback should be atomic for required packages. If
a required package fails, the bundle is not installed. If an optional package
fails, package manifest degradation applies.

When multiple bundles include the same package, Secretary can share one package
install. It still checks version range, permission diff, service dependency, and
composition attachment compatibility. Same-bundle membership never grants
service access automatically.

## Install, Update, Rollback

Secretary stores durable install records:

- installed version
- approved manifest digest
- approved permissions
- runtime artifact digest
- previous version
- migration state
- rollback snapshot

Updates that change permissions, hooks, webhooks, runtime, source, or trigger
conditions require re-approval.

Rollback restores previous version, disabled hooks, removed capabilities, and
extension-owned state. Migrations include dry-run, touched data areas, and
rollback notes.

Rollback state:

1. `installed`
2. `updating`
3. `quiescing_running_jobs`
4. `rollback_in_progress`
5. `installed_previous_version`

In-flight extension jobs are cancelled when cancellation is supported. Jobs with
external side effects are quarantined for review. Hook deliveries received during
rollback are held until the previous version is restored or the extension is
blocked.

## Blocklist

Blocklist wins over local enablement.

Block keys:

- extension id
- version range
- artifact digest
- signer
- publisher
- source repository
- permission pattern
- vulnerability id

Block reasons:

- `malware`
- `manifest_mismatch`
- `over_permissioned`
- `vulnerable_version`
- `compromised_signer`
- `policy_violation`
- `user_blocked`
- `registry_removed`

Checks run at install, update, startup, and before execution. Running jobs are
cancelled or quarantined.

## Audit Events

Minimum events:

- registry lookup
- manifest validation
- compatibility check
- install approval
- update approval
- permission grant
- capability grant
- hook creation / change
- webhook receipt
- extension execution start / end
- policy decision
- secret access
- repo mutation
- external network call
- blocklist hit
- rollback
- output customization transform

Plaintext secrets stay out of logs.

## Agency Preservation

Extensions support Secretary's judgment.

Review questions:

- Does this extension help Secretary make a better judgment?
- Does it make every judgment-affecting change visible to Secretary?
- If it changes memory, notifications, review flow, delegation, or tool
  defaults, does the manifest declare the impact?
- Can Secretary inspect what changed, why it changed, and how to undo it?
- Do failures feed AX Feedback and workplace improvement?
