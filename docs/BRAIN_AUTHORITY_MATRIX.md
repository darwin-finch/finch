# Brain authority conformance matrix

The executable inventory is `brain::authority::BRAIN_AUTHORITY_MATRIX`. Each
operation names its transport and one non-interchangeable authority kind:

- participant credentials are bound to Brain ID, canonical name, environment
  generation, exact scope, validity interval, revocation state, and (for live
  operations) an attachment plus connection;
- local IPC operations require connection-owned authority rather than copied
  UUIDs;
- runner callbacks require the registered lease authority;
- node administration requires a node-administrator credential that cannot be
  represented by a participant scope or invitation.

The generated credential test applies exact-authority and negative audience,
generation/policy replacement, expiry, and revocation cases to every remote
participant row. Existing focused conformance cases cover the stateful axes:

| Axis | Executable evidence |
| --- | --- |
| Possession versus authority | `possession_of_identifiers_never_satisfies_local_or_remote_matrix_rows`; IPC connection-authority hostile replay tests |
| Audience and policy replacement | `authority_matrix_generates_remote_credential_conformance_cases` |
| Expiry and signed clock skew | generated matrix test; `expiry_is_exclusive_and_future_credentials_fail`; invitation skew tests |
| Revocation and restart | generated matrix test; `revocation_survives_authority_restart` |
| Delegation attenuation/chain | `attenuation_cannot_add_authority_or_outlive_its_parent`; ancestor and malformed-chain tests |
| Invitation replay/restart | single-use, retry-safe, concurrent replay, and restart tests in `brain::credential::tests` |
| Mutation replay/restart | remote mutation receipt and daemon-restart tests in `server::handlers` and `brain::remote` |

## Open rows

`brain.create`, `brain.list`, `brain.archive`, and `brain.delete` deliberately
remain `NodeAdministrator` rows. Main does not yet implement that credential,
so these rows have only separation tests, not end-to-end positive and hostile
transport conformance. Compute submission, external effects, and a general
policy identity beyond the environment generation have contract rows and
credential-level negatives, but no end-to-end transport implementation yet.

Therefore the delegated-administration implementation in issue #110 may be
examined as an untrusted candidate, but it is not ready for independent merge
review until it implements the node-administrator rows and passes generated
cross-node, invitation-escalation, attachment-escalation, expiry/skew,
revocation-chain, replay, restart, and policy-replacement cases.
