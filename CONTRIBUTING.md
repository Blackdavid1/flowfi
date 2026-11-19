# Contributing

Thanks for helping improve this project. This guide covers local setup, the review workflow, and how to verify multi-network deployments.

## Getting started

1. Fork the repository and create a topic branch from `main`.
2. Install dependencies and run the existing checks locally before opening a PR.
3. Keep changes focused: one capability per pull request.

## Task ownership and review workflow

- Every pull request should have a single owning contributor who is accountable for the change from draft to merge.
- Open PRs as drafts until the change is ready for review; mark them ready only when local checks pass.
- Request review from at least one maintainer familiar with the touched area (contracts, backend, or operations).
- Address review comments with follow-up commits rather than force-pushes so reviewers can see the delta.
- A PR may be merged once it has an approving review, green CI, and no unresolved blocking comments.
- If you must step away from an owned task, hand it off explicitly in the PR thread so ownership is never ambiguous.

## Multi-network deployment decision guide

Operators must select and verify a consistent combination of **network, contract, token, endpoint, and release artifact** for each environment. Mixing values across environments is the most common cause of failed or unsafe deployments.

### Environments

| Environment | Purpose | Typical network |
| --- | --- | --- |
| `local` | Developer iteration | Local node / dev chain |
| `testnet` | Integration and E2E validation | Public testnet |
| `mainnet` | Production | Public mainnet |

### Selection rules

1. **Network** — pick the network that matches the environment. Never point a testnet deployment at mainnet RPC endpoints or vice versa.
2. **Contract** — use the contract address deployed for the selected network. Contract addresses are network-specific and must not be reused across networks.
3. **Token** — confirm the token address/decimals belong to the same network as the contract. Token and contract must be co-located on one chain.
4. **Endpoint** — the RPC/API endpoint must serve the selected network. Verify the chain ID reported by the endpoint matches the expected network before deploying.
5. **Release artifact** — deploy the artifact built for the target environment. Do not promote a testnet artifact to mainnet without rebuilding and re-verifying.

### Preflight checklist

Before running any deployment, confirm each item:

- [ ] Target environment is explicitly set (`local`, `testnet`, or `mainnet`).
- [ ] RPC endpoint chain ID matches the selected network.
- [ ] Contract address is the one deployed on that network (not copied from another environment).
- [ ] Token address and decimals match the same network as the contract.
- [ ] Release artifact was built for the target environment and its checksum is recorded.
- [ ] Required secrets are present in the environment and are not committed to the repository.
- [ ] A dry run or testnet rehearsal has passed for the same artifact.

### Safe failure examples

These are expected, safe failures — the deployment should stop rather than proceed:

- **Chain ID mismatch** — endpoint reports a different chain ID than the selected network. Abort; do not override the check.
- **Unknown contract address** — the contract address has no record for the selected network. Abort and deploy the contract first.
- **Token/contract network mismatch** — token belongs to a different network than the contract. Abort; re-select matching values.
- **Missing artifact** — no release artifact exists for the target environment. Abort; build and verify the artifact.
- **Missing secrets** — required credentials are absent. Abort; never substitute placeholder or shared credentials.

### Verifying against current configuration

Before changing deployment instructions, cross-check them against the repository's current deployment configuration (for example the deploy workflow, testnet E2E configuration, and backend environment example). If the configuration and this guide disagree, update the guide in the same PR that changes the configuration.

## Contract reference tables

Contract entrypoints, their preconditions, authorization roles, emitted events, and terminal behavior are documented as reference tables so reviewers can check them against the contract tests. Keep these tables in sync with the contract source and tests in the same PR that changes contract behavior.

### State transitions

Each row lists a transition, the state it is allowed from, the resulting state, and whether it is supported. Transitions marked **unsupported** must be rejected by the contract and covered by a failing test.

| Transition | From state | To state | Supported |
| --- | --- | --- | --- |
| `create` | (none) | `Active` | Yes |
| `fund` | `Active` | `Active` | Yes |
| `complete` | `Active` | `Completed` | Yes |
| `cancel` | `Active` | `Cancelled` | Yes |
| `complete` | `Completed` | `Completed` | No — terminal state |
| `cancel` | `Completed` | `Cancelled` | No — terminal state |
| `fund` | `Cancelled` | `Active` | No — terminal state |
| `complete` | `Cancelled` | `Completed` | No — terminal state |

### Authorization roles

| Entrypoint | Allowed caller | Preconditions | Emitted event |
| --- | --- | --- | --- |
| `create` | Any wallet | Valid parameters | `Created` |
| `fund` | Funder wallet | State is `Active`; sufficient balance | `Funded` |
| `complete` | Owner / authorized role | State is `Active` | `Completed` |
| `cancel` | Owner / authorized role | State is `Active` | `Cancelled` |

Unauthorized callers and calls against terminal states must fail without changing state or emitting events. Do not relax these authorization or state checks to make a change pass.

### Validating the tables

- Cross-check every row against the contract tests; a documented transition without a matching test (or vice versa) is a gap to fix in the same PR.
- Explicitly list unsupported transitions and the failure they produce rather than omitting them.
- When contract behavior changes, update these tables and the corresponding tests together.

## Automated coverage

Changes to deployment selection or verification behavior should include focused automated tests covering:

- the happy path for each environment,
- each safe-failure case above (chain ID mismatch, unknown contract, token/contract mismatch, missing artifact, missing secrets),
- boundary cases such as an empty network selection or an artifact built for a different environment.

Changes to contract behavior should include focused automated tests covering:

- each supported state transition and its emitted event,
- unauthorized callers for each restricted entrypoint,
- invalid and unsupported transitions (including calls against terminal states),
- boundary cases such as empty or malformed parameters.

## Submitting changes

- Keep commits scoped and descriptive.
- Update the owning documentation (API, contract, operational, or contributor docs) alongside behavior changes.
- Do not weaken wallet authorization, chain/network validation, financial accounting invariants, or secret-handling requirements to make a change pass.
