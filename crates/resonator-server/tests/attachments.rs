//! End-to-end tests of attachments: upload, download, thumbnails and
//! metadata over HTTP against the real server, and the store's own
//! interface (the cases of `AttachmentStoreTestBase` of Apache Wave).

mod common;
#[path = "common/net.rs"]
mod net;
#[path = "common/waves.rs"]
mod waves;

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use resonator_model::id::ParticipantId;
use resonator_model::proto::attachment::AttachmentsResponse;
use resonator_model::proto::json::from_json;
use resonator_server::attachment::{AttachmentError, AttachmentId, AttachmentStore};

use common::{DOMAIN, participant};
use net::TestServer;
use waves::{create_wave, root_name, wave_id};

const WAVE_REF: &str = "example.com/w+abc/~/conv+root";

fn user(name: &str) -> ParticipantId {
    participant(&format!("{name}@{DOMAIN}"))
}

/// A server with Alice's wave `w+abc`, which Bob is on and Carol is not.
async fn setup(
    configure: impl FnOnce(&mut resonator_server::config::Config),
) -> (TestServer, String, String, String) {
    let server = TestServer::start_with(configure).await;
    let alice = server.user("alice").await;
    let bob = server.user("bob").await;
    let carol = server.user("carol").await;
    create_wave(
        server.app().server(),
        &user("alice"),
        "w+abc",
        &[&user("bob")],
        "A wave",
    )
    .await;
    (server, alice, bob, carol)
}

fn form(id: Option<&str>, wave_ref: Option<&str>, file: Option<(&str, Vec<u8>)>) -> Form {
    let mut form = Form::new();
    if let Some(id) = id {
        form = form.text("attachmentId", id.to_owned());
    }
    if let Some(wave_ref) = wave_ref {
        form = form.text("waveRef", wave_ref.to_owned());
    }
    if let Some((name, bytes)) = file {
        form = form.part(
            "uploadFormElement",
            Part::bytes(bytes).file_name(name.to_owned()),
        );
    }
    form
}

async fn post(server: &TestServer, cookie: Option<&str>, form: Form) -> reqwest::Response {
    let mut request = server
        .http
        .post(server.url("/attachment/upload"))
        .multipart(form);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.send().await.expect("upload request")
}

async fn upload(
    server: &TestServer,
    cookie: &str,
    id: &str,
    name: &str,
    bytes: &[u8],
) -> reqwest::Response {
    post(
        server,
        Some(cookie),
        form(Some(id), Some(WAVE_REF), Some((name, bytes.to_vec()))),
    )
    .await
}

async fn get_with(
    server: &TestServer,
    path: &str,
    cookie: &str,
    headers: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = server.http.get(server.url(path)).header("Cookie", cookie);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.expect("get request")
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("no {name} header"))
        .to_str()
        .unwrap()
}

async fn info(server: &TestServer, cookie: Option<&str>, ids: &str) -> AttachmentsResponse {
    let mut request = server
        .http
        .get(server.url("/attachmentsInfo"))
        .query(&[("attachmentIds", ids)]);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let response = request.send().await.expect("info request");
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "application/json");
    from_json(&response.text().await.unwrap()).unwrap()
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            (x % 256) as u8,
            (y % 256) as u8,
            128,
            if x < width / 2 { 255 } else { 0 },
        ])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode png");
    bytes
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A tiny, well-formed start of a PNG that claims to be `width` by
/// `height` pixels.
fn png_claiming(width: u32, height: u32) -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits per channel, RGBA, deflate, no filter, no interlace.
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header, &mut bytes);
    // An empty zlib stream.
    chunk(
        b"IDAT",
        &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        &mut bytes,
    );
    chunk(b"IEND", &[], &mut bytes);
    bytes
}

