# Aether Governance

Aether uses a lightweight, maintainer-led governance model. The goal is to
make decisions in the open while protecting the safety and architectural
integrity expected of an IoT edge kernel.

## Roles

### Users

Users run Aether, report problems, request capabilities, and share deployment
experience. No repository permission is required.

### Contributors

Contributors improve code, tests, documentation, packs, examples, or issue
analysis. A merged contribution does not automatically grant repository
permissions.

### Reviewers

Reviewers are trusted contributors who regularly provide accurate, timely
technical review in an area. Maintainers may recognize reviewers informally
or through repository ownership metadata. Reviewers do not merge changes
unless they are also maintainers.

### Maintainers

Maintainers are repository collaborators with merge or administrative rights.
They triage issues, review and merge changes, manage releases and security
advisories, enforce the Code of Conduct, and safeguard project invariants.
Repository permissions are the authoritative record of current maintainers;
this document does not invent a separate membership list.

## Decision making

Routine, reversible changes are decided through normal pull-request review.
The author presents evidence, reviewers raise concerns, and a maintainer
merges when required checks pass and material objections are resolved.

Changes with broad or difficult-to-reverse consequences require public design
discussion and an ADR. This includes changes to:

- SHM live-state authority or process isolation;
- the `domain <- ports <- application <- runtime/interfaces` dependency
  direction;
- default external-service requirements;
- public SDK contracts or compatibility policy;
- AI command permissions, confirmation, audit, or physical-control safety;
- release, licensing, or governance policy.

The project seeks reasoned consensus, not unanimity. When consensus is not
available, maintainers decide based on user impact, safety, architectural
consistency, maintenance cost, and evidence in the discussion. The decision
and its trade-offs must be recorded in the pull request, issue, or ADR.

Maintainers must not approve their own security-sensitive or governance change
when another maintainer is reasonably available to review it. Conflicts of
interest should be disclosed and the affected maintainer should recuse from
the final decision when practical.

## Becoming a reviewer or maintainer

Maintainers may nominate a contributor who has demonstrated sustained,
constructive participation, sound technical judgment, respect for project
boundaries, reliable review, and adherence to the Code of Conduct. Existing
maintainers decide the nomination through a documented repository discussion
or pull request and then update repository permissions or ownership metadata.

There is no contribution-count threshold and no entitlement to elevated
permissions. Access follows demonstrated trust and current project need.

## Inactivity and removal

A maintainer may step down at any time. Administrative access may also be
removed after prolonged inactivity, loss of access security, an unresolved
conflict of interest, or a serious Code of Conduct or security-policy breach.
Except where immediate action is needed to protect people or the repository,
the reason and transition should be documented and the maintainer given an
opportunity to respond privately.

## Releases and security

Maintainers control release publication and supported-version decisions.
Security reports are handled privately according to
[SECURITY.md](SECURITY.md); embargoed details are not subject to public design
discussion until coordinated disclosure is safe.

## Changing governance

Governance changes are proposed as pull requests to this file. The proposal
must explain the problem, transition, permission effects, and safeguards.
Approval requires maintainer review and should not be bundled with unrelated
code changes.
