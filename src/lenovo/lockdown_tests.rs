// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::{rustls, TlsAcceptor};

use super::*;
use crate::network::{Endpoint, RedfishClientPool};

const SECURITY: &str = "Managers/1/Oem/Lenovo/Security";
const BIOS: &str = "Systems/1/Bios";

#[derive(Clone)]
struct Request {
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    status: u16,
    response: Value,
}

fn get(path: &'static str, response: Value) -> Request {
    Request {
        method: "GET",
        path,
        body: None,
        status: 200,
        response,
    }
}

fn patch(path: &'static str, body: Value) -> Request {
    Request {
        method: "PATCH",
        path,
        body: Some(body),
        status: 204,
        response: Value::Null,
    }
}

// A scripted TLS peer verifies the real Redfish client's HTTP paths, payloads,
// ordering, and error propagation, without accepting writes to unknown controls.
struct Server(JoinHandle<()>);

impl Server {
    async fn finish(mut self) {
        tokio::time::timeout(Duration::from_secs(5), &mut self.0)
            .await
            .expect("all expected requests must arrive")
            .expect("HTTP script must pass");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn client(script: Vec<Request>) -> (Bmc, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cert = CertificateDer::from_pem_slice(include_bytes!("../../tests/cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("../../tests/key.pem")).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let tls = TlsAcceptor::from(Arc::new(config));
    let task = tokio::spawn(async move {
        for expected in script {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tls.accept(stream).await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0, "request ended before headers");
                request.extend_from_slice(&buffer[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let header = String::from_utf8_lossy(&request[..header_end]);
            let first = header.lines().next().unwrap();
            assert_eq!(
                first,
                format!("{} /redfish/v1/{} HTTP/1.1", expected.method, expected.path)
            );
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < header_end + length {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0, "request body truncated");
                request.extend_from_slice(&buffer[..n]);
            }
            if let Some(body) = expected.body {
                assert_eq!(
                    serde_json::from_slice::<Value>(&request[header_end..header_end + length])
                        .unwrap(),
                    body
                );
            }
            let response = if expected.status == 204 {
                String::new()
            } else {
                expected.response.to_string()
            };
            let headers = format!("HTTP/1.1 {} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", expected.status, response.len());
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let pool = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let mut standard = pool
        .create_standard_client(Endpoint {
            host: "127.0.0.1".into(),
            port: Some(port),
            ..Default::default()
        })
        .unwrap();
    standard.set_manager_id("1").unwrap();
    standard.set_system_id("1").unwrap();
    (Bmc::new(*standard).unwrap(), Server(task))
}

fn discovery(firmware: Value, bios: Value) -> Vec<Request> {
    vec![get(SECURITY, firmware), get(BIOS, bios)]
}

#[tokio::test]
async fn optional_controls_are_discovered_independently_and_writes_are_idempotent() {
    for (security, bios, writes) in [
        (json!({}), json!({}), vec![]),
        (
            json!({"FWLockdown":false}),
            json!({}),
            vec![patch(SECURITY, json!({"FWLockdown":true}))],
        ),
        (
            json!({}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":false}}}),
            vec![patch(
                BIOS,
                json!({"Oem":{"Lenovo":{"SettingsLockdown":true}}}),
            )],
        ),
        (
            json!({}),
            json!({"Oem":{"Lenovo":{"UEFIVariablesRO":false}}}),
            vec![patch(
                BIOS,
                json!({"Oem":{"Lenovo":{"UEFIVariablesRO":true}}}),
            )],
        ),
        (
            json!({"FWLockdown":true}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":true,"UEFIVariablesRO":true}}}),
            vec![],
        ),
    ] {
        let mut script = discovery(security, bios);
        script.extend(writes);
        let (bmc, server) = client(script).await;
        bmc.lockdown_bmc(EnabledDisabled::Enabled).await.unwrap();
        server.finish().await;
    }
}

#[tokio::test]
async fn unlock_firmware_before_bios_and_lock_firmware_last() {
    for locked in [false, true] {
        let mut script = discovery(
            json!({"FWLockdown":!locked}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":!locked,"UEFIVariablesRO":!locked}}}),
        );
        let fw = patch(SECURITY, json!({"FWLockdown":locked}));
        let bios = patch(
            BIOS,
            json!({"Oem":{"Lenovo":{"SettingsLockdown":locked,"UEFIVariablesRO":locked}}}),
        );
        script.extend(if locked {
            vec![bios, fw]
        } else {
            vec![fw, bios]
        });
        let (bmc, server) = client(script).await;
        bmc.lockdown_bmc(if locked {
            EnabledDisabled::Enabled
        } else {
            EnabledDisabled::Disabled
        })
        .await
        .unwrap();
        server.finish().await;
    }
}

#[tokio::test]
async fn firmware_upgrade_adds_capabilities_to_the_same_client() {
    let mut script = discovery(json!({}), json!({}));
    script.extend(discovery(json!({"FWLockdown":false}), json!({})));
    script.push(patch(SECURITY, json!({"FWLockdown":true})));
    let (bmc, server) = client(script).await;
    bmc.lockdown_bmc(EnabledDisabled::Enabled).await.unwrap();
    bmc.lockdown_bmc(EnabledDisabled::Enabled).await.unwrap();
    server.finish().await;
}

#[tokio::test]
async fn malformed_properties_and_failed_reads_are_not_unsupported() {
    for status in [401, 403, 500] {
        let mut response = get(SECURITY, json!({}));
        response.status = status;
        let (bmc, server) = client(vec![response]).await;
        assert!(bmc.lockdown_bmc(EnabledDisabled::Enabled).await.is_err());
        server.finish().await;
    }
    for (security, bios) in [
        (json!({"FWLockdown":null}), json!({})),
        (json!({"FWLockdown":"false"}), json!({})),
        (json!({}), json!({"Oem":null})),
        (json!({}), json!({"Oem":{"Lenovo":false}})),
        (json!({}), json!({"Oem":{"Lenovo":{"SettingsLockdown":0}}})),
        (
            json!({}),
            json!({"Oem":{"Lenovo":{"UEFIVariablesRO":"true"}}}),
        ),
    ] {
        let (bmc, server) = client(discovery(security, bios)).await;
        assert!(bmc.lockdown_bmc(EnabledDisabled::Enabled).await.is_err());
        server.finish().await;
    }
}

#[tokio::test]
async fn failed_patch_is_reported_and_retries_re_read_state() {
    let mut script = discovery(json!({"FWLockdown":false}), json!({}));
    let mut write = patch(SECURITY, json!({"FWLockdown":true}));
    write.status = 400;
    script.push(write);
    script.extend(discovery(json!({"FWLockdown":false}), json!({})));
    script.push(patch(SECURITY, json!({"FWLockdown":true})));
    let (bmc, server) = client(script).await;
    assert!(bmc.lockdown_bmc(EnabledDisabled::Enabled).await.is_err());
    bmc.lockdown_bmc(EnabledDisabled::Enabled).await.unwrap();
    server.finish().await;
}

fn manager(locked: bool) -> Value {
    let mut manager: Value =
        serde_json::from_str(include_str!("../model/testdata/manager_lenovo.json")).unwrap();
    manager["Oem"]["Lenovo"]["KCSEnabled"] = json!(!locked);
    manager
}

fn front_usb(locked: bool) -> Value {
    json!({"Oem":{"Lenovo":{"USBManagementPortAssignment":{
        "InactivityTimeoutMins":0, "PortSwitchingTo":"Server",
        "FPMode": if locked { "Server" } else { "Shared" }
    }}}})
}

fn legacy_status(locked: bool) -> Vec<Request> {
    vec![
        get("Managers/1", manager(locked)),
        get(
            SECURITY,
            json!({"Configurator":{"FWRollback":if locked { "Disabled" } else { "Enabled" }}}),
        ),
        get(
            "Managers/1/EthernetInterfaces/ToHost",
            json!({"InterfaceEnabled":!locked}),
        ),
        get("Systems/1", front_usb(locked)),
    ]
}

#[tokio::test]
async fn status_requires_all_supported_controls_and_preserves_old_firmware_status() {
    for (legacy_locked, security, bios, expected) in [
        (true, json!({}), json!({}), StatusInternal::Enabled),
        (false, json!({}), json!({}), StatusInternal::Disabled),
        (
            true,
            json!({"FWLockdown":false}),
            json!({}),
            StatusInternal::Partial,
        ),
        (
            true,
            json!({"FWLockdown":true}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":true,"UEFIVariablesRO":true}}}),
            StatusInternal::Enabled,
        ),
        (
            false,
            json!({"FWLockdown":false}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":false,"UEFIVariablesRO":false}}}),
            StatusInternal::Disabled,
        ),
        (
            true,
            json!({"FWLockdown":true}),
            json!({"Oem":{"Lenovo":{"SettingsLockdown":false,"UEFIVariablesRO":true}}}),
            StatusInternal::Partial,
        ),
        (
            true,
            json!({}),
            json!({"Oem":{"Lenovo":{"UEFIVariablesRO":false}}}),
            StatusInternal::Partial,
        ),
    ] {
        let mut script = legacy_status(legacy_locked);
        script.extend(discovery(security, bios));
        let (bmc, server) = client(script).await;
        assert_eq!(bmc.lockdown_status().await.unwrap().status, expected);
        server.finish().await;
    }
}

fn legacy_mutation(locked: bool) -> Vec<Request> {
    vec![
        get("Managers/1", manager(!locked)),
        patch(
            "Managers/1",
            json!({"Oem":{"Lenovo":{"KCSEnabled":!locked}}}),
        ),
        patch(
            SECURITY,
            json!({"Configurator":{"FWRollback":if locked { "Disabled" } else { "Enabled" }}}),
        ),
        patch(
            "Managers/1/EthernetInterfaces/ToHost",
            json!({"InterfaceEnabled":!locked}),
        ),
        get("Systems/1", front_usb(!locked)),
        patch(
            "Systems/1",
            json!({"Oem":{"Lenovo":{"USBManagementPortAssignment":{"FPMode":if locked { "Server" } else { "Shared" },"PortSwitchingTo":"Server"}}}}),
        ),
    ]
}

#[tokio::test]
async fn full_lockdown_preserves_legacy_writes_and_unlocks_new_controls_before_them() {
    for supported in [false, true] {
        for locked in [false, true] {
            let mut xcc = if supported {
                discovery(
                    json!({"FWLockdown":!locked}),
                    json!({"Oem":{"Lenovo":{"SettingsLockdown":!locked,"UEFIVariablesRO":!locked}}}),
                )
            } else {
                discovery(json!({}), json!({}))
            };
            if supported {
                let fw = patch(SECURITY, json!({"FWLockdown":locked}));
                let bios = patch(
                    BIOS,
                    json!({"Oem":{"Lenovo":{"SettingsLockdown":locked,"UEFIVariablesRO":locked}}}),
                );
                xcc.extend(if locked {
                    vec![bios, fw]
                } else {
                    vec![fw, bios]
                });
            }
            let legacy = legacy_mutation(locked);
            let script = if locked {
                [legacy, xcc].concat()
            } else {
                [xcc, legacy].concat()
            };
            let (bmc, server) = client(script).await;
            bmc.lockdown(if locked {
                EnabledDisabled::Enabled
            } else {
                EnabledDisabled::Disabled
            })
            .await
            .unwrap();
            server.finish().await;
        }
    }
}

#[tokio::test]
async fn absent_optional_resources_are_skipped_but_bios_http_errors_propagate() {
    for code in [401, 403, 500, 404] {
        let mut bios = get(BIOS, json!({}));
        bios.status = code;
        let (bmc, server) = client(vec![get(SECURITY, json!({})), bios]).await;
        assert_eq!(
            bmc.lockdown_bmc(EnabledDisabled::Enabled).await.is_ok(),
            code == 404
        );
        server.finish().await;
    }
    let mut security = get(SECURITY, json!({}));
    security.status = 404;
    let (bmc, server) = client(vec![security, get(BIOS, json!({}))]).await;
    bmc.lockdown_bmc(EnabledDisabled::Disabled).await.unwrap();
    server.finish().await;
}
