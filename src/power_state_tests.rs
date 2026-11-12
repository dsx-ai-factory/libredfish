// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::{rustls, TlsAcceptor};

use crate::{
    model::SystemPowerControl, standard::RedfishStandard, Endpoint, PowerState, Redfish,
    RedfishClientPool, RedfishError,
};

const RESET_REQUEST: &str = "POST /redfish/v1/Systems/1/Actions/ComputerSystem.Reset HTTP/1.1";
const GET_REQUEST: &str = "GET /redfish/v1/Systems/1/ HTTP/1.1";
const DELETE_REQUEST: &str = "DELETE /redfish/v1/AccountService/Accounts/test HTTP/1.1";

struct Server {
    port: u16,
    task: JoinHandle<()>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    /// Stop and join the TLS peer, surfacing any panic that completed before cancellation.
    async fn finish(&mut self) {
        self.task.abort();
        let error = tokio::time::timeout(Duration::from_secs(3), &mut self.task)
            .await
            .expect("TLS peer stops within its shutdown budget")
            .expect_err("TLS peer stays alive until shutdown");
        assert!(error.is_cancelled(), "TLS peer failed: {error}");
    }
}

fn pool() -> RedfishClientPool {
    RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

async fn client(system: Value, status: u16) -> (RedfishStandard, Server) {
    client_with_transport(system, status, pool(), None).await
}

// Record every complete request and accept the expected writes. The selected
// fault loses part of the first matching response after its request is recorded.
async fn client_with_transport(
    mut system: Value,
    status: u16,
    pool: RedfishClientPool,
    truncate_response_to: Option<&'static str>,
) -> (RedfishStandard, Server) {
    system["@odata.id"] = json!("/redfish/v1/Systems/1");
    system["@odata.type"] = json!("#ComputerSystem.v1_22_0.ComputerSystem");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cert = CertificateDer::from_pem_slice(include_bytes!("../tests/cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("../tests/key.pem")).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let tls = TlsAcceptor::from(Arc::new(config));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tls.accept(stream).await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&buffer[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let header = String::from_utf8_lossy(&request[..header_end]);
            let first = header.lines().next().unwrap().to_string();
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
                assert_ne!(n, 0);
                request.extend_from_slice(&buffer[..n]);
            }
            let body = if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&request[header_end..header_end + length]).unwrap()
            };
            let truncate = {
                let mut requests = recorded.lock().unwrap();
                requests.push((first.clone(), body));
                truncate_response_to == Some(first.as_str())
                    && requests.iter().filter(|(line, _)| line == &first).count() == 1
            };
            let (code, response) =
                if first == "GET /redfish/v1/ HTTP/1.1" || first == "GET /redfish/v1 HTTP/1.1" {
                    (
                        200,
                        json!({
                            "@odata.id": "/redfish/v1/",
                            "@odata.type": "#ServiceRoot.v1_16_0.ServiceRoot",
                            "RedfishVersion": "1.16.0",
                            "Vendor": "Dell",
                        })
                        .to_string(),
                    )
                } else if first == "GET /redfish/v1/Managers/ HTTP/1.1" {
                    (
                        200,
                        json!({
                            "@odata.id": "/redfish/v1/Managers",
                            "@odata.type": "#ManagerCollection.ManagerCollection",
                            "Name": "Managers",
                            "Members": [{"@odata.id": "/redfish/v1/Managers/BMC"}],
                        })
                        .to_string(),
                    )
                } else if first == "GET /redfish/v1/Chassis/ HTTP/1.1" {
                    (200, json!({"Members": []}).to_string())
                } else if first == "GET /redfish/v1/Systems/ HTTP/1.1" {
                    (
                        200,
                        json!({
                            "@odata.id": "/redfish/v1/Systems",
                            "@odata.type": "#ComputerSystemCollection.ComputerSystemCollection",
                            "Name": "Systems",
                            "Members": [{"@odata.id": "/redfish/v1/Systems/1"}],
                        })
                        .to_string(),
                    )
                } else if first == GET_REQUEST {
                    (status, system.to_string())
                } else if first == "PATCH /redfish/v1/Chassis/SMM_0 HTTP/1.1" {
                    (204, String::new())
                } else if first == RESET_REQUEST || first == DELETE_REQUEST {
                    (200, "{}".into())
                } else {
                    (404, "{}".into())
                };
            let headers = format!("HTTP/1.1 {code} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
            stream.write_all(headers.as_bytes()).await.unwrap();
            let response_bytes = if truncate {
                assert!(
                    !response.is_empty(),
                    "the selected response fault must drop body bytes"
                );
                &response.as_bytes()[..response.len() / 2]
            } else {
                response.as_bytes()
            };
            stream.write_all(response_bytes).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let mut standard = pool
        .create_standard_client(Endpoint {
            host: "127.0.0.1".into(),
            port: Some(port),
            ..Default::default()
        })
        .unwrap();
    standard.set_system_id("1").unwrap();
    (
        *standard,
        Server {
            port,
            task,
            requests,
        },
    )
}

/// A reset with a truncated response must not be sent again by the opted-out view.
#[tokio::test]
async fn mutation_retry_opt_out_preserves_truncated_reset_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (standard, mut server) = client_with_transport(
            json!({"Id": "1"}),
            200,
            pool().without_mutation_retries(),
            Some(RESET_REQUEST),
        )
        .await;
        let result = standard.power(SystemPowerControl::ForceRestart).await;
        assert!(
            matches!(&result, Err(RedfishError::NetworkError { source, .. }) if !source.is_timeout()),
            "expected a non-timeout mutation failure, got {result:?}"
        );
        assert_eq!(
            *server.requests.lock().unwrap(),
            vec![(
                RESET_REQUEST.to_string(),
                json!({"ResetType": "ForceRestart"}),
            )]
        );
        server.finish().await;
    })
    .await
    .expect("mutation retry opt-out completes within its watchdog");
}

