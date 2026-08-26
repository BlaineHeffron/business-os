# Security Policy

## Supported versions

The project supports the current default branch.

Older commits do not receive security fixes.

## Report a problem

Use the private security advisory feature for the repository.

Do not report a security problem in a public issue.

Include the affected revision, the impact, the reproduction steps, and a proposed fix when available.

Do not include real credentials, personal data, or client data in the report.

The maintainers will confirm the report and coordinate a fix before public disclosure.

## Deployment responsibility

BusinessOS can connect to external providers.

Keep write functions disabled until you review the provider configuration and approval controls.

Store credentials outside the repository.

Use a dedicated secret store for production deployments.
