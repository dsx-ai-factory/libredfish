/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */
use std::{borrow::Cow, collections::HashMap, path::Path, sync::OnceLock, time::Duration};

use regex::Regex;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, IF_MATCH},
    multipart::{Form, Part},
    Certificate, Client as HttpClient, ClientBuilder as HttpClientBuilder, Identity, Method, Proxy,
    StatusCode,
};
use serde::{de::DeserializeOwned, Serialize};
use tracing::{debug, Instrument};

use crate::model::service_root::RedfishVendor;
use crate::model::ComputerSystem;
use crate::{model::InvalidValueError, standard::RedfishStandard, Redfish, RedfishError};

pub const REDFISH_ENDPOINT: &str = "redfish/v1";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const MIN_UPLOAD_BANDWIDTH: u64 = 10_000;

pub struct RedfishClientPoolBuilder {
    connect_timeout: Duration,
    timeout: Duration,
    accept_invalid_certs: bool,
    proxy: Option<String>,
    identity: Option<ClientIdentityPem>,
    root_certificates: Vec<Vec<u8>>,
}

/// A PEM encoded client certificate chain and its private key.
#[derive(Clone)]
struct ClientIdentityPem {
    cert: Vec<u8>,
    key: Vec<u8>,
}

impl std::fmt::Debug for RedfishClientPoolBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedfishClientPoolBuilder")
            .field("connect_timeout", &self.connect_timeout)
            .field("timeout", &self.timeout)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("proxy", &self.proxy)
            .field("identity", &self.identity.as_ref().map(|_| "[REDACTED]"))
            .field("root_certificates", &self.root_certificates.len())
            .finish()
    }
}

impl RedfishClientPoolBuilder {
    /// Prevents the Redfish Client from accepting self signed certificates
    /// and other invalid certificates.
    ///
    /// By default self signed certificates will be accepted, since BMCs usually
    /// use those.
    pub fn danger_accept_invalid_certs(mut self) -> Self {
        self.accept_invalid_certs = true;
        self
    }

    /// Overwrites the timeout for establishing a connection
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Overwrites the timeout that will be applied to every request
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Presents a client certificate during the TLS handshake.
    ///
    /// Redfish endpoints themselves do not ask for one, but an authenticating
    /// proxy in front of them identifies its callers this way, so a pool aimed
    /// at such a proxy needs an identity.
    ///
    /// `cert_pem` is the PEM encoded certificate chain and `key_pem` its PEM
    /// encoded private key, in RSA, SEC1 elliptic curve, or PKCS#8 format.
    /// They are commonly two separate files (`tls.crt` and `tls.key`) and are
    /// accepted separately here so the caller does not have to join them.
    ///
    /// The PEM is not parsed until [`Self::build`], which reports a malformed
    /// certificate or key.
    pub fn identity(mut self, cert_pem: impl Into<Vec<u8>>, key_pem: impl Into<Vec<u8>>) -> Self {
        self.identity = Some(ClientIdentityPem {
            cert: cert_pem.into(),
            key: key_pem.into(),
        });
        self
    }

    /// Trusts the certificates in a PEM bundle for server verification, in
    /// addition to the roots the platform already trusts.
    ///
    /// May be called more than once; each bundle adds to the trusted set. The
    /// bundle is not parsed until [`Self::build`].
    pub fn add_root_certificates(mut self, pem_bundle: impl Into<Vec<u8>>) -> Self {
        self.root_certificates.push(pem_bundle.into());
        self
    }

    /// Builds a Redfish Client Network Configuration
    pub fn build(&self) -> Result<RedfishClientPool, RedfishError> {
        let mut builder = HttpClientBuilder::new();
        if let Some(proxy) = self.proxy.as_ref() {
            let p = Proxy::https(proxy)?;
            builder = builder.proxy(p);
        }

        if let Some(identity) = self.identity.as_ref() {
            builder = builder.identity(identity.to_reqwest_identity()?);
        }

        for pem_bundle in &self.root_certificates {
            // A bundle legitimately holds more than one certificate, and
            // reqwest takes them one at a time.
            let certificates = Certificate::from_pem_bundle(pem_bundle).map_err(|e| {
                RedfishError::GenericError {
                    error: format!("Failed to parse root certificate bundle: {}", e),
                }
            })?;
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }

        let http_client = builder
            .danger_accept_invalid_certs(self.accept_invalid_certs)
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .build()
            .map_err(|e| RedfishError::GenericError {
                error: format!("Failed to build RedfishClientPool HTTP client: {}", e),
            })?;
        let pool = RedfishClientPool { http_client };

        Ok(pool)
    }
}

impl ClientIdentityPem {
    fn to_reqwest_identity(&self) -> Result<Identity, RedfishError> {
        let mut pem = Vec::with_capacity(self.key.len() + self.cert.len() + 1);
        pem.extend_from_slice(&self.key);
        if !self.key.ends_with(b"\n") {
            pem.push(b'\n');
        }
        pem.extend_from_slice(&self.cert);

        Identity::from_pem(&pem).map_err(|e| RedfishError::GenericError {
            error: format!("Failed to parse client identity: {}", e),
        })
    }
}

