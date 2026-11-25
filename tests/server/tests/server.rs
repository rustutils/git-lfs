//! The server, started in-process and driven over HTTP.

use lfstest_server::Server;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const LFS_JSON: &str = "application/vnd.git-lfs+json";

fn oid(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn batch(client: &reqwest::Client, server: &Server, operation: &str, data: &[u8]) -> Value {
    client
        .post(format!("{}/repo.git/info/lfs/objects/batch", server.url()))
        .basic_auth("user", Some("pass"))
        .header("Accept", LFS_JSON)
        .header("Content-Type", LFS_JSON)
        .body(
            json!({
                "operation": operation,
                "objects": [{ "oid": oid(data), "size": data.len() }],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn uploaded_objects_can_be_downloaded() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let client = reqwest::Client::new();
    let data = b"hello";

    let upload = batch(&client, &server, "upload", data).await;
    let href = upload["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap();
    let put = client.put(href).body(&data[..]).send().await.unwrap();
    assert_eq!(put.status(), 200);

    let download = batch(&client, &server, "download", data).await;
    let object = &download["objects"][0];
    assert_eq!(object["oid"], oid(data));
    // Go encodes an unset expiry as the zero time.
    assert_eq!(
        object["actions"]["download"]["expires_at"],
        "0001-01-01T00:00:00Z"
    );
    let href = object["actions"]["download"]["href"].as_str().unwrap();
    let body = client
        .get(href)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&body[..], data);

    // Already stored: no upload action.
    let again = batch(&client, &server, "upload", data).await;
    assert!(again["objects"][0].get("actions").is_none());
}

#[tokio::test]
async fn missing_objects_are_errors_in_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let resp = batch(&reqwest::Client::new(), &server, "download", b"nope").await;
    assert_eq!(resp["objects"][0]["error"]["code"], 404);
}

#[tokio::test]
async fn lfs_requests_need_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{}/repo.git/info/lfs/objects/batch", server.url()))
        .header("Accept", LFS_JSON)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.headers()["lfs-authenticate"],
        "Basic realm=\"testsuite\""
    );
}

#[tokio::test]
async fn tls_port_serves_https_and_plain_http() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let cert = reqwest::Certificate::from_pem(server.cert_pem().as_bytes()).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(cert)
        .build()
        .unwrap();
    let https = client
        .get(format!("{}/status", server.ssl_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(https.status(), 200);

    let plain_url = server.ssl_url().replacen("https://", "http://", 1);
    let plain = client
        .get(format!("{plain_url}/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), 200);
}

#[tokio::test]
async fn client_cert_server_requires_a_client_cert() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let cert = reqwest::Certificate::from_pem(server.cert_pem().as_bytes()).unwrap();
    let url = format!("{}/status", server.client_cert_url());

    let without = reqwest::Client::builder()
        .add_root_certificate(cert.clone())
        .build()
        .unwrap();
    assert!(without.get(&url).send().await.is_err());

    let identity = format!("{}{}", server.client_cert_pem(), server.client_key_pem());
    let with = reqwest::Client::builder()
        .add_root_certificate(cert)
        .identity(reqwest::Identity::from_pem(identity.as_bytes()).unwrap())
        .build()
        .unwrap();
    assert_eq!(with.get(&url).send().await.unwrap().status(), 200);
}

#[tokio::test]
async fn cut_short_download_ends_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path()).unwrap();
    let client = reqwest::Client::new();
    // These contents make the server stop the download halfway.
    let data = b"storage-download-retry-range";

    let upload = batch(&client, &server, "upload", data).await;
    let href = upload["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap();
    client.put(href).body(&data[..]).send().await.unwrap();

    let download = batch(&client, &server, "download", data).await;
    let href = download["objects"][0]["actions"]["download"]["href"]
        .as_str()
        .unwrap();
    let resp = client.get(href).send().await.unwrap();
    assert_eq!(resp.content_length(), Some(data.len() as u64));
    let body = tokio::time::timeout(std::time::Duration::from_secs(10), resp.bytes())
        .await
        .expect("the server closes the connection");
    assert!(body.is_err());
}
