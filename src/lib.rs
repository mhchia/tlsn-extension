mod requests;

use std::panic;
use std::ops::Range;
use web_time::Instant;

use hyper::{body::to_bytes, Body, Request, StatusCode};
use futures::{AsyncWriteExt, TryFutureExt};
use futures::channel::oneshot;
use tlsn_prover::tls::{Prover, ProverConfig};
use tlsn_core::proof::TlsProof;

// use tokio::io::AsyncWriteExt as _;
use serde_json;
use wasm_bindgen::JsValue;

use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use tokio_util::compat::FuturesAsyncWriteCompatExt;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use tracing_web::{MakeConsoleWriter, performance_layer};
use tracing_subscriber::fmt::format::Pretty;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::prelude::*;

use ws_stream_wasm::{*};

use crate::requests::{NotarizationSessionRequest, NotarizationSessionResponse, ClientType};

pub use wasm_bindgen_rayon::init_thread_pool;

use wasm_bindgen_futures::JsFuture;
use web_sys::{Request as WebsysRequest, RequestInit, Headers, RequestMode, Response};
use js_sys::{JSON, Array};

// A macro to provide `println!(..)`-style syntax for `console.log` logging.
macro_rules! log {
    ( $( $t:tt )* ) => {
        web_sys::console::log_1(&format!( $( $t )* ).into());
    }
}

extern crate console_error_panic_hook;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = self)]
    fn fetch(request: &web_sys::Request) -> js_sys::Promise;
}

async fn fetch_as_json_string(url: &str, opts: &RequestInit) -> Result<String, JsValue> {
    let request = WebsysRequest::new_with_str_and_init(url, opts)?;
    let promise = fetch(&request);
    let future = JsFuture::from(promise);
    let resp_value = future.await?;
    let resp: Response = resp_value.dyn_into().unwrap();
    let json = JsFuture::from(resp.json()?).await?;
    let stringified = JSON::stringify(&json).unwrap();
    Ok(stringified.as_string().unwrap())
}

fn make_app_request(
    method: &str,
    url: &str,
    headers: &JsValue,
    body: Vec<u8>,
) -> Result<Request<Body>, JsValue> {
    // Build the HTTP request to fetch the DMs
    let mut request_builder = Request::builder()
        .method(method)
        .uri(url);

    let array: Array = Array::from(headers);
    let length = array.length();

    for i in 0..length {
        let pair_js: JsValue = array.get(i);
        let header_pair: Array = Array::from(&pair_js);

        let key: JsValue = header_pair.get(0);
        let value: JsValue = header_pair.get(1);

        if let Some(key_str) = key.as_string() {
            if let Some(value_str) = value.as_string() {
                request_builder = request_builder.header(key_str, value_str);
            }
        }
    }

    let request = request_builder
        .body(Body::from(body))
        .unwrap();
    Ok(request)
}


fn string_list_to_bytes_vec(secrets: &JsValue) -> Vec<Vec<u8>> {
    let array: Array = Array::from(secrets);
    let length = array.length();
    let mut byte_slices: Vec<Vec<u8>> = Vec::new();

    for i in 0..length {
        let secret_js: JsValue = array.get(i);
        let secret_str: String = secret_js.as_string().unwrap();
        let secret_bytes = secret_str.into_bytes();
        byte_slices.push(secret_bytes);
    }
    byte_slices
}

#[wasm_bindgen]
pub async fn set_trace() {
    let fmt_layer = tracing_subscriber::fmt::layer()
    .with_ansi(false) // Only partially supported across browsers
    .with_timer(UtcTime::rfc_3339()) // std::time is not available in browsers
    .with_writer(MakeConsoleWriter); // write events to the console
    let perf_layer = performance_layer()
        .with_details_from_fields(Pretty::default());

    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::DEBUG)
        .with(fmt_layer)
        .with(perf_layer)
        .init(); // Install these as subscribers to tracing events
}