/// The endpoint that the redfish client connects to
#[derive(Clone, PartialEq, Eq)] // WARN: Do not derive Debug: Endpoint may contain credentials and must not be logged accidentally.
pub struct Endpoint {
    /// Hostname or IP address of BMC
    pub host: String,
    /// BMC port. If absent the default HTTPS port 443 will be used
    pub port: Option<u16>,
    /// BMC username
    pub user: Option<String>,
    /// BMC password
    pub password: Option<String>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            host: "".to_string(),
            port: None,
            user: None,
            password: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RedfishClientPool {
    http_client: HttpClient,
}

impl RedfishClientPool {
    /// Returns Builder for configuring a Redfish HTTP connection pool
    pub fn builder() -> RedfishClientPoolBuilder {
        RedfishClientPoolBuilder {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            timeout: DEFAULT_TIMEOUT,
            // BMCs often have a self-signed cert, so usually this has to be true
            accept_invalid_certs: false,
            proxy: None,
            identity: None,
            root_certificates: Vec::new(),
        }
    }

    /// Creates a Redfish BMC client for a certain endpoint
    ///
    /// Creating the client will immediately start a HTTP requests
    /// to set system_id, manager_id and vendor type.
    pub async fn create_client(
        &self,
        endpoint: Endpoint,
    ) -> Result<Box<dyn crate::Redfish>, RedfishError> {
        self.create_client_with_custom_headers(endpoint, Vec::default())
            .await
    }

    /// Creates a Redfish BMC client for a certain endpoint,
    /// and adds custom headers to subsequent requests.
    ///
    /// Creating the client will immediately start HTTP requests
    /// to set system_id, manager_id, and vendor type (the vendor
    /// is auto-detected from the service root.
    ///
    /// `custom_headers` will be added to any headers used by vendor
    /// specific implementations or the http client.
    pub async fn create_client_with_custom_headers(
        &self,
        endpoint: Endpoint,
        custom_headers: Vec<(HeaderName, String)>,
    ) -> Result<Box<dyn crate::Redfish>, RedfishError> {
        self.create_client_impl(endpoint, None, custom_headers)
            .await
    }

    /// Creates a Redfish BMC client for a certain endpoint using
    /// the provided vendor instead of auto-detecting from the service
    /// root. This is needed for BMCs (e.g. Lite-On power shelves) whose
    /// service root does not expose vendor information, where we need
    /// a client that uses vendor-specific logic.
    pub async fn create_client_with_vendor(
        &self,
        endpoint: Endpoint,
        vendor: RedfishVendor,
        custom_headers: Vec<(HeaderName, String)>,
    ) -> Result<Box<dyn crate::Redfish>, RedfishError> {
        self.create_client_impl(endpoint, Some(vendor), custom_headers)
            .await
    }

    // Creates a complete "client" that takes the endpoint, an optional
    // vendor (which falls back to self-detection using the service root),
    // and an optional set of custom headers.
    //
    // If there's ever a need to expose this as pub, it's entirely
    // reasonable to do so (and rename it to something descriptive like
    // create_complete_client).
    async fn create_client_impl(
        &self,
        endpoint: Endpoint,
        vendor: Option<RedfishVendor>,
        custom_headers: Vec<(HeaderName, String)>,
    ) -> Result<Box<dyn crate::Redfish>, RedfishError> {
        let client = RedfishHttpClient::new(self.http_client.clone(), endpoint, custom_headers);
        let mut s = RedfishStandard::new(client);
        let service_root = s.get_service_root().await?;

        // Resolve the vendor up-front (explicit override, else from the service
        // root, which get_service_root backfills from the chassis manufacturer
        // for vendorless power shelves). Knowing the vendor here lets us skip
        // resource lookups for platforms that don't expose them.
        let vendor = match vendor {
            Some(v) => v,
            None => service_root.vendor().ok_or(RedfishError::MissingVendor)?,
        };

        let managers = s.get_managers().await?;
        let mut manager_id = managers
            .first()
            .ok_or_else(|| RedfishError::GenericError {
                error: "No managers found in service root".to_string(),
            })?
            .clone();
        let chassis = s.get_chassis_all().await?;

        // Delta power shelves expose no `/Systems` resource (a real query 404s)
        // and the Delta client never references a system id, so skip both the
        // lookup and the set entirely. For every other vendor, resolve the
        // system id and set it before set_vendor (DGX detection depends on it).
        if vendor != RedfishVendor::DeltaPowerShelf {
            let systems = s.get_systems().await?;
            // Prefer the canonical host system `System_0` when present. Some
            // platforms enumerate an auxiliary system (e.g. the NVIDIA
            // `HGX_Baseboard_0`) ahead of the real host, so picking the first
            // member blindly targets the wrong system (no BIOS/boot). Falling
            // back to the first member preserves behavior for every platform
            // that does not expose `System_0` (e.g. Viking's `DGX`).
            let preferred_system_id = systems
                .iter()
                .find(|id| *id == "System_0")
                .or_else(|| systems.first())
                .ok_or_else(|| RedfishError::GenericError {
                    error: "No systems found in service root".to_string(),
                })?;

            // Prefer a system that exposes a Bios resource, but probe the
            // preferred host id first. Auxiliary systems such as
            // `HGX_Baseboard_0` are often enumerated ahead of `System_0` and
            // also advertise a Bios link, so scanning Members order alone
            // selects the GPU baseboard (no host SecureBoot / BootOrder) and
            // discards the System_0 preference above. Treat fetch errors as
            // "no BIOS here": we already have `preferred_system_id` as a
            // fallback, and this also handles the test mockup, which drops
            // the connection instead of returning 404 when Bios is empty.
            let mut system_with_bios: Option<ComputerSystem> = None;
            for system_member in system_ids_for_bios_probe(preferred_system_id, &systems) {
                system_with_bios = s.if_system_has_bios(system_member).await;
                if system_with_bios.is_some() {
                    break;
                }
            }
            let manager_from_system = system_with_bios
                .as_ref()
                .and_then(|swb| swb.links.as_ref())
                .and_then(|links| links.managed_by.as_ref())
                .and_then(|mb| mb.first())
                .and_then(|d| d.odata_id.trim_matches('/').split('/').next_back())
                .map(|m| m.to_string());
            manager_id = manager_from_system.unwrap_or(manager_id);

            let system_id = system_with_bios
                .map(|swb| swb.id.to_owned())
                .unwrap_or(preferred_system_id.to_owned());

            // call set_system_id always before calling set_vendor
            s.set_system_id(&system_id)?;
        }

        s.set_manager_id(&manager_id)?;
        s.set_service_root(service_root.clone())?;

        // Resolve placeholder/ambiguous vendors that can only be settled from
        // fetched resources:
        // - P3809 is a placeholder — pick the GBx variant from chassis contents,
        //   whether it was auto-detected or explicitly provided.
        // - AMI is shared by Viking/DGX/GB300, distinguished by inspecting the
        //   selected system and manager or the host systems.
        let vendor = match vendor {
            RedfishVendor::P3809 => {
                if chassis.contains(&"MGX_NVSwitch_0".to_string()) {
                    RedfishVendor::NvidiaGBSwitch
                } else {
                    RedfishVendor::NvidiaGH200
                }
            }
            RedfishVendor::AMI => Self::refine_ami_vendor(&s).await?,
            other => other,
        };

        s.set_vendor(vendor).await
    }

