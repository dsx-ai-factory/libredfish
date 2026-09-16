# Security Policy

## Reporting a Vulnerability

Please report suspected vulnerabilities privately. **Do not open a public issue or disclose vulnerability details in a public discussion or pull request.**

Use one of these channels:

1. **NVIDIA Vulnerability Disclosure Program (preferred):** https://www.nvidia.com/en-us/security/
2. **Email NVIDIA PSIRT:** psirt@nvidia.com. Encrypt sensitive reports using the NVIDIA PSIRT PGP key: https://www.nvidia.com/en-us/security/pgp-key
3. **Private vulnerability reporting:** Use the repository's **Security** tab to submit a private report when that feature is available.

Include enough information to reproduce and assess the issue:

- Affected libredfish version or commit
- Vulnerability type and affected component
- Reproduction steps
- Proof of concept, if available
- Expected security impact
- Relevant deployment assumptions or BMC vendor/model information

NVIDIA PSIRT will acknowledge the report, assess its impact and scope, coordinate remediation, and communicate disclosure guidance through the reporting channel.

## Security Architecture & Context

libredfish is a Rust library for communicating with Redfish-compatible baseboard management controllers (BMCs). It is an outbound HTTPS client and does not expose an HTTP server, bind network listeners, or provide a local database or credential store.

**Software classification:** Library / SDK

**Repository Exposure Classification:** Public — the source is hosted in the publicly accessible `NVIDIA/libredfish` GitHub repository.

**Service Exposure Classification:** Internal-Sensitive (high confidence).

**Basis:** NICo uses libredfish as an internal, outbound BMC client library. NICo's authenticated APIs and operator workflows, along with internal controllers, invoke it for privileged firmware, account, Secure Boot, boot, power, and hardware-management operations. libredfish itself exposes no inbound service.

### Key Components and Interfaces

- `src/lib.rs` defines the public `Redfish` trait, including account, power, firmware, BIOS, Secure Boot, TPM, and lockdown operations.
- `src/network.rs` constructs HTTPS requests, applies HTTP Basic authentication, configures TLS trust, proxies, client certificates, timeouts, retries, and request/response logging.
- `src/standard.rs` implements standard Redfish operations and vendor discovery.
- Vendor modules such as `src/dell.rs`, `src/hpe.rs`, `src/lenovo.rs`, and the NVIDIA platform modules implement vendor-specific operations.
- `src/model/` deserializes Redfish and OEM responses received from BMCs.

### Trust Boundaries

1. **Integrating application to libredfish:** The caller supplies BMC targets, credentials, firmware inputs, configuration values, and authorization decisions.
2. **libredfish to BMC:** Requests cross a management-network boundary over HTTPS, optionally through a configured proxy.
3. **BMC to managed host:** Redfish operations can alter host power, boot, firmware, accounts, BIOS, TPM, and Secure Boot state.
4. **BMC responses to caller:** BMC-provided JSON, headers, task state, inventory, and event data are parsed and returned to the integrating application.

The library handles sensitive material including BMC credentials, UEFI passwords, firmware images, certificates, inventory, and event-log data. It intentionally avoids deriving `Debug` for `Endpoint`, and its HTTP debug logging redacts known password fields, but callers must still treat logs and returned errors as potentially sensitive.

## Threat Model

The following threats are ordered by expected security impact and practical likelihood.

1. **BMC impersonation when TLS verification is disabled:** `RedfishClientPoolBuilder::danger_accept_invalid_certs` in `src/network.rs` allows callers to disable certificate validation. An attacker with access to the management network could impersonate a BMC, capture HTTP Basic credentials, or alter privileged Redfish operations.

2. **Sensitive data disclosure through logs and errors:** `src/network.rs` redacts known password fields in HTTP debug logs, but `RedfishError` variants in `src/error.rs` retain complete response bodies. Some vendor-specific paths, including Dell settings handling in `src/dell.rs`, log request bodies outside the central redaction path. Applications that record errors or enable verbose logging may expose credentials, configuration secrets, internal addresses, or BMC response data.

3. **Duplicate effects from automatic request retry:** `src/network.rs` retries a request once after certain non-timeout network failures. Because this mechanism can retry mutating POST, PATCH, or DELETE operations, an ambiguous first response may result in repeated power, account, configuration, or other privileged actions if the BMC does not provide effective idempotency.

4. **Untrusted firmware source selection:** `update_firmware_simple_update` in `src/standard.rs` forwards a caller-provided `ImageURI` to the BMC. An insufficiently validated URI may cause the BMC to retrieve firmware or other content from an unintended source reachable from the management network.

5. **Unauthorized use of privileged library operations:** The `Redfish` trait exposes high-impact operations but does not authenticate or authorize the application user invoking them. A vulnerable or incorrectly configured integrating service could allow unauthorized account changes, firmware updates, power operations, TPM clearing, BIOS changes, or security-control changes.

6. **Unverified firmware input at the library boundary:** Vendor firmware-update implementations open and upload caller-selected files. libredfish does not independently verify firmware signatures, hashes, provenance, or compatibility before upload; it relies on the caller and BMC to enforce those controls.

## Critical Security Assumptions

- The integrating application authenticates callers and authorizes every privileged Redfish operation. Possession of a libredfish client handle is treated as privileged.
- BMC usernames, passwords, UEFI passwords, client private keys, and related secrets are supplied and stored securely by the caller.
- Production deployments validate BMC certificates using trusted roots or explicitly configured certificate bundles. Disabling certificate validation is limited to environments with compensating network protections and an understood interception risk.
- The BMC management network is segmented and accessible only to authorized systems and administrators.
- Callers validate the target hostname or address before constructing an `Endpoint`; libredfish does not independently establish that the selected BMC belongs to the intended host.
- The BMC correctly enforces authentication, authorization, account policy, firmware compatibility, firmware signature validation, and protections for destructive operations.
- Firmware paths and `ImageURI` values come from trusted, approved sources and are validated before use.
- Callers treat error values, diagnostic output, and verbose logs as sensitive. Central HTTP redaction does not cover every vendor-specific logging or error path.
- Callers tolerate or guard against repeated effects for mutating operations that may be retried following an ambiguous network failure.
- Redfish and OEM responses are provided by a trusted BMC and are subject to caller-appropriate size, semantic, and policy validation before downstream use.

## Scope

This policy covers the libredfish source code and its handling of Redfish requests and responses.

BMC firmware, managed-host firmware, network segmentation, integrating applications, operator authorization, and deployment-specific secret storage are outside this repository's implementation scope. Vulnerabilities in those components may still affect libredfish deployments and should be reported through the appropriate private channel.

## Deployment and Dependency Security

- Prefer custom trusted root certificates over disabling TLS verification.
- Grant BMC credentials only the privileges required by the integrating workflow.
- Restrict debug and vendor-specific logging in production and protect collected logs.
- Validate firmware provenance and target compatibility before initiating updates.
- Keep libredfish and its Rust dependencies current, and review security advisories affecting the HTTP, TLS, serialization, and asynchronous runtime dependencies.
