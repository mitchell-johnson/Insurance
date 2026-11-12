# Group scheme administration

`crates/group` (`ic-group`) runs employer schemes: the scheme's terms, the membership files an employer sends, who the members are, cover above the automatic amount, leavers and their continuation option, pool declarations, renewal and ending, and the employer's view of the invoice. It follows `docs/modules.md`.

## What it does

- **Scheme terms as data** (`terms.rs`). Basis (named members, unnamed members, or pools), eligibility, when a leaver's cover ends, backdating the employer may do alone, the safety share, leave of absence, takeover terms, billing groups, continuation products and period, renewal lead time and what happens with no agreement, how underwriting loadings are billed, and when short payment suspends joiners. Terms are checked whole, versioned, and replaced whole.
- **Files** (`csv.rs`, `mapping.rs`, `diff.rs`, `state.rs`). A file is stored in quarantine, scanned, and only then read. It is read with the scheme's sealed mapping (the employer's own column names, date format, salary unit, category and change codes). A full file is compared with membership as the last file left it; a change file is applied row by row. The result is a preview: joiners, leavers, changes, absences, re-keyed IDs, rows rejected with a code and a field name, and how many members would be above automatic cover. One file per scheme is open at a time, so each is compared with the result of the one before. The same content twice is recognised by its hash.
- **Controls on a file.** A full file that would remove more than the safety share of members is held until staff confirm it. The employer approves every file; a scheme can require that the approver is not the uploader. Changes dated further back than the terms let the employer date them wait for staff, who can let them through or drop only those rows. A scheme can be frozen so that it takes and approves no files. A file not finished within the terms' limit is raised to staff.
- **Who a member is (addendum entry 2).** On a named scheme each joiner is matched with the customer master after approval. One high-confidence match links the member to that person; no candidate creates the person; anything between joins the member unlinked, tells reinsurance of each candidate, and raises an exception for staff to decide. The only other way a member is linked is their own verified registration. Names go to the customer master and nowhere else: the module's state holds a salted fingerprint, and its events carry IDs, counts and field names.
- **Applying** (`work.rs`). Approved changes go to the core as `ApplyMembership` in chunks, one command in flight per scheme so the core sees them in the file's order. A busy core is retried the next morning under a new key. A refused chunk is halved until the refused row is alone; that row is reported on the file and raised to staff, and the rest applies. A refusal about the policy itself (lapsed, transferred) stops the scheme's queue with everything in it until staff resume. Changes dated ahead wait and are sent on their day.
- **Cover above the automatic amount.** When the formula gives more than the automatic acceptance limit, the excess is a case sent to underwriting. An accepted decision is written to the core as a second layer for the accepted amount (`AddExcessLayer`); the employer learns only whether it is in place. When salary moves, open requests are withdrawn and layers ended, newest first, until what is held fits the formula, and any new excess is asked for. Reinsurance can require part of a member's automatic cover to be underwritten (accumulation across the member's policies): the member's automatic cover is capped in the core and the rest becomes a case under reinsurance's reference. Loadings are billed as one scheme total, or itemised on the member's line only with the member's recorded consent.
- **Leavers.** A leaver's layers end with the membership. A leaver young enough gets a continuation option for up to the automatic cover held; taking it up issues a retail policy in the core to the member, starting the day scheme cover ended, with no evidence of health. An option not taken up expires. A leave of absence past the terms' limit ends cover on the terms' date. Members removed in error are reinstated from the day they were removed, with their layers, as if they had not left.
- **Pools.** A pool scheme has no files: the employer declares headcount and total salary per pool, each declaration replaces the last from its date, and a declaration that is late is raised.
- **The scheme year.** Renewal opens at the lead time. Agreed terms take effect on the renewal date. Under a rate guarantee the year rolls on the same terms. With no agreement the scheme runs on old rates for the days allowed, raised to staff, and then ends, or ends at once, as the terms say. Ending a scheme, by staff or by renewal, removes every member on the date (each with a continuation option) and cancels the policy.
- **Money as the employer sees it.** From the core's events the module keeps each invoice by billing group, retrospective adjustments against the group they belong to, loadings as one total, and the balance. A scheme that pays short as many times in a row as its terms allow takes no new joiners until it has paid; existing members are untouched.
- **Refusals leave no trace.** A refused command changes nothing. A repeated command gets its first result, marked as a repeat.

## How it runs

Three parts.