/// Discovery and vendor-client cloning must carry the opt-out into an actual reset.
#[tokio::test]
async fn mutation_retry_opt_out_survives_vendor_client_construction() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let pool = pool().without_mutation_retries();
        let (_, mut server) =
            client_with_transport(json!({"Id": "1"}), 200, pool.clone(), Some(RESET_REQUEST)).await;
        let client = pool.create_client(Endpoint {
            host: "127.0.0.1".into(), port: Some(server.port), ..Default::default()
        }).await.unwrap();
        let result = client.power(SystemPowerControl::ForceRestart).await;
        assert!(
            matches!(&result, Err(RedfishError::NetworkError { source, .. }) if !source.is_timeout()),
            "expected a non-timeout mutation failure, got {result:?}"
        );
        let mutations: Vec<_> = server.requests.lock().unwrap().iter()
            .filter(|(line, _)| !line.starts_with("GET ")).cloned().collect();
        assert_eq!(mutations, vec![(RESET_REQUEST.to_string(), json!({"ResetType": "ForceRestart"}))]);
        server.finish().await;
    }).await.expect("vendor-client retry proof completes within its watchdog");
}

/// A bodyless mutation must obey the same non-GET policy as the reset POST.
#[tokio::test]
async fn mutation_retry_opt_out_preserves_truncated_delete_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (standard, mut server) = client_with_transport(
            json!({"Id": "1"}), 200, pool().without_mutation_retries(), Some(DELETE_REQUEST),
        ).await;
        let result = standard.delete_user("test").await;
        assert!(
            matches!(&result, Err(RedfishError::NetworkError { source, .. }) if !source.is_timeout()),
            "expected a non-timeout mutation failure, got {result:?}"
        );
        assert_eq!(*server.requests.lock().unwrap(), vec![(DELETE_REQUEST.to_string(), Value::Null)]);
        server.finish().await;
    }).await.expect("bodyless mutation retry proof completes within its watchdog");
}