fn files_under(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn attachments_dir(server: &TestServer) -> PathBuf {
    server.dir.path().join("attachments")
}

#[tokio::test]
async fn upload_download_thumbnail_and_info_round_trip() {
    let (server, alice, bob, _) = setup(|_| {}).await;
    let picture = png(400, 200);
    let created = upload(
        &server,
        &alice,
        "example.com/att+pic",
        "C:\\photos\\holiday pic.png",
        &picture,
    )
    .await;
    assert_eq!(created.status(), 201);
    assert_eq!(created.text().await.unwrap(), "OK");
    let notes = "plain notes \u{2603}\n".repeat(100).into_bytes();
    assert_eq!(
        upload(&server, &bob, "att+notes", "notes.txt", &notes)
            .await
            .status(),
        201
    );

    // Download, by a participant other than the uploader.
    let response = server
        .get("/attachment/example.com/att+pic", Some(&bob))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "image/png");
    assert_eq!(
        header(&response, "content-length"),
        picture.len().to_string()
    );
    assert_eq!(
        header(&response, "content-disposition"),
        "attachment; filename=\"holiday pic.png\"; filename*=UTF-8''holiday%20pic.png"
    );
    assert_eq!(header(&response, "x-content-type-options"), "nosniff");
    assert!(header(&response, "content-security-policy").starts_with("sandbox"));
    assert_eq!(header(&response, "accept-ranges"), "bytes");
    assert_eq!(header(&response, "cache-control"), "private, no-cache");
    assert!(header(&response, "etag").starts_with('"'));
    assert_eq!(response.bytes().await.unwrap(), picture);
    let response = server.get("/attachment/att+notes", Some(&alice)).await;
    assert_eq!(
        header(&response, "content-type"),
        "text/plain; charset=utf-8"
    );
    assert_eq!(response.bytes().await.unwrap(), notes);

    // HEAD has the headers and no body.
    let head = server
        .http
        .head(server.url("/attachment/att+notes"))
        .header("Cookie", &alice)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(header(&head, "content-length"), notes.len().to_string());
    assert!(head.bytes().await.unwrap().is_empty());

    // The thumbnail of the image: a JPEG of at most 200 by 200.
    let response = server
        .get("/thumbnail/example.com/att+pic", Some(&bob))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "image/jpeg");
    assert_eq!(header(&response, "x-content-type-options"), "nosniff");
    let thumbnail =
        image::load_from_memory(&response.bytes().await.unwrap()).expect("thumbnail decodes");
    assert_eq!((thumbnail.width(), thumbnail.height()), (200, 100));
    // Transparent parts are black, as the original paints them.
    let rgb = thumbnail.to_rgb8();
    let dark = rgb.get_pixel(180, 50);
    assert!(dark.0.iter().all(|&channel| channel < 40), "{dark:?}");
    assert!(rgb.get_pixel(20, 50).0[2] > 90);
    // Anything else gets a stand-in picture for its kind.
    let response = server.get("/thumbnail/att+notes", Some(&bob)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "image/svg+xml");
    assert!(header(&response, "content-security-policy").starts_with("sandbox"));
    let svg = response.text().await.unwrap();
    assert!(svg.starts_with("<svg ") && svg.contains("TXT"));

    // Metadata, in the protocol's JSON form.
    let response = server
        .get(
            "/attachmentsInfo?attachmentIds=example.com/att%2Bpic,att%2Bnotes,att%2Bmissing,,a/b/c",
            Some(&bob),
        )
        .await;
    let text = response.text().await.unwrap();
    assert!(text.starts_with("{\"1\":[{\"1\":\"example.com/att+pic\",\"2\":\"example.com/w+abc/~/conv+root\",\"3\":\"holiday pic.png\",\"4\":\"image/png\",\"5\":["), "{text}");
    let listed: AttachmentsResponse = from_json(&text).unwrap();
    assert_eq!(listed.attachment.len(), 2);
    let image_info = &listed.attachment[0];
    assert_eq!(
        image_info.attachment_id.as_deref(),
        Some("example.com/att+pic")
    );
    assert_eq!(image_info.wave_ref.as_deref(), Some(WAVE_REF));
    assert_eq!(image_info.file_name.as_deref(), Some("holiday pic.png"));
    assert_eq!(image_info.mime_type.as_deref(), Some("image/png"));
    assert_eq!(image_info.size, Some(picture.len() as i64));
    assert_eq!(image_info.creator.as_deref(), Some("alice@example.com"));
    assert_eq!(
        image_info.attachment_url.as_deref(),
        Some("/attachment/example.com/att+pic")
    );
    assert_eq!(
        image_info.thumbnail_url.as_deref(),
        Some("/thumbnail/example.com/att+pic")
    );
    let size = |metadata: &Option<resonator_model::proto::attachment::ImageMetadata>| {
        metadata
            .as_ref()
            .map(|m| (m.width.unwrap(), m.height.unwrap()))
    };
    assert_eq!(size(&image_info.image_metadata), Some((400, 200)));
    assert_eq!(size(&image_info.thumbnail_metadata), Some((200, 100)));
    assert_eq!(image_info.malware, None);
    let notes_info = &listed.attachment[1];
    assert_eq!(notes_info.attachment_id.as_deref(), Some("att+notes"));
    assert_eq!(notes_info.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(notes_info.creator.as_deref(), Some("bob@example.com"));
    assert_eq!(size(&notes_info.image_metadata), None);
    // The size of the stand-in picture, as in the original.
    assert_eq!(size(&notes_info.thumbnail_metadata), Some((95, 60)));
    // Without the parameter: not found, as in the original.
    assert_eq!(
        server.get("/attachmentsInfo", Some(&bob)).await.status(),
        404
    );

    // Larger than the body limit of the other endpoints, within that of
    // attachments; streamed both ways.
    let large: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    assert_eq!(
        upload(&server, &alice, "att+large", "large.bin", &large)
            .await
            .status(),
        201
    );
    let response = server.get("/attachment/att+large", Some(&bob)).await;
    assert_eq!(
        header(&response, "content-type"),
        "application/octet-stream"
    );
    assert_eq!(response.bytes().await.unwrap(), large);

    // An id is used once.
    let again = upload(&server, &bob, "att+notes", "other.txt", b"other").await;
    assert_eq!(again.status(), 409);
    let response = server.get("/attachment/att+notes", Some(&alice)).await;
    assert_eq!(response.bytes().await.unwrap(), notes);

    // Attachments outlive a restart.
    let dir = server.stop().await;
    let server = TestServer::start_in(dir, |_| {}).await;
    let alice = server.user("alice").await;
    let response = server
        .get("/attachment/example.com/att+pic", Some(&alice))
        .await;
    assert_eq!(response.bytes().await.unwrap(), picture);
    assert_eq!(
        info(&server, Some(&alice), "att+notes")
            .await
            .attachment
            .len(),
        1
    );
    server.shutdown().await;
}