    /// Keep known Viking systems as AMI; otherwise detect Lenovo GB300.
    async fn refine_ami_vendor(s: &RedfishStandard) -> Result<RedfishVendor, RedfishError> {
        if s.system_id() == "DGX" && s.manager_id() == "BMC" {
            return Ok(RedfishVendor::AMI);
        }

        let mut is_lenovo = false;
        let mut is_gb300 = false;
        for id in s.get_systems().await? {
            let (_, system): (_, ComputerSystem) = s.client.get(&format!("Systems/{id}")).await?;
            if system
                .manufacturer
                .as_deref()
                .unwrap_or_default()
                .contains("Lenovo")
            {
                is_lenovo = true;
            }
            if system
                .model
                .as_deref()
                .unwrap_or_default()
                .contains("GB300")
            {
                is_gb300 = true;
            }
            if is_lenovo && is_gb300 {
                return Ok(RedfishVendor::LenovoGB300);
            }
        }
        Ok(RedfishVendor::AMI)
    }

    /// Creates a Redfish BMC client for a certain endpoint
    ///
    /// Creating the standard client will not start any HTTP calls.
    pub fn create_standard_client(
        &self,
        endpoint: Endpoint,
    ) -> Result<Box<RedfishStandard>, RedfishError> {
        self.create_standard_client_with_custom_headers(endpoint, Vec::default())
    }

    /// Creates a Redfish BMC client for a certain endpoint, with custom headers injected into each request
    ///
    /// Creating the standard client will not start any HTTP calls.
    pub fn create_standard_client_with_custom_headers(
        &self,
        endpoint: Endpoint,
        custom_headers: Vec<(HeaderName, String)>,
    ) -> Result<Box<RedfishStandard>, RedfishError> {
        let client = RedfishHttpClient::new(self.http_client.clone(), endpoint, custom_headers);
        let s = RedfishStandard::new(client);
        Ok(Box::new(s))
    }
}

/// Applies `custom_headers` to a request, failing on a value that is not a
/// valid HTTP header value.
fn apply_custom_headers(
    mut req_b: reqwest::RequestBuilder,
    custom_headers: &[(HeaderName, String)],
    url: &str,
) -> Result<reqwest::RequestBuilder, RedfishError> {
    for (key, val) in custom_headers.iter() {
        let value = match HeaderValue::from_str(val) {
            Ok(x) => x,
            Err(e) => {
                return Err(RedfishError::InvalidValue {
                    url: url.to_string(),
                    field: "0".to_string(),
                    err: InvalidValueError(format!(
                        "Invalid custom header {} value: {}, error: {}",
                        key, val, e
                    )),
                });
            }
        };
        req_b = req_b.header(key, value);
    }
    Ok(req_b)
}

/// A HTTP client which targets a single libredfish endpoint
#[derive(Clone)]
pub struct RedfishHttpClient {
    endpoint: Endpoint,
    http_client: HttpClient,
    custom_headers: Vec<(HeaderName, String)>,
}

impl RedfishHttpClient {
    pub fn new(
        http_client: HttpClient,
        endpoint: Endpoint,
        custom_headers: Vec<(HeaderName, String)>,
    ) -> Self {
        Self {
            endpoint,
            http_client,
            custom_headers,
        }
    }

    /// Returns the hostname or IP address of the BMC this client connects to.
    pub fn host(&self) -> &str {
        &self.endpoint.host
    }

    /// Returns `true` if this client has no credentials (i.e. anonymous/unauthenticated).
    pub fn is_anonymous(&self) -> bool {
        self.endpoint.user.is_none()
    }

    pub(crate) async fn get_anonymous<T>(&self, api: &str) -> Result<(StatusCode, T), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        let mut client = self.clone();
        client.endpoint.user = None;
        client.endpoint.password = None;
        client.get(api).await
    }