/// Creating an opted-out view preserves the original pool's legacy mutation retry.
#[tokio::test]
async fn opted_out_view_preserves_original_pool_mutation_retry() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let pool = pool();
        let _view = pool.without_mutation_retries();
        let (standard, mut server) =
            client_with_transport(json!({"Id": "1"}), 200, pool, Some(RESET_REQUEST)).await;
        standard
            .power(SystemPowerControl::ForceRestart)
            .await
            .unwrap();
        assert_eq!(
            *server.requests.lock().unwrap(),
            vec![
                (
                    RESET_REQUEST.to_string(),
                    json!({"ResetType": "ForceRestart"}),
                );
                2
            ]
        );
        server.finish().await;
    })
    .await
    .expect("legacy mutation retry completes within its watchdog");
}

/// Read-only requests still recover from a truncated body through the opted-out view.
#[tokio::test]
async fn mutation_retry_opt_out_preserves_get_retry() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (standard, mut server) = client_with_transport(
            json!({"Id": "1", "PowerState": "On"}),
            200,
            pool().without_mutation_retries(),
            Some(GET_REQUEST),
        )
        .await;
        assert_eq!(standard.get_power_state().await.unwrap(), PowerState::On);
        assert_eq!(
            *server.requests.lock().unwrap(),
            vec![(GET_REQUEST.to_string(), Value::Null); 2]
        );
        server.finish().await;
    })
    .await
    .expect("GET retry completes within its watchdog");
}

#[tokio::test]
async fn shared_getter_rejects_missing_and_null_power_state() {
    for system in [json!({"Id": "1"}), json!({"Id": "1", "PowerState": null})] {
        let (standard, _server) = client(system, 200).await;
        assert!(matches!(standard.get_power_state().await,
            Err(RedfishError::MissingKey { key, url })
                if key == "PowerState" && url == "Systems/1/"));
    }
}

#[tokio::test]
async fn shared_getter_preserves_explicit_states_and_errors() {
    for state in [
        PowerState::On,
        PowerState::Off,
        PowerState::PoweringOn,
        PowerState::PoweringOff,
        PowerState::Paused,
        PowerState::Reset,
        PowerState::Unknown,
    ] {
        let (standard, _server) = client(json!({"Id": "1", "PowerState": state}), 200).await;
        assert_eq!(standard.get_power_state().await.unwrap(), state);
    }
    let (standard, _server) = client(json!({"Id": "1", "PowerState": "invalid"}), 200).await;
    assert!(matches!(
        standard.get_power_state().await,
        Err(RedfishError::JsonDeserializeError { .. })
    ));
    let (standard, _server) = client(json!({}), 500).await;
    assert!(matches!(
        standard.get_power_state().await,
        Err(RedfishError::HTTPErrorCode { .. })
    ));
}

#[tokio::test]
async fn vera_rubin_blocks_nic_patch_without_confirmed_power_on() {
    for system in [
        json!({"Id": "1"}),
        json!({"Id": "1", "PowerState": null}),
        json!({"Id": "1", "PowerState": "Off"}),
    ] {
        let (standard, server) = client(system, 200).await;
        let bmc = crate::nvidia_vera_rubin::Bmc::new(standard).unwrap();
        assert!(bmc
            .set_enable_onboard_nics(crate::EnabledDisabled::Disabled)
            .await
            .is_err());
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "GET /redfish/v1/Systems/1/ HTTP/1.1");
    }
}

#[tokio::test]
async fn vera_rubin_explicit_on_allows_nic_patch() {
    for target in [
        crate::EnabledDisabled::Enabled,
        crate::EnabledDisabled::Disabled,
    ] {
        let (standard, server) = client(json!({"Id": "1", "PowerState": "On"}), 200).await;
        let bmc = crate::nvidia_vera_rubin::Bmc::new(standard).unwrap();
        bmc.set_enable_onboard_nics(target).await.unwrap();
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].0, "PATCH /redfish/v1/Chassis/SMM_0 HTTP/1.1");
        assert_eq!(
            requests[1].1,
            json!({"Oem": {"Nvidia": {"HostManagementNetworkAccess": target.is_enabled()}}})
        );
    }
}
