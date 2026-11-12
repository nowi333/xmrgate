# Security policy

xmrgate handles payments, so security reports are taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private reporting instead:
[Report a vulnerability](https://github.com/nowi333/xmrgate/security/advisories/new).

Include what you found, how to reproduce it, and its impact. You will get an answer within a
few days, and credit in the release notes if you wish.

## Scope

xmrgate only ever holds a **view key**: it can see incoming payments but cannot move funds.
Reports that matter most:

- a payment marked as paid when it was not (or paid for the wrong amount),
- one website's key reading or affecting another website's invoices,
- forged or replayable webhooks,
- access to the dashboard or the view key without the password.

See the security model in the [README](README.md#security-model).