#[tokio::test]
async fn access_is_that_of_the_wavelet() {
    let (server, alice, bob, carol) = setup(|_| {}).await;
    assert_eq!(
        upload(&server, &alice, "att+1", "a.txt", b"for participants")
            .await
            .status(),
        201
    );

    // Carol is not on the wave; nobody is not signed in.
    for path in ["/attachment/att+1", "/thumbnail/att+1"] {
        assert_eq!(server.get(path, Some(&bob)).await.status(), 200, "{path}");
        assert_eq!(server.get(path, Some(&carol)).await.status(), 403, "{path}");
        assert_eq!(server.get(path, None).await.status(), 403, "{path}");
    }
    // Without a session nothing is said about what exists.
    assert_eq!(
        server.get("/attachment/att+nothing", None).await.status(),
        403
    );
    assert_eq!(
        server
            .get("/attachment/att+nothing", Some(&carol))
            .await
            .status(),
        404
    );
    assert_eq!(info(&server, Some(&bob), "att+1").await.attachment.len(), 1);
    assert!(
        info(&server, Some(&carol), "att+1")
            .await
            .attachment
            .is_empty()
    );
    assert!(info(&server, None, "att+1").await.attachment.is_empty());

    // Uploading takes access to the wavelet too.
    let denied = upload(&server, &carol, "att+carol", "c.txt", b"x").await;
    assert_eq!(denied.status(), 403);
    let anonymous = post(
        &server,
        None,
        form(
            Some("att+anon"),
            Some(WAVE_REF),
            Some(("a.txt", b"x".to_vec())),
        ),
    )
    .await;
    assert_eq!(anonymous.status(), 403);
    // Naming a wave that does not exist is no way in.
    let nowhere = post(
        &server,
        Some(&carol),
        form(
            Some("att+carol"),
            Some("example.com/w+nothing/~/conv+root"),
            Some(("c.txt", b"x".to_vec())),
        ),
    )
    .await;
    assert_eq!(nowhere.status(), 403);
    // Nor is another user's data wavelet.
    let udw = post(
        &server,
        Some(&bob),
        form(
            Some("att+udw"),
            Some("example.com/w+abc/~/user+alice@example.com"),
            Some(("c.txt", b"x".to_vec())),
        ),
    )
    .await;
    assert_eq!(udw.status(), 403);
    for id in ["att+carol", "att+anon", "att+udw"] {
        assert_eq!(
            server
                .get(&format!("/attachment/{id}"), Some(&alice))
                .await
                .status(),
            404,
            "{id}"
        );
    }
    // Nothing is left of the refused uploads.
    assert_eq!(files_under(&attachments_dir(&server)).len(), 1);

    // Tokens: reading takes the read scope, uploading the write scope.
    let (read, _) = server.create_token(&alice, "reader", &["read"]).await;
    let (write, _) = server.create_token(&alice, "writer", &["write"]).await;
    assert_eq!(
        server.get_bearer("/attachment/att+1", &read).await.status(),
        200
    );
    assert_eq!(
        server
            .get_bearer("/attachment/att+1", &write)
            .await
            .status(),
        403
    );
    assert_eq!(
        server.get_bearer("/thumbnail/att+1", &write).await.status(),
        403
    );
    let by_token = |token: &str, id: &str| {
        server
            .http
            .post(server.url("/attachment/x"))
            .bearer_auth(token)
            .multipart(form(
                Some(id),
                Some(WAVE_REF),
                Some(("t.txt", b"token".to_vec())),
            ))
            .send()
    };
    assert_eq!(by_token(&read, "att+t1").await.unwrap().status(), 403);
    assert_eq!(by_token(&write, "att+t2").await.unwrap().status(), 201);

    // A cross-site form post riding on the session cookie is refused.
    let cross_site = server
        .http
        .post(server.url("/attachment/x"))
        .header("Cookie", &alice)
        .header("Origin", "https://evil.example")
        .multipart(form(
            Some("att+csrf"),
            Some(WAVE_REF),
            Some(("x.txt", b"x".to_vec())),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_site.status(), 403);

    // Bob is removed from the wave and loses the attachment with it.
    let waves = server.app().server();
    let name = root_name(&wave_id("w+abc"));
    let snapshot = waves::wavelet(waves, &name).await;
    resonator_server::waveserver::WaveletProvider::submit_request(
        waves,
        &name,
        common::delta(
            &user("alice"),
            snapshot.hashed_version(),
            vec![common::remove_participant(&user("alice"), &user("bob"))],
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        server.get("/attachment/att+1", Some(&bob)).await.status(),
        403
    );
    assert!(
        info(&server, Some(&bob), "att+1")
            .await
            .attachment
            .is_empty()
    );
    assert_eq!(
        server.get("/attachment/att+1", Some(&alice)).await.status(),
        200
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ids_cannot_reach_outside_the_store() {
    let (server, alice, _, _) = setup(|_| {}).await;
    let marker = server.dir.path().join("marker.txt");
    std::fs::write(&marker, "untouched").unwrap();
    // Ids that are no ids.
    for id in [
        "../../marker.txt",
        "a/b/c",
        "example.com/../x",
        "",
        "/etc/passwd",
        "a\nb",
    ] {
        let response = post(
            &server,
            Some(&alice),
            form(Some(id), Some(WAVE_REF), Some(("x.txt", b"x".to_vec()))),
        )
        .await;
        assert_eq!(response.status(), 400, "{id:?}");
    }
    for path in [
        "/attachment/../../marker.txt",
        "/attachment/..%2F..%2Fmarker.txt",
        "/attachment/a/b/c",
        "/attachment/%2e%2e/%2e%2e/marker.txt",
        "/thumbnail/..%2F..%2Fresonator.db",
        "/attachment/",
    ] {
        let status = get_with(&server, path, &alice, &[]).await.status();
        assert!(status == 404 || status == 405, "{path}: {status}");
    }
    // Ids that look like paths but are ids: stored under their hash.
    for (index, id) in [
        "..",
        ".",
        "example.com/..",
        "..%2f..%2fmarker.txt",
        "marker.txt",
        "~",
        "con",
    ]
    .into_iter()
    .enumerate()
    {
        let content = format!("content {index}");
        let response = upload(&server, &alice, id, "../../evil.txt", content.as_bytes()).await;
        assert_eq!(response.status(), 201, "{id:?}");
        let listed = info(&server, Some(&alice), id).await;
        assert_eq!(listed.attachment.len(), 1, "{id:?}");
        assert_eq!(listed.attachment[0].attachment_id.as_deref(), Some(id));
        // The file name is only a name.
        assert_eq!(listed.attachment[0].file_name.as_deref(), Some("evil.txt"));
        let stored = resonator_server::attachment::AttachmentStore::read(
            &server.app().state().attachments,
            &AttachmentId::deserialise(id).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(stored.as_deref(), Some(content.as_bytes()), "{id:?}");
    }
    // Every file is where the store puts it, named by a hash.
    let directory = attachments_dir(&server);
    let files = files_under(&directory);
    assert_eq!(files.len(), 7);
    for file in &files {
        let relative = file.strip_prefix(&directory).unwrap();
        let parts: Vec<String> = relative
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        assert_eq!(parts.len(), 3, "{relative:?}");
        assert_eq!(parts[0], "files");
        assert_eq!(parts[2].len(), 64);
        assert!(parts[2].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(parts[1], parts[2][..2]);
    }
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "untouched");
    // Nothing but the database and the store in the data directory.
    let mut top: Vec<String> = std::fs::read_dir(server.dir.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("resonator.db"))
        .collect();
    top.sort();
    assert_eq!(top, ["attachments", "marker.txt"]);
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_and_malformed_uploads_are_rejected() {
    let (server, alice, _, _) = setup(|config| config.attachment_max_size = 10_000).await;
    // At the limit.
    assert_eq!(
        upload(&server, &alice, "att+fits", "fits.bin", &vec![7u8; 10_000])
            .await
            .status(),
        201
    );
    // Just over it.
    let over = upload(&server, &alice, "att+over", "over.bin", &vec![7u8; 10_001]).await;
    assert_eq!(over.status(), 413);
    // Far over it: cut off by the body limit, however it ends.
    let far = server
        .http
        .post(server.url("/attachment/x"))
        .header("Cookie", &alice)
        .multipart(form(
            Some("att+far"),
            Some(WAVE_REF),
            Some(("far.bin", vec![7u8; 2_000_000])),
        ))
        .send()
        .await;
    match far {
        Ok(response) => assert_eq!(response.status(), 413),
        // The server may close the connection while the client still sends.
        Err(error) => assert!(
            error.is_request() || error.is_body() || error.is_connect(),
            "{error}"
        ),
    }
    for id in ["att+over", "att+far"] {
        assert_eq!(
            server
                .get(&format!("/attachment/{id}"), Some(&alice))
                .await
                .status(),
            404,
            "{id}"
        );
    }
    // Missing parts.
    let file = || Some(("x.txt", b"x".to_vec()));
    let no_id = post(&server, Some(&alice), form(None, Some(WAVE_REF), file())).await;
    assert_eq!(no_id.status(), 400);
    assert!(
        no_id
            .text()
            .await
            .unwrap()
            .contains("No attachment Id in the request.")
    );
    let no_wave = post(&server, Some(&alice), form(Some("att+x"), None, file())).await;
    assert_eq!(no_wave.status(), 400);
    assert!(
        no_wave
            .text()
            .await
            .unwrap()
            .contains("No wave reference in request.")
    );
    let bad_wave = post(
        &server,
        Some(&alice),
        form(Some("att+x"), Some("not a waveref"), file()),
    )
    .await;
    assert_eq!(bad_wave.status(), 400);
    let no_file = post(
        &server,
        Some(&alice),
        form(Some("att+x"), Some(WAVE_REF), None),
    )
    .await;
    assert_eq!(no_file.status(), 400);
    // Not a multipart form at all.
    let plain = server
        .http
        .post(server.url("/attachment/x"))
        .header("Cookie", &alice)
        .header("Content-Type", "application/octet-stream")
        .body("just bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), 415);
    // Only the one accepted upload left anything behind.
    let files = files_under(&attachments_dir(&server));
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(server.get("/attachment/att+x", Some(&alice)).await.status() == 404);
    server.shutdown().await;
}

#[tokio::test]
async fn active_content_cannot_run_in_the_servers_origin() {
    let (server, alice, _, _) = setup(|_| {}).await;
    let page = b"<!doctype html><script>alert(document.cookie)</script>".to_vec();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"><script>alert(2)</script></svg>".to_vec();
    let uploads: [(&str, &str, &[u8], &str); 7] = [
        ("att+html", "evil.html", &page, "text/html"),
        ("att+htm", "evil.HTM", &page, "text/html"),
        ("att+svg", "evil.svg", &svg, "image/svg+xml"),
        ("att+js", "evil.js", b"alert(1)", "text/javascript"),
        (
            "att+xml",
            "evil.xml",
            b"<?xml version=\"1.0\"?><x/>",
            "application/xml",
        ),
        ("att+xhtml", "evil.xhtml", &page, "application/xhtml+xml"),
        // HTML under an innocent name: served as what the name says, never sniffed.
        ("att+disguised", "holiday.png", &page, "image/png"),
    ];
    for (id, name, bytes, mime) in uploads {
        assert_eq!(
            upload(&server, &alice, id, name, bytes).await.status(),
            201,
            "{id}"
        );
        let listed = info(&server, Some(&alice), id).await;
        // The metadata says what the file is ...
        assert_eq!(
            listed.attachment[0].mime_type.as_deref(),
            Some(mime),
            "{id}"
        );
        // ... and a damaged "image" has no image metadata.
        assert!(listed.attachment[0].image_metadata.is_none(), "{id}");
        let response = server.get(&format!("/attachment/{id}"), Some(&alice)).await;
        assert_eq!(response.status(), 200, "{id}");
        let expected_type = if id == "att+disguised" {
            "image/png"
        } else {
            "application/octet-stream"
        };
        assert_eq!(header(&response, "content-type"), expected_type, "{id}");
        assert!(
            header(&response, "content-disposition").starts_with("attachment; filename=\""),
            "{id}"
        );
        assert_eq!(
            header(&response, "x-content-type-options"),
            "nosniff",
            "{id}"
        );
        let policy = header(&response, "content-security-policy");
        assert!(
            policy.starts_with("sandbox;") && policy.contains("default-src 'none'"),
            "{id}: {policy}"
        );
        assert_eq!(header(&response, "x-frame-options"), "DENY", "{id}");
        assert_eq!(
            response.bytes().await.unwrap(),
            bytes,
            "{id}: bytes are served as stored"
        );
        // The thumbnail is a built-in picture, not the upload.
        let thumbnail = server.get(&format!("/thumbnail/{id}"), Some(&alice)).await;
        assert_eq!(header(&thumbnail, "content-type"), "image/svg+xml", "{id}");
        let picture = thumbnail.text().await.unwrap();
        assert!(
            !picture.contains("script") && !picture.contains("onload"),
            "{id}"
        );
    }
    // A file name cannot inject headers or break out of the quoted string.
    let hostile = "x\"; filename=\"evil.html\r\nSet-Cookie: a=b";
    let status = upload(&server, &alice, "att+crlf", hostile, b"x")
        .await
        .status();
    // Refused as a malformed form, or stored under a harmless name.
    assert!(status == 400 || status == 201, "{status}");
    assert_eq!(
        upload(&server, &alice, "att+name", "we\"ird; na\\me.html", b"x")
            .await
            .status(),
        201
    );
    for id in ["att+crlf", "att+name"] {
        let response = server.get(&format!("/attachment/{id}"), Some(&alice)).await;
        assert!(response.headers().get("set-cookie").is_none());
        if response.status() == 200 {
            let disposition = header(&response, "content-disposition");
            let quoted = disposition
                .strip_prefix("attachment; filename=\"")
                .and_then(|rest| rest.split_once("\"; filename*=UTF-8''"))
                .unwrap_or_else(|| panic!("{disposition}"));
            assert!(!quoted.0.contains(['"', '\\', '\r', '\n']), "{disposition}");
            assert!(
                quoted
                    .1
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"%.-_".contains(&b)),
                "{disposition}"
            );
        }
    }
    // The other pages of the server keep their own policy.
    let index = server.get("/", Some(&alice)).await;
    assert!(header(&index, "content-security-policy").starts_with("default-src 'self'"));
    server.shutdown().await;
}

#[tokio::test]
async fn ranges_and_conditional_requests() {
    let (server, alice, _, _) = setup(|_| {}).await;
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        upload(&server, &alice, "att+big", "big.bin", &bytes)
            .await
            .status(),
        201
    );
    let path = "/attachment/att+big";
    let whole = get_with(&server, path, &alice, &[]).await;
    let etag = header(&whole, "etag").to_owned();
    assert_eq!(whole.bytes().await.unwrap(), bytes);

    let cases: [(&str, u64, u64); 5] = [
        ("bytes=0-99", 0, 99),
        ("bytes=500-999", 500, 999),
        ("bytes=99990-", 99_990, 99_999),
        ("bytes=-10", 99_990, 99_999),
        ("bytes=99000-200000", 99_000, 99_999),
    ];
    for (range, first, last) in cases {
        let response = get_with(&server, path, &alice, &[("Range", range)]).await;
        assert_eq!(response.status(), 206, "{range}");
        assert_eq!(
            header(&response, "content-range"),
            format!("bytes {first}-{last}/100000"),
            "{range}"
        );
        assert_eq!(
            header(&response, "content-length"),
            (last - first + 1).to_string(),
            "{range}"
        );
        assert_eq!(header(&response, "etag"), etag);
        assert_eq!(
            response.bytes().await.unwrap(),
            bytes[first as usize..=last as usize],
            "{range}"
        );
    }
    let beyond = get_with(&server, path, &alice, &[("Range", "bytes=100000-")]).await;
    assert_eq!(beyond.status(), 416);
    assert_eq!(header(&beyond, "content-range"), "bytes */100000");
    // What is not understood is answered in full.
    for range in ["bytes=0-1,5-6", "lines=1-2", "bytes=9-1"] {
        let response = get_with(&server, path, &alice, &[("Range", range)]).await;
        assert_eq!(response.status(), 200, "{range}");
        assert_eq!(
            response.bytes().await.unwrap().len(),
            bytes.len(),
            "{range}"
        );
    }
    // If-Range: the range counts only for the content the client has.
    let current = get_with(
        &server,
        path,
        &alice,
        &[("Range", "bytes=0-9"), ("If-Range", &etag)],
    )
    .await;
    assert_eq!(current.status(), 206);
    let stale = get_with(
        &server,
        path,
        &alice,
        &[("Range", "bytes=0-9"), ("If-Range", "\"other\"")],
    )
    .await;
    assert_eq!(stale.status(), 200);
    assert_eq!(stale.bytes().await.unwrap().len(), bytes.len());

    // Conditional requests.
    let unchanged = get_with(&server, path, &alice, &[("If-None-Match", &etag)]).await;
    assert_eq!(unchanged.status(), 304);
    assert_eq!(header(&unchanged, "etag"), etag);
    assert!(unchanged.bytes().await.unwrap().is_empty());
    let weak = get_with(
        &server,
        path,
        &alice,
        &[("If-None-Match", &format!("\"x\", W/{etag}"))],
    )
    .await;
    assert_eq!(weak.status(), 304);
    let changed = get_with(
        &server,
        path,
        &alice,
        &[("If-None-Match", "\"something else\"")],
    )
    .await;
    assert_eq!(changed.status(), 200);
    // The access check comes first, whatever the client has cached.
    let carol = server.user("carol").await;
    let denied = get_with(&server, path, &carol, &[("If-None-Match", &etag)]).await;
    assert_eq!(denied.status(), 403);
    // Thumbnails are conditional too.
    let thumbnail = get_with(&server, "/thumbnail/att+big", &alice, &[]).await;
    let thumbnail_etag = header(&thumbnail, "etag").to_owned();
    assert_ne!(thumbnail_etag, etag);
    let unchanged = get_with(
        &server,
        "/thumbnail/att+big",
        &alice,
        &[("If-None-Match", &thumbnail_etag)],
    )
    .await;
    assert_eq!(unchanged.status(), 304);
    server.shutdown().await;
}

#[tokio::test]
async fn image_bombs_and_broken_images_get_no_thumbnail() {
    let (server, alice, _, _) = setup(|_| {}).await;
    let uploads: [(&str, &str, Vec<u8>); 5] = [
        // A few dozen bytes claiming 3.6 gigapixels.
        ("att+bomb", "bomb.png", png_claiming(60_000, 60_000)),
        // Sides within bounds, too many pixels.
        ("att+wide", "wide.png", png_claiming(12_000, 12_000)),
        ("att+zero", "zero.png", png_claiming(0, 10)),
        // Small enough, but the pixel data is missing.
        ("att+cut", "cut.png", png_claiming(64, 64)),
        (
            "att+noise",
            "noise.jpg",
            (0..5000u32).map(|i| (i * 7919 % 256) as u8).collect(),
        ),
    ];
    for (id, name, bytes) in &uploads {
        let started = std::time::Instant::now();
        assert_eq!(
            upload(&server, &alice, id, name, bytes).await.status(),
            201,
            "{id}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{id} took {:?}",
            started.elapsed()
        );
        let listed = info(&server, Some(&alice), id).await;
        let metadata = &listed.attachment[0];
        assert!(metadata.image_metadata.is_none(), "{id}");
        let thumbnail = metadata.thumbnail_metadata.as_ref().unwrap();
        assert_eq!(
            (thumbnail.width, thumbnail.height),
            (Some(95), Some(60)),
            "{id}"
        );
        // The bytes are kept and served; the thumbnail is the stand-in.
        let response = server.get(&format!("/attachment/{id}"), Some(&alice)).await;
        assert_eq!(&response.bytes().await.unwrap()[..], &bytes[..], "{id}");
        let response = server.get(&format!("/thumbnail/{id}"), Some(&alice)).await;
        assert_eq!(header(&response, "content-type"), "image/svg+xml", "{id}");
    }
    // No thumbnail files were written.
    let files = files_under(&attachments_dir(&server));
    assert_eq!(files.len(), uploads.len());
    assert!(
        files.iter().all(|file| file.extension().is_none()),
        "{files:?}"
    );

    // Real images of each supported format still get one.
    let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(300, 600, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 200])
    }));
    for (id, name, format) in [
        ("att+png", "p.png", image::ImageFormat::Png),
        ("att+jpeg", "p.jpeg", image::ImageFormat::Jpeg),
        ("att+gif", "p.gif", image::ImageFormat::Gif),
        ("att+webp", "p.webp", image::ImageFormat::WebP),
        // The content decides, not the name.
        ("att+misnamed", "p.txt", image::ImageFormat::Png),
    ] {
        let mut bytes = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut bytes), format)
            .expect("encode");
        assert_eq!(
            upload(&server, &alice, id, name, &bytes).await.status(),
            201,
            "{id}"
        );
        let listed = info(&server, Some(&alice), id).await;
        let metadata = &listed.attachment[0];
        let image_size = metadata.image_metadata.as_ref().unwrap();
        assert_eq!(
            (image_size.width, image_size.height),
            (Some(300), Some(600)),
            "{id}"
        );
        let thumbnail = metadata.thumbnail_metadata.as_ref().unwrap();
        assert_eq!(
            (thumbnail.width, thumbnail.height),
            (Some(100), Some(200)),
            "{id}"
        );
        let response = server.get(&format!("/thumbnail/{id}"), Some(&alice)).await;
        assert_eq!(header(&response, "content-type"), "image/jpeg", "{id}");
        let decoded = image::load_from_memory(&response.bytes().await.unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (100, 200), "{id}");
    }
    server.shutdown().await;
}

