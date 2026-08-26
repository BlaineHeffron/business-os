# Client Overlay Policy

This public repository contains only generic BusinessOS code.

Store each client overlay in a separate private repository.

An overlay can contain these items:

- A client identity and live identifiers
- Enabled workflow modules
- Client rules and business policies
- Seed data and imported baselines
- Provider account references
- Private hosts and deployment settings
- Secret references

Do not commit credentials to an overlay repository.

Use a secret store for credential values.

Public tests and examples must use invented names and values.

Changes to the public extension interface must remain useful without an overlay.