    pub async fn get<T>(&self, api: &str) -> Result<(StatusCode, T), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        self.get_with_timeout(api, None).await
    }
    pub async fn get_with_timeout<T>(
        &self,
        api: &str,
        timeout: Option<Duration>,
    ) -> Result<(StatusCode, T), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        let (status_code, resp_opt, _resp_headers) = self
            .req::<T, String>(Method::GET, api, None, timeout, None, Vec::new())
            .await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(RedfishError::NoContent),
        }
    }
    pub async fn post<B>(
        &self,
        api: &str,
        data: B,
    ) -> Result<(StatusCode, Option<HeaderMap>), RedfishError>
    where
        B: Serialize + ::std::fmt::Debug,
    {
        self.post_with_headers(api, data, None).await
    }

    pub async fn post_with_headers<B>(
        &self,
        api: &str,
        data: B,
        headers: Option<Vec<(HeaderName, String)>>,
    ) -> Result<(StatusCode, Option<HeaderMap>), RedfishError>
    where
        B: Serialize + ::std::fmt::Debug,
    {
        let (status_code, _resp_body, resp_headers): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .req(
                Method::POST,
                api,
                Some(data),
                None,
                None,
                headers.unwrap_or_default(),
            )
            .await?;
        Ok((status_code, resp_headers))
    }

    pub async fn post_file<T>(
        &self,
        api: &str,
        file: tokio::fs::File,
    ) -> Result<(StatusCode, T), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        let body_option: Option<HashMap<&str, String>> = None;
        let timeout = DEFAULT_TIMEOUT
            + file.metadata().await.map_or_else(
                |_err| DEFAULT_TIMEOUT,
                |m| Duration::from_secs(m.len() / MIN_UPLOAD_BANDWIDTH),
            );
        let (status_code, resp_opt, _resp_headers) = self
            .req::<T, _>(
                Method::POST,
                api,
                body_option,
                Some(timeout),
                Some(file),
                Vec::new(),
            )
            .await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(RedfishError::NoContent),
        }
    }

    pub async fn patch<T>(
        &self,
        api: &str,
        data: T,
    ) -> Result<(StatusCode, Option<HeaderMap>), RedfishError>
    where
        T: Serialize + ::std::fmt::Debug,
    {
        let (status_code, _resp_body, resp_headers): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .req(Method::PATCH, api, Some(data), None, None, Vec::new())
            .await?;
        Ok((status_code, resp_headers))
    }

    pub async fn patch_with_if_match<B>(&self, api: &str, data: B) -> Result<(), RedfishError>
    where
        B: Serialize + ::std::fmt::Debug,
    {
        let timeout = Duration::from_secs(60);
        let headers: Vec<(HeaderName, String)> = vec![(IF_MATCH, "*".to_string())];
        let (status_code, resp_body, _): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .req(Method::PATCH, api, Some(data), Some(timeout), None, headers)
            .await?;
        match status_code {
            StatusCode::NO_CONTENT => Ok(()),
            _ => Err(RedfishError::HTTPErrorCode {
                url: api.to_string(),
                status_code,
                response_body: format!("{:?}", resp_body.unwrap_or_default()),
            }),
        }
    }

    pub async fn delete(&self, api: &str) -> Result<StatusCode, RedfishError> {
        let (status_code, _resp_body, _resp_headers): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .req::<_, String>(Method::DELETE, api, None, None, None, Vec::new())
            .await?;
        Ok(status_code)
    }

    // All the HTTP requests happen from here.
    pub async fn req<T, B>(
        &self,
        method: Method,
        api: &str,
        body: Option<B>,
        override_timeout: Option<Duration>,
        file: Option<tokio::fs::File>,
        mut custom_headers: Vec<(HeaderName, String)>,
    ) -> Result<(StatusCode, Option<T>, Option<HeaderMap>), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        custom_headers.extend_from_slice(&self.custom_headers);

        let is_file = file.is_some();

        // Create a span with explicitly NO parent to isolate HTTP operations.
        // This prevents hyper-util's background tasks from capturing our caller's spans.
        // See: hyper-util's TokioExecutor uses .in_current_span() when tracing feature is enabled,
        // which causes span "bouncing" between tasks and delayed span closure.
        let isolated_span = tracing::trace_span!(parent: None, "http_isolated");

        async {
            match self
                ._req(&method, api, &body, override_timeout, file, &custom_headers)
                .await
            {
                Ok(x) => Ok(x),
                // post_file failure must be done manually. The seek is moved and we
                // can't reuse file by cloning. Clone shares read, writes and seek.
                Err(err) if is_file => Err(err),
                // Avoid doubling of timeouts. It is specifically important if caller relies on
                // timing of this call.
                Err(RedfishError::NetworkError { source, url }) => {
                    if source.is_timeout() {
                        Err(RedfishError::NetworkError { source, url })
                    } else {
                        // HPE sends RST in case same connection is reused. To avoid that let's retry.
                        self._req(&method, api, &body, override_timeout, None, &custom_headers)
                            .await
                    }
                }
                Err(err) => Err(err),
            }
        }
        .instrument(isolated_span)
        .await
    }

    // All the HTTP requests happen from here.
    #[tracing::instrument(name = "libredfish::request", skip_all, fields(uri=api), level = tracing::Level::DEBUG)]
    async fn _req<T, B>(
        &self,
        method: &Method,
        api: &str,
        body: &Option<B>,
        override_timeout: Option<Duration>,
        file: Option<tokio::fs::File>,
        custom_headers: &[(HeaderName, String)],
    ) -> Result<(StatusCode, Option<T>, Option<HeaderMap>), RedfishError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        let url = match self.endpoint.port {
            Some(p) => format!(
                "https://{}:{}/{}/{}",
                self.endpoint.host, p, REDFISH_ENDPOINT, api
            ),
            None => format!(
                "https://{}/{}/{}",
                self.endpoint.host, REDFISH_ENDPOINT, api
            ),
        };
        let body_enc = match body {
            Some(b) => {
                let url: String = url.clone();
                let body_enc =
                    serde_json::to_string(b).map_err(|e| RedfishError::JsonSerializeError {
                        url,
                        object_debug: redact_sensitive_fields(&format!("{b:?}")).into_owned(),
                        source: e,
                    })?;

                Some(body_enc)
            }
            None => None,
        };
        debug!(
            "TX {} {} {}",
            method,
            url,
            RedactPasswords(body_enc.as_deref().unwrap_or_default())
        );
        let mut req_b = match *method {
            Method::GET => self.http_client.get(&url),
            Method::POST => self.http_client.post(&url),
            Method::PATCH => self.http_client.patch(&url),
            Method::DELETE => self.http_client.delete(&url),
            _ => unreachable!("Only GET, POST, PATCH and DELETE http methods are used."),
        };
        req_b = req_b.header(ACCEPT, HeaderValue::from_static("application/json"));

        if file.is_some() {
            req_b = req_b.header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
        } else {
            req_b = req_b.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }

        req_b = apply_custom_headers(req_b, custom_headers, &url)?;

        if let Some(user) = &self.endpoint.user {
            req_b = req_b.basic_auth(user, self.endpoint.password.as_ref());
        }
        if let Some(t) = override_timeout {
            req_b = req_b.timeout(t);
        }
        if let Some(b) = body_enc {
            req_b = req_b.body(b);
        }
        if let Some(f) = file {
            req_b = req_b.body(f);
        }
        let response = req_b.send().await.map_err(|e| RedfishError::NetworkError {
            url: url.clone(),
            source: e,
        })?;

        let status_code = response.status();
        if status_code == StatusCode::CONFLICT {
            // 409 No Content is how Dell responds if we try to turn off a system that's already off, etc.
            // Note that Lenovo accepts these unnecessary operations and returns '204 No Content'.
            return Err(RedfishError::UnnecessaryOperation);
        }
        debug!("RX {status_code}");

        let mut res_headers = None;
        if !response.headers().is_empty() {
            res_headers = Some(response.headers().clone());
        }

        // read the body even if not status 2XX, because BMCs give useful error messages as JSON
        let response_body = response
            .text()
            .await
            .map_err(|e| RedfishError::NetworkError {
                url: url.clone(),
                source: e,
            })?;
        debug!(
            "RX {status_code} {}",
            truncate(&redact_sensitive_fields(&response_body), 1500)
        );

        if !status_code.is_success() {
            if status_code == StatusCode::FORBIDDEN && !response_body.is_empty() {
                // If PasswordChangeRequired is in the response, return a PasswordChangeRequired error.
                if let Ok(err) = serde_json::from_str::<crate::model::error::Error>(&response_body)
                {
                    if let Some(password_change_required) = err
                        .error
                        .extended
                        .iter()
                        // TODO(ajf) The actual message ID is specified in DTMF RedFish 9.5.11.2 so we
                        // should properly parse it into a type since the error may come from different
                        // MessageRegistries
                        .find(|ext| ext.message_id.ends_with("PasswordChangeRequired"))
                    {
                        return Err(RedfishError::PasswordChangeRequired {
                            account_uri: password_change_required.message_args.first().cloned(),
                        });
                    }
                }
                // If we can't decode the error JSON, just return the normal HTTPErrorCode. Some
                // misbehaved BMCs will return an XHTML document for forbidden responses, for
                // instance.
            }
            return Err(RedfishError::HTTPErrorCode {
                url,
                status_code,
                response_body,
            });
        }

        let mut res = None;
        if !response_body.is_empty() {
            match serde_json::from_str(&response_body) {
                Ok(v) => res.insert(v),
                Err(e) => {
                    return Err(RedfishError::JsonDeserializeError {
                        url,
                        body: response_body,
                        source: e,
                    });
                }
            };
        }

        Ok((status_code, res, res_headers))
    }

    // req_multipart_firmware_upload does a Redfish request for a multipart based firmware upload.
    pub async fn req_update_firmware_multipart(
        &self,
        filename: &Path,
        file: tokio::fs::File,
        parameters: String,
        api: &str,
        drop_redfish_url_part: bool,
        timeout: Duration,
    ) -> Result<(StatusCode, Option<String>, String), RedfishError> {
        self.req_update_firmware_multipart_with_oem(
            filename,
            file,
            parameters,
            None,
            api,
            drop_redfish_url_part,
            timeout,
        )
        .await
    }

    // req_update_firmware_multipart_with_oem is like req_update_firmware_multipart, but allows
    // sending an additional OEM-specific `OemParameters` JSON part. AMI MegaRAC BMCs require this
    // third part; vendors that do not use it (e.g. Lenovo XCC) pass `None`.
    #[allow(clippy::too_many_arguments)]
    pub async fn req_update_firmware_multipart_with_oem(
        &self,
        filename: &Path,
        file: tokio::fs::File,
        parameters: String,
        oem_parameters: Option<String>,
        api: &str,
        drop_redfish_url_part: bool,
        timeout: Duration,
    ) -> Result<(StatusCode, Option<String>, String), RedfishError> {
        let user = match &self.endpoint.user {
            Some(user) => user,
            None => return Err(RedfishError::NotSupported("User not specified".to_string())),
        };

        let basename = match Path::file_name(filename) {
            Some(x) => x.to_string_lossy().to_string(),
            None => {
                return Err(RedfishError::FileError("Bad filename".to_string()));
            }
        };

        // Some vendors, but not all, have a prefix at the start of the given endpoint.
        let api_str = api.to_string();
        let api = api_str.strip_prefix("/").unwrap_or(api);
        // Some (Lenovo, perhaps others) vendors have nonstandard endpoint names for multipart upload.
        let with_redfish_endpoint = if drop_redfish_url_part {
            api.to_string()
        } else {
            format!("{}/{}", REDFISH_ENDPOINT, api)
        };
        let url = match self.endpoint.port {
            Some(p) => format!(
                "https://{}:{}/{}",
                self.endpoint.host, p, with_redfish_endpoint
            ),
            None => format!("https://{}/{}", self.endpoint.host, with_redfish_endpoint),
        };

        let length = filename
            .metadata()
            .map_err(|e| RedfishError::FileError(e.to_string()))?
            .len();
        // The spec is for two parts to the form: UpdateParameters, which is JSON encoded metadata,
        // and UpdateFile, which is the file itself.  Exact details of UpdateParameters end up being implementation specific.
        let mut form = Form::new()
            .part(
                "UpdateParameters",
                reqwest::multipart::Part::text(parameters)
                    // mime_str_to_part parses the MIME type. Technically this is
                    // infallible for known MIME types, including application/json,
                    // but still check for an error instead of unwrapping.
                    .mime_str("application/json")
                    .map_err(|e| RedfishError::GenericError {
                        error: format!("Invalid MIME type 'application/json': {}", e),
                    })?,
            )
            .part(
                "UpdateFile",
                Part::stream_with_length(file, length)
                    // mime_str_to_part parses the MIME type. Technically this is
                    // infallible for known MIME types, including application/octet-stream,
                    // but still check for an error instead of unwrapping.
                    .mime_str("application/octet-stream")
                    .map_err(|e| RedfishError::GenericError {
                        error: format!("Invalid MIME type 'application/octet-stream': {}", e),
                    })?
                    // Yes, the filename passed does matter for some reason, at least for Dells, and it has to be the basename.
                    .file_name(basename.clone()),
            );

        // AMI MegaRAC BMCs expect a third `OemParameters` JSON part alongside UpdateParameters
        // and UpdateFile. Other vendors omit it.
        if let Some(oem_parameters) = oem_parameters {
            form = form.part(
                "OemParameters",
                reqwest::multipart::Part::text(oem_parameters)
                    .mime_str("application/json")
                    .map_err(|e| RedfishError::GenericError {
                        error: format!("Invalid MIME type 'application/json': {}", e),
                    })?,
            );
        }

        let req_b = self
            .http_client
            .post(url.clone())
            .timeout(timeout)
            .multipart(form);
        let response = apply_custom_headers(req_b, &self.custom_headers, &url)?
            .basic_auth(user, self.endpoint.password.as_ref())
            .send()
            .await
            .map_err(|e| RedfishError::NetworkError {
                url: url.to_string(),
                source: e,
            })?;

        let status_code = response.status();
        debug!("RX {status_code}");

        // Some (or all?) implementations will return the task ID in the Location header, with an empty body.
        let loc = response
            .headers()
            .get("Location")
            .map(|x| x.to_str().unwrap_or_default().to_string());

        // read the body even if not status 2XX, because BMCs give useful error messages as JSON
        let response_body = response
            .text()
            .await
            .map_err(|e| RedfishError::NetworkError {
                url: url.to_string(),
                source: e,
            })?;
        debug!(
            "RX {status_code} {}",
            truncate(&redact_sensitive_fields(&response_body), 1500)
        );

        if !status_code.is_success() {
            return Err(RedfishError::HTTPErrorCode {
                url: url.to_string(),
                status_code,
                response_body,
            });
        }

        Ok((status_code, loc, response_body))
    }
}

