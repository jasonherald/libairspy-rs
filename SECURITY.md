# Security Policy

## Supported Versions

Only the latest release on the `main` branch is supported with security
updates. We do not backport fixes to older versions.

| Branch | Supported |
|--------|-----------|
| `main` | Yes |
| Other  | No  |

## Reporting a Vulnerability

**Please do not open a public issue for security vulnerabilities.**

Use GitHub's private vulnerability reporting to submit a report:

1. Go to the [Security tab](https://github.com/jasonherald/libairspy-rs/security)
2. Click **"Report a vulnerability"**
3. Provide a description, steps to reproduce, and any relevant details

### Alternative reporting

If you cannot use GitHub's private reporting, email
**security@aaru.network** with a description, steps to reproduce, and
any relevant details.

### What to expect

- **Acknowledgment** within 48 hours
- **Assessment** of severity and impact within 1 week
- **Fix or mitigation** as soon as practical, depending on severity
- **Disclosure** 90 days after the fix is released, or immediately if
  the vulnerability is already public
- Credit in the fix commit (unless you prefer to remain anonymous)

## Security Scanning

This project uses automated security scanning:

| Tool | Integration | Coverage |
|------|-------------|----------|
| [cargo-audit](https://rustsec.org/) | GitHub Actions (PR + weekly) | Known CVEs in Rust dependencies (RustSec advisory database) |
| [cargo-deny](https://embarkstudios.github.io/cargo-deny/) | GitHub Actions (PR + weekly) | License compliance, banned crates, dependency sources |
| [CodeQL](https://codeql.github.com/) | GitHub Actions (PR + weekly) | Workflow (GitHub Actions) security analysis |
