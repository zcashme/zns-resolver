use std::time::Duration;

use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use zcashname_sdk::*;

// Local HTTP server: capture the actual request and close the connection after
// one response. Tests never depend on a live resolver or registry identity.
async fn reply(status: u16, body: String, delay: Duration) -> (String, JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = tokio::time::timeout(Duration::from_secs(3), async {
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let len = socket.read(&mut buf).await.unwrap();
                assert!(len > 0, "request ended before body");
                bytes.extend_from_slice(&buf[..len]);
                assert!(bytes.len() < 65536);
                if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= header_end + 4 + length {
                        break serde_json::from_slice(
                            &bytes[header_end + 4..header_end + 4 + length],
                        )
                        .unwrap();
                    }
                }
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(delay).await;
        let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        // Timed-out clients may already have closed their socket.
        let _ = socket.write_all(response.as_bytes()).await;
        request
    });
    (endpoint, task)
}

fn client(network: Network, endpoints: Vec<String>, timeout: Duration) -> ResolverClient {
    ResolverClient::with_options(
        network,
        Some(endpoints),
        reqwest::Client::builder().no_proxy().build().unwrap(),
        timeout,
    )
    .unwrap()
}

fn status(key: &str) -> Value {
    json!({"synced_height": 42, "synced": true, "viewing_key": key, "treasury_address": TESTNET.treasury_address})
}

fn success(value: Value) -> String {
    json!({"jsonrpc": "2.0", "id": 1, "result": value}).to_string()
}

#[test]
fn presets_default_to_testnet_and_custom_options_are_checked() {
    let default = ResolverClient::default();
    assert_eq!(default.network(), Network::Testnet);
    assert_eq!(default.endpoints(), TESTNET.urls);
    assert_eq!(
        ResolverClient::for_network(Network::Mainnet).endpoints(),
        MAINNET.urls
    );
    assert_eq!(
        ResolverClient::new("http://localhost/rpc").endpoints(),
        ["http://localhost/rpc"]
    );
    for endpoints in [
        vec![],
        vec!["file:///tmp/resolver".into()],
        vec!["not a url".into()],
    ] {
        assert!(ResolverClient::with_options(
            Network::Testnet,
            Some(endpoints),
            reqwest::Client::new(),
            Duration::from_secs(1)
        )
        .is_err());
    }
    assert!(ResolverClient::with_options(
        Network::Testnet,
        None,
        reqwest::Client::new(),
        Duration::ZERO
    )
    .is_err());
}

#[tokio::test]
async fn status_matches_current_resolver_without_registered_count() {
    let (url, request) = reply(200, success(status(TESTNET.viewing_key)), Duration::ZERO).await;
    let value = ResolverClient::new(url).status().await.unwrap();
    assert_eq!(value.synced_height, 42);
    assert_eq!(value.synced, Some(true));
    assert_eq!(
        value.treasury_address.as_deref(),
        Some(TESTNET.treasury_address)
    );
    assert_eq!(request.await.unwrap()["params"], json!({}));
    let older: Status = serde_json::from_value(json!({"viewing_key": "old"})).unwrap();
    assert_eq!(older.synced_height, 0);
    assert_eq!(older.synced, None);
    assert_eq!(older.treasury_address, None);
}

#[tokio::test]
async fn verifies_registry_key_and_reports_mismatch() {
    for (network, key, matches) in [
        (Network::Testnet, TESTNET.viewing_key, true),
        (Network::Mainnet, MAINNET.viewing_key, true),
        (Network::Mainnet, TESTNET.viewing_key, false),
    ] {
        let (url, request) = reply(200, success(status(key)), Duration::ZERO).await;
        let result = client(network, vec![url], Duration::from_secs(1))
            .verify()
            .await;
        if matches {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(Error::Verification {
                    network: Network::Mainnet
                })
            ));
        }
        assert_eq!(request.await.unwrap()["method"], "status");
    }
}

#[tokio::test]
async fn name_lookups_normalize_and_invalid_names_never_hit_the_network() {
    let (url, request) = reply(200, success(Value::Null), Duration::ZERO).await;
    assert_eq!(
        ResolverClient::new(url)
            .resolve_name(" Alice ")
            .await
            .unwrap(),
        None
    );
    assert_eq!(request.await.unwrap()["params"], json!({"query": "alice"}));
    let client = ResolverClient::new("not a URL");
    for name in ["", "alice.zcash", "a-name", "K", &"a".repeat(64)] {
        assert_eq!(client.resolve_name(name).await.unwrap(), None);
        assert!(!client.is_available(name).await.unwrap());
    }
    let (url, request) = reply(200, success(Value::Null), Duration::ZERO).await;
    assert!(ResolverClient::new(url)
        .is_available(" ALICE ")
        .await
        .unwrap());
    request.await.unwrap();
}