/// Order system ids for the Bios probe: preferred host first, then the remaining
/// members in enumeration order (skipping the preferred id so it is not probed twice).
fn system_ids_for_bios_probe<'a>(
    preferred: &'a str,
    systems: &'a [String],
) -> impl Iterator<Item = &'a str> {
    std::iter::once(preferred).chain(
        systems
            .iter()
            .map(String::as_str)
            .filter(move |id| *id != preferred),
    )
}

fn truncate(s: &str, len: usize) -> &str {
    &s[..len.min(s.len())]
}

/// Redacts known sensitive JSON fields for safe logging.
///
/// Operates directly on the serialised JSON string to avoid re-serialisation
/// cost.  Returns `Cow::Borrowed(body)` unchanged when no sensitive field
/// names are present (zero-copy fast path).  The actual bytes sent over the
/// wire are **never** modified — only the string passed to this function is
/// affected.
///
/// Redacted fields (exact, case-sensitive JSON key match):
///   `Password`, `OldPassword`, `NewPassword`   — standard Redfish account/BIOS ops
///   `CurrentUefiPassword`, `UefiPassword`       — NVIDIA DPU Bios/Settings PATCH
///   `ImportBuffer`                              — Dell ImportSystemConfiguration XML blob
///
/// A `Display` wrapper that redacts sensitive JSON fields on formatting.
///
/// Passing this to `tracing::debug!` defers evaluation until the macro decides the
/// message will actually be emitted, so the regex never runs at non-debug log levels.
struct RedactPasswords<'a>(&'a str);