// ---- The store's own interface (AttachmentStoreTestBase) ----

#[tokio::test]
async fn store_interface_keeps_reads_and_deletes_bytes() {
    let (server, _, _, _) = setup(|config| config.attachment_max_size = 1000).await;
    let store = &server.app().state().attachments;
    let name = root_name(&wave_id("w+abc"));
    let alice = user("alice");
    let id = |text: &str| AttachmentId::deserialise(text).unwrap();

    // testStoreReturnsNullForNonexistantId
    assert!(
        AttachmentStore::read(store, &id("some_madeup_id"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        AttachmentStore::metadata(store, &id("some_madeup_id"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .open_file(&id("some_madeup_id"))
            .await
            .unwrap()
            .is_none()
    );
    // testStoreCanStoreData, testContentLengthMatchesDataSize
    let data = b"some file data";
    let metadata =
        AttachmentStore::store(store, &id("id_1"), &name, "data.txt", Some(&alice), data)
            .await
            .unwrap();
    assert_eq!(metadata.size, data.len() as u64);
    assert_eq!(metadata.wave_ref, WAVE_REF);
    assert_eq!(metadata.wavelet_name(), Some(name.clone()));
    assert_eq!(metadata.creator, "alice@example.com");
    assert_eq!(
        AttachmentStore::read(store, &id("id_1"))
            .await
            .unwrap()
            .as_deref(),
        Some(&data[..])
    );
    let (_, length) = store.open_file(&id("id_1")).await.unwrap().unwrap();
    assert_eq!(length, data.len() as u64);
    assert_eq!(
        AttachmentStore::metadata(store, &id("id_1")).await.unwrap(),
        Some(metadata)
    );
    // testGetStreamReturnsNewStream: every read starts at the beginning.
    assert_eq!(
        AttachmentStore::read(store, &id("id_1"))
            .await
            .unwrap()
            .as_deref(),
        Some(&data[..])
    );
    // An id is stored once; the first content stays.
    let again =
        AttachmentStore::store(store, &id("id_1"), &name, "other.txt", None, b"other").await;
    assert!(matches!(again, Err(AttachmentError::AlreadyExists(_))));
    assert_eq!(
        AttachmentStore::read(store, &id("id_1"))
            .await
            .unwrap()
            .as_deref(),
        Some(&data[..])
    );
    // Too large.
    let large =
        AttachmentStore::store(store, &id("id_big"), &name, "big", None, &[0u8; 1001]).await;
    assert!(matches!(
        large,
        Err(AttachmentError::TooLarge { max: 1000 })
    ));
    assert!(
        AttachmentStore::metadata(store, &id("id_big"))
            .await
            .unwrap()
            .is_none()
    );
    // An image, stored through the interface, gets its thumbnail.
    let with_picture = AttachmentStore::store(
        store,
        &id("example.com/pic"),
        &name,
        "tiny.png",
        None,
        &png(8, 4),
    )
    .await
    .unwrap();
    assert_eq!(with_picture.creator, "");
    assert_eq!(
        with_picture.image.map(|size| (size.width, size.height)),
        Some((8, 4))
    );
    assert!(
        store
            .open_thumbnail(&id("example.com/pic"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(store.list_for_wavelet(&name).await.unwrap().len(), 2);
    // testStoreCanDeleteData
    assert!(
        AttachmentStore::delete(store, &id("example.com/pic"))
            .await
            .unwrap()
    );
    assert!(
        !AttachmentStore::delete(store, &id("example.com/pic"))
            .await
            .unwrap()
    );
    assert!(
        AttachmentStore::read(store, &id("example.com/pic"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .open_thumbnail(&id("example.com/pic"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(files_under(&attachments_dir(&server)).len(), 1);
    // The id is free again.
    AttachmentStore::store(
        store,
        &id("example.com/pic"),
        &name,
        "again.txt",
        None,
        b"again",
    )
    .await
    .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn files_nothing_owns_are_swept() {
    let (server, alice, _, _) = setup(|_| {}).await;
    assert_eq!(
        upload(&server, &alice, "att+kept", "kept.txt", b"kept")
            .await
            .status(),
        201
    );
    assert_eq!(
        upload(&server, &alice, "att+pic", "pic.png", &png(10, 10))
            .await
            .status(),
        201
    );
    let store = server.app().state().attachments.clone();
    let directory = attachments_dir(&server);
    let owned = files_under(&directory);
    assert_eq!(owned.len(), 3);

    // What a crash can leave: a half-written upload, a file whose row was
    // never committed, and a thumbnail of one.
    let shard = directory.join("files").join("zz");
    std::fs::create_dir_all(&shard).unwrap();
    let orphan = shard.join("f".repeat(64));
    std::fs::write(&orphan, "orphan").unwrap();
    std::fs::write(shard.join(format!("{}.thumb", "f".repeat(64))), "orphan").unwrap();
    std::fs::write(directory.join("tmp").join("abandoned"), "partial").unwrap();
    // An upload in progress is nobody's orphan.
    let mut in_progress = store.begin_upload().await.unwrap();
    in_progress.write(b"still coming").await.unwrap();
    assert_eq!(files_under(&directory).len(), 7);

    // Young files are left alone: they may be uploads about to finish.
    let report = store.sweep(Duration::from_secs(3600)).await.unwrap();
    assert_eq!((report.temporary_files, report.orphans), (0, 0));
    assert_eq!(files_under(&directory).len(), 7);
    // Old enough, they go; what has a row stays.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    in_progress.write(b" ...").await.unwrap();
    let report = store.sweep(Duration::from_secs(1)).await.unwrap();
    assert_eq!((report.temporary_files, report.orphans), (1, 2));
    let mut left = files_under(&directory);
    assert_eq!(left.len(), 4);
    left.retain(|file| !file.starts_with(directory.join("tmp")));
    assert_eq!(left, owned);
    // An abandoned upload removes its file.
    drop(in_progress);
    assert_eq!(files_under(&directory), owned);
    let response = server.get("/attachment/att+kept", Some(&alice)).await;
    assert_eq!(response.text().await.unwrap(), "kept");

    // At start-up whatever is in tmp is from a previous run.
    std::fs::write(directory.join("tmp").join("leftover"), "partial").unwrap();
    let dir = server.stop().await;
    let server = TestServer::start_in(dir, |_| {}).await;
    assert_eq!(files_under(&attachments_dir(&server)), owned);
    server.shutdown().await;
}

#[tokio::test]
async fn attachments_directory_is_configurable_and_must_be_usable() {
    let elsewhere = tempfile::tempdir().unwrap();
    let directory = elsewhere.path().join("store");
    let configured = directory.clone();
    let (server, alice, _, _) = setup(move |config| config.attachments_dir = configured).await;
    assert_eq!(
        upload(&server, &alice, "att+1", "a.txt", b"x")
            .await
            .status(),
        201
    );
    assert_eq!(files_under(&directory).len(), 1);
    assert!(!server.dir.path().join("attachments").exists());
    server.shutdown().await;

    // A directory that cannot be created fails start-up with a clear error.
    let blocker = elsewhere.path().join("file");
    std::fs::write(&blocker, "not a directory").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = TestServer::config_in(&dir);
    config.attachments_dir = blocker.join("store");
    let error = match resonator_server::app::App::start(&config).await {
        Ok(_) => panic!("started with an unusable attachments directory"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("attachments directory"), "{error}");
}

#[tokio::test]
async fn uploads_count_against_the_uploaders_quota() {
    let (server, alice, bob, _) = setup(|config| config.attachment_user_quota = 3_000).await;
    assert_eq!(
        upload(&server, &alice, "att+q1", "one.bin", &vec![1u8; 2_000])
            .await
            .status(),
        201
    );
    // This one would take her over the quota: refused, nothing stored.
    let over = upload(&server, &alice, "att+q2", "two.bin", &vec![2u8; 2_000]).await;
    assert_eq!(over.status(), 413);
    assert!(over.text().await.unwrap().contains("quota of 3000 bytes"));
    // What is left still fits.
    assert_eq!(
        upload(&server, &alice, "att+q3", "three.bin", &vec![3u8; 1_000])
            .await
            .status(),
        201
    );
    // Used up: refused at once, even a tiny file.
    assert_eq!(
        upload(&server, &alice, "att+q4", "four.bin", &[4u8])
            .await
            .status(),
        413
    );
    // Quotas are per uploader.
    assert_eq!(
        upload(&server, &bob, "att+q5", "five.bin", &vec![5u8; 2_500])
            .await
            .status(),
        201
    );
    server.shutdown().await;
}

// ---- Attachments of other servers (federation's fetch-attachment) ----

mod remote {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use futures_util::future::BoxFuture;
    use resonator_model::id::WaveletName;
    use resonator_server::attachment::{
        AttachmentId, AttachmentOrigin, FetchError, RemoteAttachment, Upload,
    };
    use sha2::{Digest, Sha256};

    use super::*;

    pub const ORIGIN: &str = "other.example";

    /// What the fake origin sends for an id: the bytes, and the hash and
    /// size it announces (normally those of the bytes).
    #[derive(Clone)]
    pub struct Served {
        pub bytes: Vec<u8>,
        pub sha256: [u8; 32],
        pub size: u64,
    }

    impl Served {
        pub fn honest(bytes: Vec<u8>) -> Served {
            Served {
                sha256: Sha256::digest(&bytes).into(),
                size: bytes.len() as u64,
                bytes,
            }
        }
    }

    /// An origin in memory: serves `files` of `wave_ref` while online,
    /// slowly enough for concurrent requests to overlap.
    pub struct FakeOrigin {
        pub files: Mutex<HashMap<String, Served>>,
        pub online: AtomicBool,
        pub fetches: AtomicUsize,
        pub wave_ref: String,
    }

    impl FakeOrigin {
        pub fn new() -> Arc<FakeOrigin> {
            Arc::new(FakeOrigin {
                files: Mutex::new(HashMap::new()),
                online: AtomicBool::new(true),
                fetches: AtomicUsize::new(0),
                wave_ref: WAVE_REF.to_owned(),
            })
        }

        pub fn put(&self, id: &str, served: Served) {
            self.files.lock().unwrap().insert(id.to_owned(), served);
        }
    }

    impl AttachmentOrigin for FakeOrigin {
        fn fetch<'a>(
            &'a self,
            id: &'a AttachmentId,
            wavelet: &'a WaveletName,
            upload: &'a mut Upload,
        ) -> BoxFuture<'a, Result<RemoteAttachment, FetchError>> {
            Box::pin(async move {
                self.fetches.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(150)).await;
                if !self.online.load(Ordering::SeqCst) {
                    return Err(FetchError::Unavailable {
                        domain: ORIGIN.to_owned(),
                        message: "offline".to_owned(),
                    });
                }
                let served = self.files.lock().unwrap().get(&id.serialise()).cloned();
                let Some(served) = served else {
                    return Err(FetchError::NotFound);
                };
                if resonator_server::attachment::wavelet_name_to_wave_ref(wavelet) != self.wave_ref
                {
                    return Err(FetchError::NotFound);
                }
                upload
                    .write(&served.bytes)
                    .await
                    .map_err(|e| FetchError::Rejected(e.to_string()))?;
                Ok(RemoteAttachment {
                    file_name: format!("{}.png", id.id()),
                    size: served.size,
                    creator: format!("bob@{ORIGIN}"),
                    sha256: served.sha256,
                    created_at: 1,
                    origin: ORIGIN.to_owned(),
                })
            })
        }
    }

    pub fn url(kind: &str, id: &str) -> String {
        format!("/{kind}/{id}?waveRef={}", WAVE_REF.replace('+', "%2B"))
    }
}

/// A server whose attachment store fetches from a fake origin.
async fn remote_setup(budget_mb: u64) -> (TestServer, String, String, Arc<remote::FakeOrigin>) {
    let (server, alice, _bob, carol) =
        setup(|config| config.federation.attachment_cache_mb = budget_mb).await;
    let origin = remote::FakeOrigin::new();
    assert!(
        server.app().state().attachments.set_origin(
            Arc::clone(&origin) as Arc<dyn resonator_server::attachment::AttachmentOrigin>
        )
    );
    (server, alice, carol, origin)
}

use std::sync::Arc;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn attachments_of_other_servers_are_fetched_for_readers_of_the_wavelet() {
    let (server, alice, carol, origin) = remote_setup(1024).await;
    let id = format!("{}/picture", remote::ORIGIN);
    let image = png(300, 200);
    origin.put(&id, remote::Served::honest(image.clone()));

    // Not for those who cannot read the wavelet; nothing is fetched.
    let response = get_with(&server, &remote::url("attachment", &id), &carol, &[]).await;
    assert_eq!(response.status(), 403);
    assert_eq!(origin.fetches.load(Ordering::SeqCst), 0);
    // Without the wavelet, an id not here is not found.
    let response = get_with(&server, &format!("/attachment/{id}"), &alice, &[]).await;
    assert_eq!(response.status(), 404);

    let response = get_with(&server, &remote::url("attachment", &id), &alice, &[]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().to_vec(), image);
    let response = get_with(&server, &remote::url("thumbnail", &id), &alice, &[]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "image/jpeg");
    let thumbnail = response.bytes().await.unwrap();
    assert!(thumbnail.len() > 8 && thumbnail.len() != image.len());
    let described = info(&server, Some(&alice), &id).await;
    assert_eq!(described.attachment.len(), 1);
    assert_eq!(described.attachment[0].size, Some(image.len() as i64));
    assert!(described.attachment[0].thumbnail_metadata.is_some());
    // Cached: fetched once, served without the origin from then on.
    assert_eq!(origin.fetches.load(Ordering::SeqCst), 1);
    origin.online.store(false, Ordering::SeqCst);
    let response = get_with(&server, &remote::url("attachment", &id), &alice, &[]).await;
    assert_eq!(response.status(), 200);
    // Carol still cannot read the cached copy.
    let response = get_with(&server, &remote::url("attachment", &id), &carol, &[]).await;
    assert_eq!(response.status(), 403);
    // Not counted against anyone's quota.
    let used = server
        .app()
        .state()
        .attachments
        .usage_of(&user("alice"))
        .await
        .unwrap();
    assert_eq!(used, 0);

    // Nobody here uploads in the origin's name.
    let response = upload(
        &server,
        &alice,
        &format!("{}/mine", remote::ORIGIN),
        "x.txt",
        b"x",
    )
    .await;
    assert_eq!(response.status(), 400);
    server.shutdown().await;
}

#[tokio::test]
async fn fetched_attachments_must_match_their_hash_and_size() {
    let (server, alice, _carol, origin) = remote_setup(1024).await;
    let bytes = b"the real thing".to_vec();
    let wrong_hash = format!("{}/wrong-hash", remote::ORIGIN);
    origin.put(
        &wrong_hash,
        remote::Served {
            sha256: [7; 32],
            ..remote::Served::honest(bytes.clone())
        },
    );
    let wrong_size = format!("{}/wrong-size", remote::ORIGIN);
    origin.put(
        &wrong_size,
        remote::Served {
            size: 3,
            ..remote::Served::honest(bytes.clone())
        },
    );
    for id in [&wrong_hash, &wrong_size] {
        let response = get_with(&server, &remote::url("attachment", id), &alice, &[]).await;
        assert_eq!(response.status(), 502, "{id}");
        let stored = server
            .app()
            .state()
            .attachments
            .metadata(&AttachmentId::deserialise(id).unwrap())
            .await
            .unwrap();
        assert!(stored.is_none(), "{id} kept");
    }
    // Nothing left behind in the temporary directory either.
    let directory = server
        .app()
        .state()
        .attachments
        .settings()
        .directory
        .clone();
    let leftovers = std::fs::read_dir(directory.join("tmp"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn an_unreachable_origin_is_a_503_and_concurrent_requests_share_one_fetch() {
    let (server, alice, _carol, origin) = remote_setup(1024).await;
    let id = format!("{}/shared", remote::ORIGIN);
    origin.put(&id, remote::Served::honest(vec![42; 100_000]));

    origin.online.store(false, Ordering::SeqCst);
    let response = get_with(&server, &remote::url("attachment", &id), &alice, &[]).await;
    assert_eq!(response.status(), 503);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("unavailable from other.example"),
        "names the origin"
    );
    // Not asked again right away.
    let response = get_with(&server, &remote::url("thumbnail", &id), &alice, &[]).await;
    assert_eq!(response.status(), 503);
    assert_eq!(origin.fetches.load(Ordering::SeqCst), 1);

    // A fresh server: eight requests at once, one fetch.
    server.shutdown().await;
    let (server, alice, _carol, origin) = remote_setup(1024).await;
    origin.put(&id, remote::Served::honest(vec![42; 100_000]));
    let requests = (0..8).map(|_| {
        let server = &server;
        let alice = &alice;
        let path = remote::url("attachment", &id);
        async move {
            let response = get_with(server, &path, alice, &[]).await;
            (response.status(), response.bytes().await.unwrap().len())
        }
    });
    let answers = futures_util::future::join_all(requests).await;
    assert!(
        answers
            .iter()
            .all(|a| *a == (reqwest::StatusCode::OK, 100_000)),
        "{answers:?}"
    );
    assert_eq!(origin.fetches.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn the_cache_drops_the_least_recently_used_and_keeps_hashes_pinned() {
    // A budget of 1 MiB, attachments of 400 kB: two fit.
    let (server, alice, _carol, origin) = remote_setup(1).await;
    let ids: Vec<String> = (0..3)
        .map(|i| format!("{}/file{i}", remote::ORIGIN))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        origin.put(id, remote::Served::honest(vec![i as u8; 400_000]));
    }
    let get = |id: &str| {
        let path = remote::url("attachment", id);
        let (server, alice) = (&server, &alice);
        async move { get_with(server, &path, alice, &[]).await }
    };
    let service = &server.app().state().attachments;
    let evicted = |id: String| async move {
        service
            .metadata(&AttachmentId::deserialise(&id).unwrap())
            .await
            .unwrap()
            .unwrap()
            .evicted
    };
    assert_eq!(get(&ids[0]).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(get(&ids[1]).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(20)).await;
    // file0 is used again: file1 is now the least recently used.
    assert_eq!(get(&ids[0]).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(get(&ids[2]).await.status(), 200);
    assert!(!evicted(ids[0].clone()).await);
    assert!(evicted(ids[1].clone()).await);
    assert!(!evicted(ids[2].clone()).await);
    assert_eq!(origin.fetches.load(Ordering::SeqCst), 3);

    // An evicted copy is fetched again, but only as it was.
    origin.put(&ids[1], remote::Served::honest(vec![9; 400_000]));
    assert_eq!(get(&ids[1]).await.status(), 502);
    origin.put(&ids[1], remote::Served::honest(vec![1; 400_000]));
    let response = get(&ids[1]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().to_vec(), vec![1; 400_000]);
    assert!(!evicted(ids[1].clone()).await);
    server.shutdown().await;
}
