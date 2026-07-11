# Security Policy

Security matters especially at an edge gateway because Aether parses
untrusted protocol traffic, handles credentials, exposes local and remote
interfaces, and may dispatch commands to physical devices.

## Supported versions

Security fixes are applied to the current development branch and, when a
release is maintained, to the latest release line. Older releases and
unreleased downstream forks should not be assumed to receive fixes. Check the
latest release and changelog before reporting a version-specific issue.

## Report a vulnerability privately

Do not disclose a suspected vulnerability in a public issue, discussion, pull
request, log paste, or chat.

Use GitHub private vulnerability reporting:

1. Open the repository's **Security** tab.
2. Select **Advisories** and **Report a vulnerability**.
3. If private reporting is not available to you, ask a repository maintainer
   through a private contact method published on that maintainer's GitHub
   profile to open a draft Security Advisory. Do not include vulnerability
   details in a public request for contact.

Include, when available:

- affected version, commit, platform, and deployment mode;
- a concise impact assessment and the trust boundary crossed;
- reproducible steps or a minimal proof of concept;
- whether device control, credentials, SHM integrity, authentication,
  authorization, protocol parsing, or an optional extension is involved;
- suggested mitigations or a patch, if you have one;
- any disclosure constraints already known to you.

Remove real credentials, customer data, device identifiers, and production
memory images from the report. Use synthetic fixtures wherever possible.

## What happens next

Maintainers will use the private advisory to validate scope, discuss a fix,
coordinate disclosure, and credit reporters who want attribution. Response
and remediation timing depends on severity, reproducibility, affected
releases, and maintainer availability; this project does not promise a fixed
response SLA.

Please keep the report confidential until the advisory is published or the
maintainers agree that coordinated disclosure is complete. If a report is not
a security issue, maintainers may redirect the non-sensitive portion to the
normal issue or support workflow.

## Security design expectations

Changes must preserve Aether's baseline controls:

- Device commands are deny-by-default, permission checked, confirmation
  aware, and audited.
- AI clients are not part of deterministic hard real-time or safety loops.
- SHM is the authority for live state; mirrors and history stores cannot
  silently take over that role.
- External databases and network services are opt-in extensions, not default
  runtime requirements.
- Secrets and credentials must not be committed, logged, embedded in test
  fixtures, or placed in issue reports.

These expectations do not make every deployment secure by default. Operators
remain responsible for host hardening, network segmentation, least-privilege
credentials, physical safety controls, and timely updates.