impl std::fmt::Display for RedactPasswords<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        redact_sensitive_fields(self.0).fmt(f)
    }
}

fn redact_sensitive_fields(body: &str) -> Cow<'_, str> {
    // Fast path: skip regex engine entirely when no sensitive key is present.
    // "Password" covers all five password-style keys; "ImportBuffer" covers the
    // Dell XML-in-JSON fallback path.
    if !body.contains("Password") && !body.contains("ImportBuffer") {
        return Cow::Borrowed(body);
    }

    static REDACT_RE: OnceLock<Regex> = OnceLock::new();
    let re = REDACT_RE.get_or_init(|| {
        // Matches a JSON key from the sensitive list followed by its quoted string value
        // (including JSON escape sequences).  The key is captured in group 1 so it can
        // be preserved verbatim in the replacement.
        Regex::new(
            r#""(Password|OldPassword|NewPassword|CurrentUefiPassword|UefiPassword|ImportBuffer)"\s*:\s*"(?:[^"\\]|\\.)*""#,
        )
        .expect("hardcoded redaction regex must be valid")
    });

    re.replace_all(body, r#""$1":"[REDACTED]""#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("", 1500), "");

        let big = "a".repeat(2000);
        assert_eq!(truncate(&big, 1500).len(), 1500);
    }

    #[test]
    fn redact_password_field() {
        let body = r#"{"UserName":"admin","Password":"s3cr3t!"}"#;
        let redacted = redact_sensitive_fields(body);
        assert!(
            !redacted.contains("s3cr3t!"),
            "plaintext password must not appear in log output"
        );
        assert!(redacted.contains("[REDACTED]"));
        assert!(
            redacted.contains("UserName"),
            "non-sensitive fields must be preserved"
        );
    }

    #[test]
    fn redact_old_and_new_password_fields() {
        let body = r#"{"PasswordName":"AdministratorPassword","OldPassword":"old123","NewPassword":"new456"}"#;
        let redacted = redact_sensitive_fields(body);
        assert!(
            !redacted.contains("old123"),
            "OldPassword value must be redacted"
        );
        assert!(
            !redacted.contains("new456"),
            "NewPassword value must be redacted"
        );
        // PasswordName is a slot name, not a secret — must NOT be redacted.
        assert!(
            redacted.contains("AdministratorPassword"),
            "PasswordName value must not be redacted"
        );
    }

    #[test]
    fn nvidia_dpu_uefi_password_fields_are_redacted() {
        let body =
            r#"{"Attributes":{"CurrentUefiPassword":"old_secret","UefiPassword":"new_secret"}}"#;
        let redacted = redact_sensitive_fields(body);
        assert!(
            !redacted.contains("old_secret"),
            "CurrentUefiPassword value must be redacted"
        );
        assert!(
            !redacted.contains("new_secret"),
            "UefiPassword value must be redacted"
        );
        assert!(
            redacted.contains("CurrentUefiPassword"),
            "key name must be preserved"
        );
    }

    #[test]
    fn dell_import_buffer_xml_blob_is_redacted() {
        let xml = r#"<SystemConfiguration><Component FQDD="BIOS.Setup.1-1"><Attribute Name="OldSetupPassword">my_uefi_pass</Attribute><Attribute Name="NewSetupPassword"></Attribute></Component></SystemConfiguration>"#;
        let body = format!(
            r#"{{"ShutdownType":"Forced","ShareParameters":{{"Target":"BIOS"}},"ImportBuffer":"{}"}}"#,
            xml.replace('"', "\\\"")
        );
        let redacted = redact_sensitive_fields(&body);
        assert!(
            !redacted.contains("my_uefi_pass"),
            "UEFI password in ImportBuffer XML must not appear in log output"
        );
        assert!(redacted.contains("[REDACTED]"));
        assert!(
            redacted.contains("ShutdownType"),
            "non-sensitive fields must be preserved"
        );
    }

    #[test]
    fn non_sensitive_body_is_returned_borrowed() {
        let body = r#"{"ResetType":"GracefulRestart"}"#;
        match redact_sensitive_fields(body) {
            Cow::Borrowed(s) => assert_eq!(s, body),
            Cow::Owned(_) => panic!("non-sensitive body must take the zero-copy fast path"),
        }
    }

    #[test]
    fn empty_body_fast_path() {
        match redact_sensitive_fields("") {
            Cow::Borrowed(s) => assert_eq!(s, ""),
            Cow::Owned(_) => panic!("empty string must take fast path"),
        }
    }

    #[test]
    fn wire_payload_is_unaffected() {
        let body_enc = r#"{"UserName":"newuser","Password":"myP@ssw0rd"}"#.to_string();
        let _log_safe = redact_sensitive_fields(&body_enc);
        assert_eq!(
            body_enc, r#"{"UserName":"newuser","Password":"myP@ssw0rd"}"#,
            "wire payload must never be modified"
        );
    }

    #[test]
    fn escaped_characters_in_password_are_redacted() {
        let body = r#"{"Password":"p@ss\"w\\ord"}"#;
        let redacted = redact_sensitive_fields(body);
        assert!(
            !redacted.contains("p@ss"),
            "escaped password value must be redacted"
        );
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn truncation_after_redaction_does_not_leak_partial_secret() {
        let filler = "x".repeat(1490);
        let secret = "supersecret_password_value";
        let body = format!(r#"{{"Data":"{}","Password":"{}"}}"#, filler, secret);
        assert!(
            body.len() > 1500,
            "body must exceed truncation limit for this test to be valid"
        );

        let redacted = redact_sensitive_fields(&body);
        let logged = truncate(&redacted, 1500);
        assert!(
            !logged.contains("supersecret"),
            "no part of the secret must appear after truncation"
        );
    }

    #[test]
    fn bios_probe_order_prefers_system_0_ahead_of_hgx_baseboard() {
        let systems = vec!["HGX_Baseboard_0".to_string(), "System_0".to_string()];
        let order: Vec<&str> = system_ids_for_bios_probe("System_0", &systems).collect();
        assert_eq!(order, vec!["System_0", "HGX_Baseboard_0"]);
    }

    #[test]
    fn bios_probe_order_keeps_preferred_first_when_already_first() {
        let systems = vec!["System_0".to_string(), "HGX_Baseboard_0".to_string()];
        let order: Vec<&str> = system_ids_for_bios_probe("System_0", &systems).collect();
        assert_eq!(order, vec!["System_0", "HGX_Baseboard_0"]);
    }

    #[test]
    fn bios_probe_order_falls_back_to_first_member_when_no_system_0() {
        let systems = vec!["DGX".to_string(), "HGX_Baseboard_0".to_string()];
        let preferred = systems.first().map(String::as_str).unwrap();
        let order: Vec<&str> = system_ids_for_bios_probe(preferred, &systems).collect();
        assert_eq!(order, vec!["DGX", "HGX_Baseboard_0"]);
    }

    const TEST_CERT_PEM: &[u8] = include_bytes!("../tests/cert.pem");
    const TEST_KEY_PEM: &[u8] = include_bytes!("../tests/key.pem");

    #[test]
    fn custom_headers_are_applied_and_invalid_values_rejected() {
        let client = HttpClient::new();
        let headers = vec![(
            HeaderName::from_static("forwarded"),
            "host=192.0.2.10".to_string(),
        )];

        let request = apply_custom_headers(
            client.post("https://bmc.invalid/redfish/v1/UpdateService"),
            &headers,
            "https://bmc.invalid/redfish/v1/UpdateService",
        )
        .expect("valid header value applies")
        .build()
        .expect("request builds");
        assert_eq!(
            request.headers().get("forwarded").unwrap(),
            "host=192.0.2.10"
        );

        let invalid = vec![(
            HeaderName::from_static("forwarded"),
            "host=bad\nvalue".to_string(),
        )];
        let err = apply_custom_headers(
            client.post("https://bmc.invalid/x"),
            &invalid,
            "https://bmc.invalid/x",
        )
        .expect_err("a header value with a control character must be rejected");
        assert!(
            err.to_string().contains("Invalid custom header"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn builds_with_a_client_identity() {
        RedfishClientPool::builder()
            .identity(TEST_CERT_PEM, TEST_KEY_PEM)
            .build()
            .expect("a matching certificate and key should build a client identity");
    }

    // The two files are supplied separately and joined internally, so a cert
    // that does not end in a newline must not run into the key's PEM header.
    #[test]
    fn builds_with_a_client_identity_lacking_a_trailing_newline() {
        let mut key = TEST_KEY_PEM.to_vec();
        while key.last() == Some(&b'\n') {
            key.pop();
        }

        RedfishClientPool::builder()
            .identity(TEST_CERT_PEM, key)
            .build()
            .expect("a key without a trailing newline should still build");
    }

    #[test]
    fn rejects_a_malformed_client_identity() {
        let err = RedfishClientPool::builder()
            .identity(b"not a certificate".to_vec(), b"not a key".to_vec())
            .build()
            .expect_err("malformed PEM should fail the build");

        let message = err.to_string();
        assert!(
            message.contains("Failed to parse client identity"),
            "unexpected error: {message}"
        );
        assert!(
            !message.contains("not a key"),
            "the error must not quote the key material: {message}"
        );
    }

    #[test]
    fn builds_with_added_root_certificates() {
        RedfishClientPool::builder()
            .add_root_certificates(TEST_CERT_PEM)
            .build()
            .expect("a PEM certificate should be accepted as a root");
    }

    #[test]
    fn rejects_a_malformed_root_certificate_bundle() {
        let err = RedfishClientPool::builder()
            .add_root_certificates(b"-----BEGIN CERTIFICATE-----\nnope\n".to_vec())
            .build()
            .expect_err("malformed PEM should fail the build");

        assert!(
            err.to_string()
                .contains("Failed to parse root certificate bundle"),
            "unexpected error: {err}"
        );
    }

    // The builder carries a private key, so its Debug must not print it.
    #[test]
    fn debug_redacts_the_client_identity() {
        let rendered = format!(
            "{:?}",
            RedfishClientPool::builder().identity(TEST_CERT_PEM, TEST_KEY_PEM)
        );

        assert!(rendered.contains("[REDACTED]"), "unexpected: {rendered}");
        assert!(
            !rendered.contains("PRIVATE KEY"),
            "the key must not be rendered: {rendered}"
        );
    }
}