1. **The state machine** (`GroupState`) is hosted on every ledger node in its own replicated group, as module `group` (`ic_group::module_spec()` in `ic_node::modules()`). It reads the core's events: the business date (its daily tick), scheme invoices, adjustments and balances, and underwritten layers. It never reads a clock or the network. Everything it wants done outside is an entry in its outbox, which is part of its replicated state.
2. **The driver** (`driver.rs`, `adapters.rs`) carries the outbox: commands to the core as caller `group-admin` (the core's answer goes back as `CoreResult`, and only then does the entry leave), notices to the reinsurance module, requests and withdrawals to the underwriting module (whose answers it reads back from that module's outbox and records as decisions), joiners to the customer master, and events to the `group-events` hub on Event Hubs. It also takes each stored file one step: record the scan result, or read, map and submit its rows. Every result is a command whose key comes from the work itself, so a driver stopped anywhere carries on when restarted and nothing is done twice. An entry that cannot be delivered stays in the outbox and does not hold up the ones behind it.
3. **The service** (`service.rs`, binary `ic-group`) runs the driver on a timer and serves the routes below. It holds no state: any number can run.

Commands are an `Envelope` (`key`, `caller`, `actor`, `today`, `command`). Callers are fixed names: `group-admin` (staff), `employer-portal`, `member-portal`, `underwriting`, `reinsurance`, `cash-matching`, and `group-driver` for the driver's own. Questions (`host::Question`) cover a scheme's standing, a file with its preview and row results, invoices, open exceptions, and what the driver has to do.

### Routes

Callers present the service key as a bearer token. The service sits behind the portal gateway, which has established who the person is.

- `POST /v1/schemes/{scheme}/files/{file}` with the file as the body and `x-ic-caller` (`employer-portal` or `group-admin`) and `x-ic-actor`. The bytes are stored in quarantine before the module hears of the file. 202 on success. Limit 32 MB.
- `POST /v1/commands` with `{key, caller, actor, command}`. The business date is the core's. The driver's commands and `receive_file` are not accepted here.
- `POST /v1/questions` with a question. The outbox is not served.
- `GET /healthz`.

422 carries the module's reject code. 503 means nothing is known to have happened: send the same request again with the same key.

### On Azure

- **Blob Storage** holds uploaded files in a storage account of their own (`infra/group.bicep`), Entra ID only, deleted after 35 days by a lifecycle rule.
- **Microsoft Defender for Storage** scans each blob on upload and writes the result as the index tag `Malware Scanning scan result`. The driver reads the tag: no tag yet means wait, `No threats found` means clean, and anything else refuses the file. The hash taken at upload is checked again when the file is read.
- **Event Hubs** carries the module's events on `group-events`, one partition, each with `event-id`, `seq`, `module` and `type` properties.
- **Workload identity** for the service (`AzureCredential::from_env`), with Storage Blob Data Contributor on the container and Event Hubs Data Sender on the hub and nothing else.
- **Key Vault** holds the two secrets, mounted into the pod: the fingerprint salt and the service key.

### Settings

| Variable | Meaning | Default |
| --- | --- | --- |
| `IC_CLUSTER_NODES` (or `IC_NODES`) | Ledger nodes, `host:port` separated by commas | required |
| `IC_GROUP_QUARANTINE_ENDPOINT` | Blob endpoint of the quarantine account | required |
| `IC_GROUP_QUARANTINE_CONTAINER` | Container | `group-quarantine` |
| `IC_GROUP_EVENT_HUBS_NAMESPACE` | `https://<namespace>.servicebus.windows.net` | required |
| `IC_GROUP_EVENT_HUB` | Hub | `group-events` |
| `IC_GROUP_CUSTOMER_MASTER`, `IC_GROUP_CUSTOMER_MASTER_SCOPE` | Customer master URL and its Entra ID scope | required |
| `IC_GROUP_REINSURANCE_MODULE` | Name the reinsurance module is hosted under | `reinsurance` |
| `IC_GROUP_UNDERWRITING_MODULE` | Name the underwriting module is hosted under | `underwriting` |
| `IC_GROUP_FINGERPRINT_SALT` or `IC_GROUP_FINGERPRINT_SALT_FILE` | Secret, at least 32 characters, or the file holding it. Never change it: every member would look new | required |
| `IC_GROUP_API_KEY` or `IC_GROUP_API_KEY_FILE` | Secret, at least 32 characters, or the file holding it | required |
| `IC_GROUP_LISTEN` | Listen address | `0.0.0.0:8080` |
| `IC_GROUP_CYCLE_SECONDS` | Seconds between passes when idle | `5` |

