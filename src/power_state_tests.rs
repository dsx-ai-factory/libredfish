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
    standard::RedfishStandard, Endpoint, PowerState, Redfish, RedfishClientPool, RedfishError,
};

struct Server {
    task: JoinHandle<()>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// Record every request and accept PATCHes so an unexpected write cannot hide
// behind a transport error. Each test owns an ephemeral port and TLS peer.
async fn client(mut system: Value, status: u16) -> (RedfishStandard, Server) {
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
            recorded.lock().unwrap().push((first.clone(), body));
            let (code, response) = if first == "GET /redfish/v1/Systems/1/ HTTP/1.1" {
                (status, system.to_string())
            } else if first == "PATCH /redfish/v1/Chassis/SMM_0 HTTP/1.1" {
                (204, String::new())
            } else {
                (404, "{}".into())
            };
            let headers = format!("HTTP/1.1 {code} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
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
    standard.set_system_id("1").unwrap();
    (*standard, Server { task, requests })
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
