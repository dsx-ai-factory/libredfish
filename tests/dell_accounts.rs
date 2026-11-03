// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

use std::{sync::Arc, time::Duration};

use libredfish::{
    model::service_root::RedfishVendor, Endpoint, Redfish, RedfishClientPool, RedfishError, RoleId,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::{rustls, TlsAcceptor};

const ACCOUNTS: &str = "AccountService/Accounts";

struct Request {
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    response: Value,
}

fn account_payload() -> Value {
    json!({
        "UserName": "new-user",
        "Password": "test-password",
        "RoleId": "Administrator",
        "Enabled": true,
    })
}

fn post(status: u16) -> Request {
    Request {
        method: "POST",
        path: ACCOUNTS.into(),
        body: Some(account_payload()),
        status,
        response: json!({}),
    }
}

fn slot(id: u8, enabled: Option<bool>) -> Request {
    Request {
        method: "GET",
        path: format!("{ACCOUNTS}/{id}"),
        body: None,
        status: 200,
        response: json!({
            "@odata.id": format!("/redfish/v1/{ACCOUNTS}/{id}"),
            "@odata.type": "#ManagerAccount.v1_8_0.ManagerAccount",
            "UserName": "existing-user",
            "RoleId": "None",
            "Enabled": enabled,
        }),
    }
}

fn patch(id: u8, status: u16) -> Request {
    let mut body = account_payload();
    // These fields are serialized by the existing edit_account implementation.
    body["@odata.id"] = json!("");
    body["@odata.type"] = json!("");
    body["@odata.context"] = Value::Null;
    body["@odata.etag"] = Value::Null;
    Request {
        method: "PATCH",
        path: format!("{ACCOUNTS}/{id}"),
        body: Some(body),
        status,
        response: json!({}),
    }
}

// Exercise the public Redfish interface over TLS, checking every request's
// order, method, URI and JSON body. No Python environment or fixed port is needed.
struct Server(JoinHandle<()>);

impl Server {
    async fn finish(mut self) {
        tokio::time::timeout(Duration::from_secs(5), &mut self.0)
            .await
            .expect("all scripted requests must arrive")
            .expect("request script must pass");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn client(script: Vec<Request>) -> (Box<dyn Redfish>, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cert = CertificateDer::from_pem_slice(include_bytes!("cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("key.pem")).unwrap();
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
            let mut stream = BufReader::new(tls.accept(stream).await.unwrap());
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(
                line,
                format!(
                    "{} /redfish/v1/{} HTTP/1.1\r\n",
                    expected.method, expected.path
                )
            );
            let mut length = 0;
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            match expected.body {
                Some(expected) => {
                    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected)
                }
                None => assert!(body.is_empty()),
            }
            let response = if expected.status == 204 {
                String::new()
            } else {
                expected.response.to_string()
            };
            let headers = format!(
                "HTTP/1.1 {} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                expected.status, response.len()
            );
            let mut stream = stream.into_inner();
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
    let redfish = standard.set_vendor(RedfishVendor::Dell).await.unwrap();
    (redfish, Server(task))
}

async fn create(redfish: &dyn Redfish) -> Result<(), RedfishError> {
    redfish
        .create_user("new-user", "test-password", RoleId::Administrator)
        .await
}

fn assert_http_error(error: RedfishError, expected: u16) {
    match error {
        RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            ..
        } => {
            assert_eq!(status_code.as_u16(), expected);
            assert_eq!(response_body, "{}");
        }
        error => panic!("expected HTTP {expected}, got {error:?}"),
    }
}

#[tokio::test]
async fn modern_creation_posts_an_enabled_account_without_probing_slots() {
    for status in [200, 201, 204] {
        let (redfish, server) = client(vec![post(status)]).await;
        create(redfish.as_ref()).await.unwrap();
        server.finish().await;
    }
}

#[tokio::test]
async fn legacy_creation_skips_missing_enabled_and_unknown_slots() {
    let mut missing = slot(3, None);
    missing.status = 404;
    missing.response = json!({});
    let (redfish, server) = client(vec![
        post(405),
        missing,
        slot(4, Some(true)),
        slot(5, None),
        slot(6, Some(false)),
        patch(6, 204),
    ])
    .await;
    create(redfish.as_ref()).await.unwrap();
    server.finish().await;
}

#[tokio::test]
async fn post_errors_do_not_fall_back_to_slot_mutation() {
    for status in [400, 401, 403, 404, 429, 500] {
        let (redfish, server) = client(vec![post(status)]).await;
        assert_http_error(create(redfish.as_ref()).await.unwrap_err(), status);
        server.finish().await;
    }
    let (redfish, server) = client(vec![post(409)]).await;
    assert!(matches!(
        create(redfish.as_ref()).await,
        Err(RedfishError::UnnecessaryOperation)
    ));
    server.finish().await;
}

#[tokio::test]
async fn legacy_slot_lookup_errors_stop_without_patching() {
    for status in [401, 403, 500] {
        let mut lookup = slot(3, None);
        lookup.status = status;
        lookup.response = json!({});
        let (redfish, server) = client(vec![post(405), lookup]).await;
        assert_http_error(create(redfish.as_ref()).await.unwrap_err(), status);
        server.finish().await;
    }
}

#[tokio::test]
async fn malformed_slot_response_is_not_an_available_account() {
    let mut lookup = slot(3, None);
    lookup.response = json!({});
    let (redfish, server) = client(vec![post(405), lookup]).await;
    assert!(matches!(
        create(redfish.as_ref()).await,
        Err(RedfishError::JsonDeserializeError { .. })
    ));
    server.finish().await;
}

#[tokio::test]
async fn legacy_patch_failure_is_returned_without_trying_another_slot() {
    for status in [400, 403, 500] {
        let (redfish, server) =
            client(vec![post(405), slot(3, Some(false)), patch(3, status)]).await;
        assert_http_error(create(redfish.as_ref()).await.unwrap_err(), status);
        server.finish().await;
    }
}

#[tokio::test]
async fn legacy_full_or_missing_slots_report_too_many_users() {
    for missing in [false, true] {
        let mut script = vec![post(405)];
        for id in 3..=16 {
            let mut lookup = slot(id, Some(true));
            if missing {
                lookup.status = 404;
                lookup.response = json!({});
            }
            script.push(lookup);
        }
        let (redfish, server) = client(script).await;
        assert!(matches!(
            create(redfish.as_ref()).await,
            Err(RedfishError::TooManyUsers)
        ));
        server.finish().await;
    }
}

#[tokio::test]
async fn legacy_creation_can_use_the_last_disabled_slot() {
    let mut script = vec![post(405)];
    script.extend((3..16).map(|id| slot(id, Some(true))));
    script.extend([slot(16, Some(false)), patch(16, 204)]);
    let (redfish, server) = client(script).await;
    create(redfish.as_ref()).await.unwrap();
    server.finish().await;
}