The cluster token is read as every cluster client reads it (`ClusterAuth::from_lookup`). `deploy/k8s/group.yaml` is the deployment: two replicas, the three secrets mounted from Key Vault, reachable only from the portal service and the API.

## Operating it

- **Exceptions** (`{"ask":"exceptions"}`) are the work queue for staff: possible matches, held files, refused rows, a blocked scheme, stale files, overdue declarations, persistent short payment, renewal not agreed. Each is published as an event when raised and when cleared.
- **A pass that leaves work waiting** logs a warning with the count. Work waiting for days on one peer shows as the same count every pass; the entries are in the outbox with the peer's name.
- **A scheme is blocked** (`scheme_blocked` with the core's code): fix the cause on the policy (for example reinstate a lapsed scheme), then send `resume`. Nothing was dropped.
- **An employer file removed people in error.** Send `freeze` so the next automatic file is not taken. Send `reinstate` with the employee IDs: each is put back from the day they were removed, with underwritten layers restored and open continuation options withdrawn. Ask the employer for a corrected file, then `unfreeze`.
- **A file is stuck in quarantine.** No scan tag has appeared. Check Defender for Storage on the quarantine account (the monthly scan cap in particular). Nothing reads the file meanwhile; after the scheme's file age limit it is raised as stale. `withdraw_file` frees the scheme for another upload.

## Tests

`cargo test -p ic-group`. The state machine is tested against the real core (`ic-sim`), with a stand-in for the driver that carries commands, answers and events exactly as the driver does: intake, applying, cover above the automatic amount, leavers, the scheme year, billing, administration and the host's seams. The driver is tested with each outside party failing in turn, the Azure adapters against the stand-in that speaks the Blob Storage and Event Hubs protocols, and the whole arrangement on a real node (`tests/cluster.rs`).

## Known gaps

- **The hand-off to underwriting is tested by shape and against the hosted module, not through a whole decision.** A request reaches the underwriting module as a proposal with entry `group_excess`, named `grp-uw:{scheme}:{case}`, for one life and the excess amount; a request nobody wants any more is withdrawn; accepted terms and endings come back as decisions, carried by underwriting's driver and by this one under the same key (`uw:{effect key}`), so each is recorded once; and when the core has written the layer, or refused it, underwriting is told (`policy_issued` with the layer, or `issue_rejected`) so it can confirm reinsurance capacity. `tests/cluster.rs` checks every one of these messages against that module's own types, and the request against the module on a real node; no test here takes a member through disclosure to an accepted layer with both services running. Limits of the hand-off: the proposal carries no bound quote (the premium is the scheme's) and gives the policy's ID as its number; underwriting is not told the amount a previous insurer had accepted, the facultative path, or the reinsurance reference; and it has no step where the member agrees to the employer seeing a loading, so on a scheme whose terms itemise loadings an accepted answer with a loading is refused (`consent_required`), logged, and the case stays open. A request for a member not yet linked to a person waits until staff resolve the match.
- **Reinsurance notices are tested by shape, not end to end.** The reinsurance module is hosted under the name `reinsurance` and notices (scheme terms, risk factors, possible matches) are sent to it. The command shapes are checked against that module's own types in `tests/cluster.rs`; no test yet runs both modules on one node and follows a notice into its accumulation.
- **No customer master exists in the repository.** `HttpCustomerMaster` calls `POST /v1/parties/match`, which is this module's proposal for that service. Until one answers, files on named schemes stop at matching; unnamed and pool schemes are unaffected.
- **Loadings billed to the member are not built.** A scheme's terms may bill loadings to the employer as one total or itemised with consent. Member-paid loadings, and the rule that hides a total when too few members are loaded for it to be anonymous, need voluntary cover billing, which the core does not have. Terms asking for either are refused.
- **A continuation policy carries automatic cover only.** The option is for up to the automatic amount at leaving, on the retail product's standard terms. Underwritten excess and its terms are not carried over.
- **One file layout per scheme.** A scheme has one mapping, for full files or for change files. Changing between them is a new mapping version.
- **Not built:** splitting a scheme in two, delegated underwriting authority for an employer's broker, and a menu of health plans for members to choose from.
- **The service key is one shared key.** Who the person is comes from the gateway in front; the service trusts `caller` and `actor` from any holder of the key. It must not be exposed beyond the gateway and the other services.
- **`infra/group.bicep` has not been deployed.** CI compiles it with the other templates, but `main.bicep` does not reference it yet, so it is deployed on its own into the same resource group, and the Defender for Storage resource uses a preview API version. `deploy/k8s/group.yaml` is in the kustomization and has likewise not been applied to a cluster.