#[tokio::test]
async fn events_normalize_filters_and_preserve_wire_fields() {
    let (url, request) = reply(
        200,
        success(json!({"events": [], "total": 0, "limit": 10, "offset": 5})),
        Duration::ZERO,
    )
    .await;
    let page = ResolverClient::new(url)
        .events(EventsFilter {
            name: Some(" Alice ".into()),
            action: Some(Action::Update),
            since_height: Some(42),
            limit: Some(10),
            offset: Some(5),
        })
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    assert_eq!(
        request.await.unwrap()["params"],
        json!({"name": "alice", "action": "update", "since_height": 42, "limit": 10, "offset": 5})
    );
    let (url, request) = reply(
        200,
        success(json!({"events": [], "total": 0, "limit": 50, "offset": 0})),
        Duration::ZERO,
    )
    .await;
    ResolverClient::new(url)
        .events(EventsFilter::default())
        .await
        .unwrap();
    assert_eq!(request.await.unwrap()["params"], json!({}));
}

#[tokio::test]
async fn list_shorthands_use_typed_paginated_endpoints() {
    let record = json!({"name":"alice", "address":"u1example", "txid":"00", "height":42, "last_action":"claim", "expires_at":"none"});
    for reverse in [false, true] {
        let (url, request) = reply(
            200,
            success(json!({"items":[record], "total":12, "limit":1, "offset":2})),
            Duration::ZERO,
        )
        .await;
        let client = ResolverClient::new(url);
        let records = if reverse {
            client.resolve_address("u1example", Some(1), Some(2)).await
        } else {
            client.list_all_registrations(Some(1), Some(2)).await
        }
        .unwrap();
        assert_eq!(records[0].name, "alice");
        let request = request.await.unwrap();
        assert_eq!(
            request["method"],
            if reverse {
                "reverse_lookup"
            } else {
                "list_names"
            }
        );
        assert_eq!(request["params"]["limit"], 1);
        assert_eq!(request["params"]["offset"], 2);
        if reverse {
            assert_eq!(request["params"]["address"], "u1example");
        }
    }
}

#[tokio::test]
async fn falls_back_for_server_and_malformed_responses_using_the_same_request_id() {
    for (status_code, body) in [
        (503, "unavailable".into()),
        (200, "not JSON".into()),
        (200, "[]".into()),
        (200, success(json!({"wrong": "status shape"}))),
        (
            200,
            json!({"jsonrpc":"2.0", "id":99, "result":status(TESTNET.viewing_key)}).to_string(),
        ),
    ] {
        let (first, first_request) = reply(status_code, body, Duration::ZERO).await;
        let (second, second_request) =
            reply(200, success(status(TESTNET.viewing_key)), Duration::ZERO).await;
        let value = client(
            Network::Testnet,
            vec![first, second],
            Duration::from_secs(1),
        )
        .status()
        .await
        .unwrap();
        assert_eq!(value.synced_height, 42);
        assert_eq!(first_request.await.unwrap(), second_request.await.unwrap());
    }
}

#[tokio::test]
async fn rpc_and_http_client_errors_do_not_fall_back() {
    for (code, body) in [
        (
            200,
            json!({"jsonrpc":"2.0", "id":1, "error":{"code":-32602, "message":"bad params"}})
                .to_string(),
        ),
        (400, "bad request".into()),
    ] {
        let (first, request) = reply(code, body, Duration::ZERO).await;
        // A contacted second endpoint would replace the expected error with
        // a URL error, making this test fail.
        let result = client(
            Network::Testnet,
            vec![first, "http://127.0.0.1:1".into()],
            Duration::from_secs(1),
        )
        .status()
        .await;
        match result {
            Err(Error::Rpc { code: -32602, .. }) if code == 200 => {}
            Err(Error::Http(error)) if code == 400 => {
                assert_eq!(error.status().unwrap().as_u16(), 400)
            }
            other => panic!("unexpected result: {other:?}"),
        }
        request.await.unwrap();
    }
}

#[tokio::test]
async fn request_timeout_is_bounded_and_can_fall_back() {
    let (slow, slow_request) = reply(
        200,
        success(status(TESTNET.viewing_key)),
        Duration::from_millis(300),
    )
    .await;
    let (fast, fast_request) =
        reply(200, success(status(TESTNET.viewing_key)), Duration::ZERO).await;
    let value = client(
        Network::Testnet,
        vec![slow, fast],
        Duration::from_millis(100),
    )
    .status()
    .await
    .unwrap();
    assert_eq!(value.synced_height, 42);
    fast_request.await.unwrap();
    slow_request.await.unwrap();
    let (slow, request) = reply(
        200,
        success(status(TESTNET.viewing_key)),
        Duration::from_millis(300),
    )
    .await;
    let error = client(Network::Testnet, vec![slow], Duration::from_millis(100))
        .status()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http(ref e) if e.is_timeout()));
    request.await.unwrap();
}

#[tokio::test]
async fn transport_failure_falls_back_and_override_has_no_implicit_fallback() {
    let (endpoint, request) =
        reply(200, success(status(TESTNET.viewing_key)), Duration::ZERO).await;
    let value = client(
        Network::Testnet,
        vec!["http://127.0.0.1:1".into(), endpoint],
        Duration::from_secs(1),
    )
    .status()
    .await
    .unwrap();
    assert_eq!(value.synced_height, 42);
    request.await.unwrap();
    let error = client(
        Network::Mainnet,
        vec!["http://127.0.0.1:1".into()],
        Duration::from_secs(1),
    )
    .status()
    .await
    .unwrap_err();
    assert!(matches!(error, Error::Http(_)));
}
