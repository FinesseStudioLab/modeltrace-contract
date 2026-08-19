# Security Policy

ModelTrace handles attestation, metering, and settlement of AI inference. A
vulnerability here can mean forged audit evidence or misdirected funds, so we
treat reports seriously and will credit reporters who follow this process.

## Reporting a vulnerability

**Do not open a public issue for an undisclosed vulnerability.**

Use GitHub's private reporting: **Security → Advisories → Report a vulnerability**
on this repository. If that is unavailable, contact the maintainers listed on the
[FinesseStudioLab](https://github.com/FinesseStudioLab) organization profile.

Please include:

- Affected component and version or commit SHA
- Reproduction steps or a proof of concept
- Impact assessment — what an attacker gains
- Any suggested remediation

## Response targets

| Stage | Target |
| --- | --- |
| Acknowledgement | 48 hours |
| Initial assessment | 5 business days |
| Fix or mitigation plan | 30 days for high/critical |

## Scope

In scope: contract logic, authorization boundaries, arithmetic and rounding in
metering or settlement, API authentication and authorization, secret handling.

Out of scope: findings that require a compromised maintainer machine, denial of
service via unbounded self-funded transaction volume on public testnets, and
issues in third-party dependencies with no exploitable path through this code
(report those upstream, but tell us so we can pin or patch).

## Disclosure

We aim to coordinate disclosure once a fix ships. Contracts already deployed to
mainnet may warrant a longer embargo; we will tell you if so and why.