#[wasm_bindgen]
pub async fn notarize(
    max_transcript_size: usize,
    notary_host: &str,
    notary_port: u16,
    server_domain: &str,
    websocket_proxy_url: &str,
    method: &str,
    url: &str,
    headers: JsValue,
    body: Vec<u8>,
    secrets: JsValue,
    reveals: JsValue,
) -> Result<String, JsValue> {
    // https://github.com/rustwasm/console_error_panic_hook
    panic::set_hook(Box::new(console_error_panic_hook::hook));

    let start_time = Instant::now();

    /*
     * Connect Notary with websocket
     */

    let mut opts = RequestInit::new();
    opts.method("POST");
    // opts.method("GET");
    opts.mode(RequestMode::Cors);

    // set headers
    let notary_headers = Headers::new().unwrap();
    notary_headers.append("Host", notary_host).unwrap();
    notary_headers.append("Content-Type", "application/json").unwrap();
    opts.headers(&notary_headers);

    // set body
    let payload = serde_json::to_string(&NotarizationSessionRequest {
        client_type: ClientType::Websocket,
        max_transcript_size: Some(max_transcript_size),
    })
    .unwrap();
    opts.body(Some(&JsValue::from_str(&payload)));

    // session url
    let session_url = format!("https://{}:{}/session", notary_host, notary_port);
    let rust_string = fetch_as_json_string(&session_url, &opts).await.unwrap();
    let notarization_response = serde_json::from_str::<NotarizationSessionResponse>(&rust_string).unwrap();
    log!("Response: {}", rust_string);

    log!("Notarization response: {:?}", notarization_response,);
    let notary_wss_url = format!("wss://{}:{}/notarize?sessionId={}", notary_host, notary_port, notarization_response.session_id);
    let (mut notary_ws_meta, mut notary_ws_stream) = WsMeta::connect(
        notary_wss_url,
         None
        ).await
        .expect_throw( "assume the notary ws connection succeeds" );
    let mut notary_ws_stream_into = notary_ws_stream.into_io();

    /*
        Connect Application Server with websocket proxy
     */

    let (mut client_ws_meta, mut client_ws_stream) = WsMeta::connect(
        websocket_proxy_url,
        None ).await
        .expect_throw( "assume the client ws connection succeeds" );
    let mut client_ws_stream_into = client_ws_stream.into_io();

    log!("!@# 0");

    // Because the root cert of "test-server.io" is self-signed, we must load the `rootCert.der`. Otherwise, you get
    // an error like this: "WARN connect:tls_connection: tls_client::conn: Sending fatal alert BadCertificate" in `handled_prover_fut`
    // Ref: https://github.com/tlsnotary/tlsn/blob/31708c080597b1e176cd5d892bfd44496bfdbf36/notary-server/tests/integration_test.rs#L423
    let cert = [48, 130, 3, 117, 48, 130, 2, 93, 160, 3, 2, 1, 2, 2, 20, 96, 189, 77, 78, 171, 178, 89, 136, 5, 74, 27, 179, 63, 116, 151, 81, 109, 75, 68, 187, 48, 13, 6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 11, 5, 0, 48, 74, 49, 11, 48, 9, 6, 3, 85, 4, 6, 19, 2, 65, 85, 49, 19, 48, 17, 6, 3, 85, 4, 8, 12, 10, 83, 111, 109, 101, 45, 83, 116, 97, 116, 101, 49, 18, 48, 16, 6, 3, 85, 4, 10, 12, 9, 116, 108, 115, 110, 111, 116, 97, 114, 121, 49, 18, 48, 16, 6, 3, 85, 4, 3, 12, 9, 116, 108, 115, 110, 111, 116, 97, 114, 121, 48, 30, 23, 13, 50, 51, 48, 54, 48, 55, 48, 49, 51, 52, 50, 55, 90, 23, 13, 50, 56, 48, 54, 48, 53, 48, 49, 51, 52, 50, 55, 90, 48, 74, 49, 11, 48, 9, 6, 3, 85, 4, 6, 19, 2, 65, 85, 49, 19, 48, 17, 6, 3, 85, 4, 8, 12, 10, 83, 111, 109, 101, 45, 83, 116, 97, 116, 101, 49, 18, 48, 16, 6, 3, 85, 4, 10, 12, 9, 116, 108, 115, 110, 111, 116, 97, 114, 121, 49, 18, 48, 16, 6, 3, 85, 4, 3, 12, 9, 116, 108, 115, 110, 111, 116, 97, 114, 121, 48, 130, 1, 34, 48, 13, 6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 1, 5, 0, 3, 130, 1, 15, 0, 48, 130, 1, 10, 2, 130, 1, 1, 0, 202, 19, 224, 151, 8, 227, 105, 76, 83, 190, 84, 25, 43, 114, 14, 85, 216, 168, 174, 96, 179, 120, 112, 175, 201, 155, 28, 5, 202, 37, 110, 18, 252, 9, 186, 237, 37, 137, 40, 205, 243, 11, 193, 160, 82, 113, 202, 129, 46, 147, 27, 190, 233, 78, 120, 160, 65, 7, 121, 101, 246, 39, 223, 121, 105, 16, 33, 201, 225, 155, 138, 51, 127, 176, 221, 27, 177, 33, 14, 122, 116, 148, 20, 184, 209, 73, 131, 87, 140, 199, 41, 186, 165, 83, 48, 65, 183, 189, 251, 68, 102, 131, 72, 60, 223, 211, 13, 155, 242, 213, 78, 3, 70, 100, 5, 124, 59, 104, 105, 91, 143, 88, 164, 87, 142, 70, 22, 92, 135, 229, 191, 176, 10, 247, 27, 161, 70, 137, 146, 222, 2, 150, 90, 133, 242, 226, 40, 224, 43, 35, 167, 113, 42, 44, 201, 198, 91, 174, 121, 232, 115, 110, 144, 186, 34, 98, 90, 150, 243, 213, 76, 251, 176, 180, 201, 162, 248, 221, 19, 163, 62, 189, 37, 92, 240, 105, 10, 181, 125, 142, 123, 21, 73, 160, 73, 218, 237, 192, 123, 126, 74, 129, 170, 133, 197, 195, 43, 95, 58, 76, 2, 184, 210, 140, 69, 210, 91, 161, 162, 237, 119, 72, 237, 181, 140, 165, 249, 203, 191, 119, 178, 70, 75, 132, 206, 72, 66, 187, 83, 207, 76, 13, 68, 5, 174, 30, 18, 41, 0, 170, 41, 84, 238, 233, 10, 205, 2, 3, 1, 0, 1, 163, 83, 48, 81, 48, 29, 6, 3, 85, 29, 14, 4, 22, 4, 20, 44, 227, 39, 221, 95, 17, 55, 48, 193, 170, 0, 51, 73, 129, 103, 202, 252, 178, 177, 165, 48, 31, 6, 3, 85, 29, 35, 4, 24, 48, 22, 128, 20, 44, 227, 39, 221, 95, 17, 55, 48, 193, 170, 0, 51, 73, 129, 103, 202, 252, 178, 177, 165, 48, 15, 6, 3, 85, 29, 19, 1, 1, 255, 4, 5, 48, 3, 1, 1, 255, 48, 13, 6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 11, 5, 0, 3, 130, 1, 1, 0, 172, 130, 152, 136, 158, 158, 46, 163, 92, 203, 85, 2, 131, 139, 219, 161, 21, 129, 201, 81, 184, 156, 222, 77, 56, 250, 1, 53, 20, 245, 197, 60, 196, 62, 200, 196, 161, 154, 112, 17, 209, 60, 174, 130, 131, 226, 49, 186, 61, 254, 247, 202, 35, 44, 44, 104, 82, 80, 14, 199, 4, 68, 250, 53, 52, 185, 131, 188, 6, 128, 98, 244, 64, 244, 8, 120, 177, 203, 127, 90, 11, 89, 189, 123, 102, 169, 166, 139, 125, 122, 52, 28, 240, 60, 69, 241, 242, 201, 202, 253, 103, 1, 204, 248, 166, 109, 194, 145, 12, 6, 9, 27, 149, 26, 132, 98, 114, 104, 129, 57, 181, 197, 206, 70, 174, 237, 43, 134, 2, 201, 195, 53, 169, 58, 180, 119, 30, 81, 140, 239, 153, 39, 93, 122, 210, 123, 45, 93, 21, 33, 70, 233, 162, 148, 245, 181, 23, 60, 78, 183, 236, 39, 104, 156, 218, 211, 188, 240, 44, 95, 77, 166, 25, 192, 135, 165, 198, 10, 200, 172, 207, 242, 150, 242, 199, 11, 248, 51, 110, 233, 109, 62, 138, 184, 177, 132, 105, 119, 47, 47, 98, 116, 212, 11, 152, 53, 4, 90, 131, 186, 178, 157, 204, 21, 110, 199, 123, 152, 137, 131, 49, 101, 33, 141, 104, 163, 187, 144, 22, 65, 209, 27, 58, 225, 2, 80, 223, 9, 202, 2, 28, 91, 199, 115, 147, 46, 168, 90, 111, 129, 43, 22, 18, 128, 182, 1];
    let mut root_store = tls_core::anchors::RootCertStore::empty();
    root_store
        .add(&tls_core::key::Certificate(cert.to_vec()))
        .unwrap();
    // Basic default prover config
    let config = ProverConfig::builder()
        .id(notarization_response.session_id)
        .server_dns(server_domain)
        .root_cert_store(root_store)
        .max_transcript_size(max_transcript_size)
        .build()
        .unwrap();

    log!("!@# 1");

    // Create a Prover and set it up with the Notary
    // This will set up the MPC backend prior to connecting to the server.
    let prover = Prover::new(config)
        .setup(notary_ws_stream_into)
        .await
        .unwrap();


    // Bind the Prover to the server connection.
    // The returned `mpc_tls_connection` is an MPC TLS connection to the Server: all data written
    // to/read from it will be encrypted/decrypted using MPC with the Notary.
    let (mpc_tls_connection, prover_fut) = prover.connect(client_ws_stream_into).await.unwrap();

    log!("!@# 3");


    // let prover_task = tokio::spawn(prover_fut);
    let (prover_sender, prover_receiver) = oneshot::channel();
    let handled_prover_fut = async {
        match prover_fut.await {
            Ok(prover_result) => {
                // Send the prover
                let _ = prover_sender.send(prover_result);
            },
            Err(err) => {
                panic!("An error occurred in prover_fut: {:?}", err);
            }
        }
    };
    spawn_local(handled_prover_fut);
    log!("!@# 7");

    // Attach the hyper HTTP client to the TLS connection
    let (mut request_sender, connection) = hyper::client::conn::handshake(mpc_tls_connection.compat())
        .await
        .unwrap();
    log!("!@# 8");

    // Spawn the HTTP task to be run concurrently
    // let connection_task = tokio::spawn(connection.without_shutdown());
    let (connection_sender, connection_receiver) = oneshot::channel();
    let connection_fut = connection.without_shutdown();
    let handled_connection_fut = async {
        match connection_fut.await {
            Ok(connection_result) => {
                // Send the connection
                let _ = connection_sender.send(connection_result);
            },
            Err(err) => {
                panic!("An error occurred in connection_task: {:?}", err);
            }
        }
    };
    spawn_local(handled_connection_fut);
    log!("!@# 9");

    // Build the HTTP request to fetch the DMs
    let request_app = make_app_request(method, url, &headers, body).unwrap();

    log!("Starting an MPC TLS connection with the server");

    // Send the request to the Server and get a response via the MPC TLS connection
    let response = request_sender.send_request(request_app).await.unwrap();

    log!("Got a response from the server");

    assert!(response.status() == StatusCode::OK);

    log!("Request OK");

    // Pretty printing :)
    let payload = to_bytes(response.into_body()).await.unwrap().to_vec();
    let parsed =
        serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&payload)).unwrap();
    log!("!@# 10");
    log!("{}", serde_json::to_string_pretty(&parsed).unwrap());
    log!("!@# 11");

    // Close the connection to the server
    // let mut client_socket = connection_task.await.unwrap().unwrap().io.into_inner();
    let mut client_socket = connection_receiver.await.unwrap().io.into_inner();
    log!("!@# 12");
    client_socket.close().await.unwrap();
    log!("!@# 13");

    // The Prover task should be done now, so we can grab it.
    // let mut prover = prover_task.await.unwrap().unwrap();
    let prover = prover_receiver.await.unwrap();
    let mut prover = prover.start_notarize();
    log!("!@# 14");

    let secrets_vecs = string_list_to_bytes_vec(&secrets);
    let secrets_slices: Vec<&[u8]> = secrets_vecs.iter().map(|vec| vec.as_slice()).collect();

    // Identify the ranges in the transcript that contain secrets
    let (sent_public_ranges, sent_private_ranges) = find_ranges(
        prover.sent_transcript().data(),
        secrets_slices.as_slice(),
    );
    log!("!@# 15");

    let reveal_vecs = string_list_to_bytes_vec(&reveals);
    let reveal_slices: Vec<&[u8]> = reveal_vecs.iter().map(|vec| vec.as_slice()).collect();
    // Identify the ranges in the transcript that contain the only data we want to reveal later
    let (recv_private_ranges, recv_public_ranges) = find_ranges(
        prover.recv_transcript().data(),
        reveal_slices.as_slice(),
    );
    log!("!@# 15");

    let builder = prover.commitment_builder();

    // Commit to the outbound and inbound transcript, isolating the data that contain secrets
    let sent_pub_commitment_ids = sent_public_ranges
        .iter()
        .map(|range| builder.commit_sent(range.clone()).unwrap())
        .collect::<Vec<_>>();

    sent_private_ranges.iter().for_each(|range| {
        builder.commit_sent(range.clone()).unwrap();
    });

    let recv_pub_commitment_ids = recv_public_ranges
        .iter()
        .map(|range| builder.commit_recv(range.clone()).unwrap())
        .collect::<Vec<_>>();

    recv_private_ranges.iter().for_each(|range| {
        builder.commit_recv(range.clone()).unwrap();
    });

    // Finalize, returning the notarized session
    let notarized_session = prover.finalize().await.unwrap();

    log!("Notarization complete!");

    // Create a proof for all committed data in this session
    let session_proof = notarized_session.session_proof();

    let mut proof_builder = notarized_session.data().build_substrings_proof();

    // Reveal everything except the redacted stuff (which for the response it's everything except the screen_name)
    sent_pub_commitment_ids
        .iter()
        .chain(recv_pub_commitment_ids.iter())
        .for_each(|id| {
            proof_builder.reveal(*id).unwrap();
        });

    let substrings_proof = proof_builder.build().unwrap();

    let proof = TlsProof {
        session: session_proof,
        substrings: substrings_proof,
    };

    let res = serde_json::to_string_pretty(&proof).unwrap();

    let duration = start_time.elapsed();
    log!("!@# request took: {} seconds", duration.as_secs());

    Ok(res)

}


/// Find the ranges of the public and private parts of a sequence.
///
/// Returns a tuple of `(public, private)` ranges.
fn find_ranges(seq: &[u8], private_seq: &[&[u8]]) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut private_ranges = Vec::new();
    for s in private_seq {
        for (idx, w) in seq.windows(s.len()).enumerate() {
            if w == *s {
                private_ranges.push(idx..(idx + w.len()));
            }
        }
    }

    let mut sorted_ranges = private_ranges.clone();
    sorted_ranges.sort_by_key(|r| r.start);

    let mut public_ranges = Vec::new();
    let mut last_end = 0;
    for r in sorted_ranges {
        if r.start > last_end {
            public_ranges.push(last_end..r.start);
        }
        last_end = r.end;
    }

    if last_end < seq.len() {
        public_ranges.push(last_end..seq.len());
    }

    (public_ranges, private_ranges)
}
