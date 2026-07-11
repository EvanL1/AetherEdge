# Aether Support

This community project provides support through public repository channels.
Support is best-effort; no response time, resolution time, compatibility, or
commercial support SLA is promised.

## Choose the right channel

- **Reproducible defect:** open a bug report using the repository issue
  template.
- **New capability or behavior change:** open a feature request.
- **Usage, architecture, or integration question:** use GitHub Discussions if
  it is enabled for the repository. If it is not enabled and the question
  reveals a likely documentation defect, open an issue describing the missing
  or misleading documentation.
- **Vulnerability:** do not use a public channel; follow
  [SECURITY.md](SECURITY.md).
- **Conduct incident:** follow the private reporting guidance in
  [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

Please search existing issues, discussions, documentation, and the
[changelog](CHANGELOG.md) first.

## Information that helps

For technical help, provide only non-sensitive information:

- Aether version or commit;
- operating system, architecture, and deployment method;
- whether this is the default SHM-only runtime or an explicitly enabled
  Redis/PostgreSQL extension;
- affected crate, service, protocol, CLI command, or MCP tool;
- minimal configuration and reproduction steps;
- expected and actual behavior;
- relevant sanitized logs and the exact verification command that failed.

Never post passwords, tokens, private keys, customer data, production device
identifiers, full memory images, or details that could enable a vulnerability.

## Support boundaries

The default supported architecture uses SHM as the sole authority for live
point state and requires no external database. Questions about a custom Redis
mirror, PostgreSQL history sink, field protocol, platform integration, or
industry pack should identify that extension explicitly.

Maintainers and community members may suggest diagnostics, documentation, or
patches, but they cannot validate the physical safety of a deployment. Device
control, interlocks, network segmentation, host hardening, regulatory
compliance, backups, and safe commissioning remain the operator's
responsibility.

Commercial support, private deployment review, and guaranteed maintenance are
not currently offered by this repository unless a separate provider states
its own terms outside the project.
