//! The built-in agents against a real server and a fake model provider.

mod common;

use std::time::Duration;

use common::fake_provider::{FakeProvider, Reply};
use common::*;
use futures_util::{SinkExt, StreamExt};
use resonator_model::proto::json as proto_json;
use resonator_model::proto::waveclient_rpc::ProtocolOpenRequest;
use resonator_server::config::{AgentTools, AiAgentConfig, AiProviderConfig, Config};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

const HELPER: &str = "helper@example.com";
const ALICE: &str = "alice@example.com";

fn agent(name: &str, tools: AgentTools) -> AiAgentConfig {
    AiAgentConfig {
        name: name.to_owned(),
        provider: "fake".to_owned(),
        model: "fake-model".to_owned(),
        tools,
        system_prompt: "Be brief.".to_owned(),
        ..AiAgentConfig::default()
    }
}

fn with_agents(fake: &FakeProvider, agents: Vec<AiAgentConfig>) -> impl FnOnce(&mut Config) {
    let base_url = fake.base_url();
    move |config: &mut Config| {
        config.ai.providers.insert(
            "fake".to_owned(),
            AiProviderConfig {
                base_url,
                max_retries: 0,
                first_byte_timeout_secs: 5,
                idle_timeout_secs: 5,
                ..AiProviderConfig::default()
            },
        );
        config.ai.agents = agents;
        config.ai.reply_debounce_ms = 300;
        config.ai.stream_batch_ms = 40;
    }
}

async fn start(fake: &FakeProvider, agents: Vec<AiAgentConfig>) -> Server {
    Server::start_with(with_agents(fake, agents)).await
}

async fn run(server: &Server, token: &str, ops: Value) -> Value {
    let (status, body) = server.data_api(token, ops).await;
    assert_eq!(status, 200, "{body}");
    for response in body.as_array().unwrap() {
        assert!(response.get("error").is_none(), "{body}");
    }
    body
}

/// The blips written by an agent: (id, content).
async fn blips_of(
    server: &Server,
    token: &str,
    wave: &str,
    wavelet: &str,
    who: &str,
) -> Vec<(String, String)> {
    server
        .blips(token, wave, wavelet)
        .await
        .into_iter()
        .filter(|b| b.1 == who)
        .map(|b| (b.0, b.2))
        .collect()
}

/// Waits until an agent's blip contains `text`; returns its id.
async fn wait_for_text(
    server: &Server,
    token: &str,
    wave: &str,
    wavelet: &str,
    who: &str,
    text: &str,
) -> String {
    let found = std::sync::Mutex::new(String::new());
    server
        .wait_until(&format!("{who} writes '{text}'"), || async {
            match blips_of(server, token, wave, wavelet, who)
                .await
                .into_iter()
                .find(|b| b.1.contains(text))
            {
                Some((id, _)) => {
                    *found.lock().unwrap() = id;
                    true
                }
                None => false,
            }
        })
        .await;
    found.into_inner().unwrap()
}

/// Starts with `ai.max_reanswers` set.
async fn start_reanswering(fake: &FakeProvider, max_reanswers: u32) -> Server {
    let configure = with_agents(fake, vec![agent("helper", AgentTools::None)]);
    Server::start_with(move |config: &mut Config| {
        configure(config);
        config.ai.max_reanswers = max_reanswers;
    })
    .await
}

/// The content of a blip ("" if it does not exist).
async fn content_of(server: &Server, token: &str, wave: &str, wavelet: &str, blip: &str) -> String {
    let fetched = server.fetch(token, wave, wavelet).await;
    fetched["data"]["blips"][blip]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Replaces the whole text of a blip in one edit, as a person retyping
/// and saving it.
async fn rewrite(server: &Server, token: &str, wave: &str, wavelet: &str, blip: &str, text: &str) {
    let length = content_of(server, token, wave, wavelet, blip)
        .await
        .encode_utf16()
        .count();
    run(
        server,
        token,
        json!([{"id": "rw", "method": "document.modify", "params": {
            "waveId": wave, "waveletId": wavelet, "blipId": blip,
            "range": {"start": 1, "end": length},
            "modifyAction": {"modifyHow": "REPLACE", "values": [text]}
        }}]),
    )
    .await;
}

/// Waits until a blip's content is exactly `content`.
async fn wait_for_content(
    server: &Server,
    token: &str,
    wave: &str,
    wavelet: &str,
    blip: &str,
    content: &str,
) {
    server
        .wait_until(&format!("{blip} reads {content:?}"), || async {
            content_of(server, token, wave, wavelet, blip).await == content
        })
        .await;
}

/// A wave with the helper in it (its greeting "Hello." answered) and
/// the owner's reply to the greeting answered with `answer`: (wave, wavelet,
/// root, reply, answer blip).
async fn answered_reply(
    server: &Server,
    fake: &FakeProvider,
    (owner, token): (&str, &str),
    question: &str,
    answer: Reply,
    answer_text: &str,
) -> (String, String, String, String, String) {
    fake.push(Reply::text(&["Hello."]));
    fake.push(answer);
    let (wave, wavelet, root) = server.create_wave(token, &[owner], "Questions").await;
    run(
        server,
        token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let greeting = wait_for_text(server, token, &wave, &wavelet, HELPER, "Hello.").await;
    let created = run(
        server,
        token,
        json!([continue_thread("r", &wave, &wavelet, &greeting, question)]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    let answer = wait_for_text(server, token, &wave, &wavelet, HELPER, answer_text).await;
    (wave, wavelet, root, reply, answer)
}

/// The text of the `user` message of a recorded request.
fn user_message(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn added_agent_streams_its_reply_and_clients_see_it() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Text {
        pieces: vec!["Four".into(), " is the".into(), " **answer**.".into()],
        delay: Duration::from_millis(120),
    });
    let server = start(&fake, vec![agent("helper", AgentTools::Wave)]).await;
    let cookie = server.user("alice").await;
    let token = server.token_for(&cookie, &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "What is 2+2?")
        .await;

    // Alice has the wave open in a client.
    let mut request = format!("ws://{}/socket", server.addr)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().unwrap());
    request
        .headers_mut()
        .insert("Origin", format!("http://{}", server.addr).parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let open = proto_json::envelope(
        1,
        &ProtocolOpenRequest {
            participant_id: Some(String::new()),
            wave_id: Some(wave.clone()),
            wavelet_id_prefix: vec![String::new()],
            known_wavelet: Vec::new(),
        },
    )
    .unwrap();
    socket.send(WsMessage::Text(open.into())).await.unwrap();

    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let reply = wait_for_text(
        &server,
        &token,
        &wave,
        &wavelet,
        HELPER,
        "Four is the answer.",
    )
    .await;
    // The Markdown was turned into formatting.
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let blip = &fetched["data"]["blips"][&reply];
    assert_eq!(blip["content"], "\nFour is the answer.", "{blip}");
    let annotations = blip["annotations"].to_string();
    assert!(annotations.contains("style/fontWeight"), "{annotations}");

    // The model saw the wave and was told who it is.
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let body = &requests[0].body;
    assert_eq!(body["model"], "fake-model");
    assert_eq!(body["stream"], true);
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("You are helper@example.com")
    );
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .ends_with("Be brief.")
    );
    let user = user_message(body);
    assert!(
        user.contains("What is 2+2?") && user.contains("alice@example.com"),
        "{user}"
    );

    // The client received the agent's edits as they were made: several
    // deltas by the agent, the text arriving piece by piece.
    let mut seen = String::new();
    let mut agent_deltas = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(seen.contains("Four") && seen.contains(" is the") && seen.contains("answer")) {
        let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, socket.next()).await else {
            panic!("the client did not see the reply: {seen}");
        };
        if let WsMessage::Text(text) = frame {
            agent_deltas += usize::from(text.contains(HELPER));
            seen.push_str(text.as_str());
        }
    }
    assert!(agent_deltas >= 2, "{agent_deltas}");
    let _ = socket.close(None).await;
    server.stop().await;
}

/// The thread holding `blip` ("" for the root thread) and its blips.
fn thread_holding(fetched: &Value, blip: &str) -> (String, Vec<String>) {
    let data = &fetched["data"];
    if let Some(root) = data["waveletData"]["rootThread"]["blipIds"].as_array()
        && root.iter().any(|b| b == blip)
    {
        return (
            String::new(),
            root.iter()
                .map(|b| b.as_str().unwrap().to_owned())
                .collect(),
        );
    }
    for (id, thread) in data["threads"].as_object().unwrap() {
        let blips: Vec<String> = thread["blipIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_str().unwrap().to_owned())
            .collect();
        if blips.iter().any(|b| b == blip) {
            return (id.clone(), blips);
        }
    }
    panic!("{blip} is in no thread: {fetched}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_are_replies_and_side_conversations_stay_together() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["First answer."]));
    fake.push(Reply::text(&["Second answer."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server.create_wave(&token, &[ALICE], "What is 2+2?").await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;

    // Added: the answer is an indented reply under the question.
    let first = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "First answer.").await;
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let (thread, blips) = thread_holding(&fetched, &first);
    assert!(!thread.is_empty(), "the answer is not in a reply thread");
    let under_root = fetched["data"]["blips"][&root]["replyThreadIds"].to_string();
    assert!(under_root.contains(&thread), "{under_root}");
    assert_eq!(blips, vec![first.clone()]);

    // A reply that ends that thread is answered next in the same thread,
    // not one level deeper.
    let created = run(
        &server,
        &token,
        json!([continue_thread("r", &wave, &wavelet, &first, "And 3+3?")]),
    )
    .await;
    let question = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    let second = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Second answer.").await;
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let (same, blips) = thread_holding(&fetched, &second);
    assert_eq!(same, thread);
    assert_eq!(blips, vec![first, question, second]);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_typed_slowly_is_answered_once_after_it_is_quiet() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hello, I am here."]));
    fake.push(Reply::text(&["Answer to the question."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "Planning the trip")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let agent_blip = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "I am here").await;
    assert!(fake.wait_for_requests(1, TIMEOUT).await);

    // Alice replies, typing in bursts 100 ms apart.
    let created = run(
        &server,
        &token,
        json!([continue_thread("r", &wave, &wavelet, &agent_blip, "\nWhat")]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    for piece in [" should", " we", " pack", "?"] {
        tokio::time::sleep(Duration::from_millis(100)).await;
        run(
            &server,
            &token,
            json!([modify_append("m", &wave, &wavelet, &reply, piece)]),
        )
        .await;
    }
    // Nothing while she types.
    assert_eq!(fake.request_count(), 1);
    assert!(fake.wait_for_requests(2, TIMEOUT).await);
    let user = user_message(&fake.requests()[1].body);
    assert!(user.contains("What should we pack?"), "{user}");
    assert!(
        user.contains(&format!("[{reply}]")) && user.contains("<== the message you are answering")
    );
    let answer = wait_for_text(
        &server,
        &token,
        &wave,
        &wavelet,
        HELPER,
        "Answer to the question.",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(fake.request_count(), 2, "answered more than once");

    // A later edit of the answered reply changes what was asked: once it
    // is quiet, the answer is redone in its blip, once.
    fake.push(Reply::text(&["Pack food too."]));
    run(
        &server,
        &token,
        json!([modify_append("m2", &wave, &wavelet, &reply, " And food?")]),
    )
    .await;
    assert_eq!(
        fake.request_count(),
        2,
        "answered before the edit was quiet"
    );
    assert!(fake.wait_for_requests(3, TIMEOUT).await);
    let user = user_message(&fake.requests()[2].body);
    assert!(user.contains("What should we pack? And food?"), "{user}");
    wait_for_content(
        &server,
        &token,
        &wave,
        &wavelet,
        &answer,
        "\nPack food too.",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 3);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrelated_edits_call_no_model() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hi."]));
    // With re-answering on, editing the text the agent answered when it
    // was added redoes that answer (see
    // `a_rewritten_root_redoes_the_added_answer_in_place`); here only
    // that these are not replies to the agent.
    let server = start_reanswering(&fake, 0).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server
        .create_wave(&token, &["alice@example.com"], "Notes")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Hi.").await;
    // Editing the root blip and replying to it are not replies to the agent.
    run(
        &server,
        &token,
        json!([
            modify_append("m", &wave, &wavelet, &root, " more notes"),
            {"id": "c", "method": "blip.createChild", "params": {
                "waveId": wave, "waveletId": wavelet, "blipId": root,
                "blipData": {"waveId": wave, "waveletId": wavelet, "blipId": "TBD_c", "content": "\naside"}
            }},
            {"id": "t", "method": "wavelet.setTitle", "params": {
                "waveId": wave, "waveletId": wavelet, "waveletTitle": "Renamed"}}
        ]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(fake.request_count(), 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_added_agent_waits_until_the_wave_is_quiet() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Noted."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server
        .create_wave(&token, &["alice@example.com"], "Draft")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    // Alice keeps writing for longer than the quiet interval (300 ms).
    for word in [
        "one", "two", "three", "four", "five", "six", "seven", "LAST",
    ] {
        tokio::time::sleep(Duration::from_millis(120)).await;
        run(
            &server,
            &token,
            json!([modify_append(
                "m",
                &wave,
                &wavelet,
                &root,
                &format!(" {word}")
            )]),
        )
        .await;
        assert_eq!(fake.request_count(), 0, "answered while {word} was typed");
    }
    wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Noted.").await;
    assert_eq!(fake.request_count(), 1);
    let requests = fake.requests();
    let body = &requests[0].body;
    assert!(user_message(body).contains("seven LAST"));
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(system.contains("may be unfinished"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_calls_round_trip() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::Wave)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server
        .create_wave(&token, &["alice@example.com"], "The code word is PELICAN")
        .await;
    fake.push(Reply::ToolCalls(vec![(
        "call-1".into(),
        "read_wave".into(),
        json!({"blip_id": root}),
    )]));
    fake.push(Reply::text(&["The code word is PELICAN, I read it."]));
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    wait_for_text(&server, &token, &wave, &wavelet, HELPER, "I read it.").await;
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    let tools: Vec<&str> = requests[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["read_wave", "reply_to_blip", "edit_own_blip"]);
    let messages = requests[1].body["messages"].as_array().unwrap();
    let assistant = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
    let tool = messages.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(tool["tool_call_id"], "call-1");
    assert!(
        tool["content"].as_str().unwrap().contains("PELICAN"),
        "{tool}"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_failure_ends_with_a_visible_note() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Status {
        status: 500,
        retry_after: None,
        message: "secret internal detail".into(),
    });
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "Hello?")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let blip = wait_for_text(
        &server,
        &token,
        &wave,
        &wavelet,
        HELPER,
        "could not be finished",
    )
    .await;
    let blips = blips_of(&server, &token, &wave, &wavelet, HELPER).await;
    let content = &blips.iter().find(|b| b.0 == blip).unwrap().1;
    assert!(
        content.contains("HTTP 500") && !content.contains("secret"),
        "{content}"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_agent_mid_stream_cancels_the_request() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Hang);
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "Think hard")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    assert!(fake.wait_for_requests(1, TIMEOUT).await);
    // The reply blip exists while the model thinks.
    server
        .wait_until("the reply blip", || async {
            !blips_of(&server, &token, &wave, &wavelet, HELPER)
                .await
                .is_empty()
        })
        .await;
    run(
        &server,
        &token,
        json!([{"id": "r", "method": "wavelet.removeParticipant", "params": {
            "waveId": wave, "waveletId": wavelet, "participantId": HELPER}}]),
    )
    .await;
    assert!(
        fake.wait_for_cancelled(1, TIMEOUT).await,
        "the request was not cancelled"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_agents_do_not_talk_in_a_loop() {
    let fake = FakeProvider::start().await;
    for _ in 0..6 {
        fake.push(Reply::text(&["My two cents."]));
    }
    let server = start(
        &fake,
        vec![
            agent("helper", AgentTools::None),
            agent("critic", AgentTools::None),
        ],
    )
    .await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "Discuss")
        .await;
    run(
        &server,
        &token,
        json!([
            add_participant("a", &wave, &wavelet, HELPER),
            add_participant("b", &wave, &wavelet, "critic@example.com")
        ]),
    )
    .await;
    assert!(fake.wait_for_requests(2, TIMEOUT).await);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(fake.request_count(), 2, "agents answered each other");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_does_not_answer_again() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hello."]));
    fake.push(Reply::text(&["Answered."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _) = server
        .create_wave(&token, &["alice@example.com"], "Restarts")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let agent_blip = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Hello.").await;
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &agent_blip,
            "\nA question"
        )]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Answered.").await;
    assert_eq!(fake.request_count(), 2);

    let dir = server.stop().await;
    let server = Server::start_in(
        dir,
        with_agents(&fake, vec![agent("helper", AgentTools::None)]),
    )
    .await;
    let token = server.token("alice", &["read", "write"]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(fake.request_count(), 2, "answered again after the restart");
    // An edit that changes nothing is still nothing after the restart.
    run(
        &server,
        &token,
        json!([modify_append("m", &wave, &wavelet, &reply, "  ")]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 2, "answered again without a change");
    // A real edit after the restart redoes the answer in its blip.
    fake.push(Reply::text(&["Answered again."]));
    run(
        &server,
        &token,
        json!([modify_append("m", &wave, &wavelet, &reply, " again")]),
    )
    .await;
    assert!(fake.wait_for_requests(3, TIMEOUT).await);
    let user = user_message(&fake.requests()[2].body);
    assert!(
        user.contains("A question") && user.contains("again"),
        "{user}"
    );
    wait_for_content(
        &server,
        &token,
        &wave,
        &wavelet,
        &answer,
        "\nAnswered again.",
    )
    .await;
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_cannot_read_other_waves() {
    let fake = FakeProvider::start().await;
    let server = start(
        &fake,
        vec![
            agent("helper", AgentTools::Wave),
            agent("scout", AgentTools::All),
        ],
    )
    .await;
    let token = server.token("alice", &["read", "write"]).await;
    let bob = server.token("bob", &["read", "write"]).await;
    let (secret_wave, _, _) = server
        .create_wave(&bob, &["bob@example.com"], "The secret is OSPREY-77")
        .await;

    for (name, expected) in [
        ("helper", "only use the wave you are answering in"),
        ("scout", "cannot read wave"),
    ] {
        let before = fake.request_count();
        fake.push(Reply::ToolCalls(vec![(
            "c".into(),
            "read_wave".into(),
            json!({"wave_id": secret_wave}),
        )]));
        fake.push(Reply::text(&["I could not read it."]));
        let (wave, wavelet, _) = server
            .create_wave(&token, &["alice@example.com"], "Read bob's wave please")
            .await;
        let address = format!("{name}@example.com");
        run(
            &server,
            &token,
            json!([add_participant("a", &wave, &wavelet, &address)]),
        )
        .await;
        wait_for_text(
            &server,
            &token,
            &wave,
            &wavelet,
            &address,
            "could not read it",
        )
        .await;
        let requests = fake.requests();
        assert_eq!(requests.len(), before + 2);
        let messages = requests[before + 1].body["messages"].as_array().unwrap();
        let tool = messages.iter().find(|m| m["role"] == "tool").unwrap();
        let content = tool["content"].as_str().unwrap();
        assert!(
            content.starts_with("Error:") && content.contains(expected),
            "{name}: {content}"
        );
        assert!(!content.contains("OSPREY"), "{content}");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profiles_mark_agents_and_robots() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let cookie = server.user("alice").await;
    let token = server.token_for(&cookie, &["read", "write"]).await;
    let (status, robot) = server
        .post_json("/robot/register", &token, &json!({"name": "notifier"}))
        .await;
    assert_eq!(status, 201, "{robot}");
    let response =
        server
            .http
            .get(server.url(
                "/profile/?addresses=helper@example.com,alice@example.com,notifier@example.com",
            ))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let profiles = body["1"].as_array().unwrap();
    assert_eq!(profiles[0]["1"], HELPER);
    assert_eq!(profiles[0]["robotKind"], "agent");
    assert!(profiles[1].get("robotKind").is_none(), "{body}");
    assert_eq!(profiles[2]["robotKind"], "robot", "{body}");
    assert_eq!(fake.request_count(), 0);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rewritten_reply_is_answered_again_in_the_same_blip() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nHow high is Mont Blanc?",
        Reply::text(&["**Mont Blanc** is 4808 m high."]),
        "4808 m",
    )
    .await;
    // The first answer was turned into formatting.
    wait_for_content(
        &server,
        &token,
        &wave,
        &wavelet,
        &answer,
        "\nMont Blanc is 4808 m high.",
    )
    .await;
    assert_eq!(fake.request_count(), 2);

    // Alice changes her question completely and saves.
    fake.push(Reply::text(&["The capital of *Peru* is Lima."]));
    rewrite(
        &server,
        &token,
        &wave,
        &wavelet,
        &reply,
        "What is the capital of Peru?",
    )
    .await;
    assert!(fake.wait_for_requests(3, TIMEOUT).await);
    let user = user_message(&fake.requests()[2].body);
    assert!(user.contains("What is the capital of Peru?"), "{user}");
    assert!(!user.contains("Mont Blanc"), "{user}");
    assert!(user.contains("edited after you had answered it"), "{user}");
    // The same blip now holds the new answer, and only that.
    wait_for_content(
        &server,
        &token,
        &wave,
        &wavelet,
        &answer,
        "\nThe capital of Peru is Lima.",
    )
    .await;
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let annotations = fetched["data"]["blips"][&answer]["annotations"].to_string();
    assert!(annotations.contains("style/fontStyle"), "{annotations}");
    // Its own writing does not trigger the agent again.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 3);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edits_that_change_nothing_call_no_model() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nWhat time is it?",
        Reply::text(&["Noon."]),
        "Noon.",
    )
    .await;
    // A character typed and taken back, and a few spaces.
    run(
        &server,
        &token,
        json!([modify_append("m", &wave, &wavelet, &reply, "x")]),
    )
    .await;
    let length = content_of(&server, &token, &wave, &wavelet, &reply)
        .await
        .encode_utf16()
        .count();
    run(
        &server,
        &token,
        json!([{"id": "d", "method": "document.modify", "params": {
            "waveId": wave, "waveletId": wavelet, "blipId": reply,
            "range": {"start": length - 1, "end": length},
            "modifyAction": {"modifyHow": "DELETE"}
        }}]),
    )
    .await;
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &reply).await,
        "\nWhat time is it?"
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    rewrite(
        &server,
        &token,
        &wave,
        &wavelet,
        &reply,
        "  What   time is\tit? ",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(fake.request_count(), 2);
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &answer).await,
        "\nNoon."
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rewritten_root_redoes_the_added_answer_in_place() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Four."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server
        .create_wave(&token, &["alice@example.com"], "What is 2+2?")
        .await;
    run(
        &server,
        &token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Four.").await;
    // Its own answer and a person editing the agent's blip are not the
    // text it answered.
    tokio::time::sleep(Duration::from_millis(700)).await;
    run(
        &server,
        &token,
        json!([modify_append("m", &wave, &wavelet, &answer, " (thanks)")]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 1);

    fake.push(Reply::text(&["Six."]));
    rewrite(&server, &token, &wave, &wavelet, &root, "What is 3+3?").await;
    assert!(fake.wait_for_requests(2, TIMEOUT).await);
    let user = user_message(&fake.requests()[1].body);
    assert!(user.contains("What is 3+3?"), "{user}");
    assert!(!user.contains("2+2") && !user.contains("Four"), "{user}");
    assert!(
        user.contains("edited after you had written your contribution"),
        "{user}"
    );
    wait_for_content(&server, &token, &wave, &wavelet, &answer, "\nSix.").await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 2, "the agent retriggered itself");
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        1
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_are_redone_at_most_max_reanswers_times() {
    let fake = FakeProvider::start().await;
    let server = start_reanswering(&fake, 1).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nFirst",
        Reply::text(&["One."]),
        "One.",
    )
    .await;
    fake.push(Reply::text(&["Two."]));
    rewrite(&server, &token, &wave, &wavelet, &reply, "Second").await;
    wait_for_content(&server, &token, &wave, &wavelet, &answer, "\nTwo.").await;
    rewrite(&server, &token, &wave, &wavelet, &reply, "Third").await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(fake.request_count(), 3, "redone more than once");
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &answer).await,
        "\nTwo."
    );
    server.stop().await;

    // 0: never redone.
    let fake = FakeProvider::start().await;
    let server = start_reanswering(&fake, 0).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nFirst",
        Reply::text(&["One."]),
        "One.",
    )
    .await;
    rewrite(&server, &token, &wave, &wavelet, &reply, "Second").await;
    rewrite(&server, &token, &wave, &wavelet, &root, "Other").await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(fake.request_count(), 2);
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &answer).await,
        "\nOne."
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_while_the_answer_streams_restarts_it() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let slow = Reply::Text {
        pieces: vec!["slow ".to_owned(); 40],
        delay: Duration::from_millis(150),
    };
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nFirst question",
        slow,
        "slow",
    )
    .await;
    assert_eq!(fake.request_count(), 2);
    fake.push(Reply::text(&["Final answer."]));
    rewrite(&server, &token, &wave, &wavelet, &reply, "Second question").await;
    assert!(
        fake.wait_for_cancelled(1, TIMEOUT).await,
        "the first request was not cancelled"
    );
    assert!(fake.wait_for_requests(3, TIMEOUT).await);
    let user = user_message(&fake.requests()[2].body);
    assert!(
        user.contains("Second question") && !user.contains("First question"),
        "{user}"
    );
    wait_for_content(&server, &token, &wave, &wavelet, &answer, "\nFinal answer.").await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 3);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deleted_or_replied_to_answer_is_redone_in_a_new_blip() {
    let fake = FakeProvider::start().await;
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;

    // The answer was deleted: a new blip.
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        (ALICE, &token),
        "\nFirst",
        Reply::text(&["One."]),
        "One.",
    )
    .await;
    run(
        &server,
        &token,
        json!([{"id": "d", "method": "blip.delete", "params": {
            "waveId": wave, "waveletId": wavelet, "blipId": answer}}]),
    )
    .await;
    fake.push(Reply::text(&["Two."]));
    rewrite(&server, &token, &wave, &wavelet, &reply, "Second").await;
    let again = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Two.").await;
    assert_ne!(again, answer);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    assert_eq!(fake.request_count(), 3);

    // Someone replied to the answer: it stays as their context, and the
    // new answer goes into a new blip. (Bob: Alice has used up her
    // requests per minute.)
    let token = server.token("bob", &["read", "write"]).await;
    let (wave, wavelet, _, reply, answer) = answered_reply(
        &server,
        &fake,
        ("bob@example.com", &token),
        "\nFirst",
        Reply::text(&["One."]),
        "One.",
    )
    .await;
    fake.push(Reply::text(&["You are welcome."]));
    run(
        &server,
        &token,
        json!([continue_thread("t", &wave, &wavelet, &answer, "\nThanks")]),
    )
    .await;
    wait_for_text(&server, &token, &wave, &wavelet, HELPER, "welcome").await;
    assert_eq!(fake.request_count(), 6);
    fake.push(Reply::text(&["Two."]));
    rewrite(&server, &token, &wave, &wavelet, &reply, "Second").await;
    let again = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Two.").await;
    assert_ne!(again, answer);
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &answer).await,
        "\nOne."
    );
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        4
    );
    // Rewriting again goes to the new blip, which has no replies.
    fake.push(Reply::text(&["Three."]));
    rewrite(&server, &token, &wave, &wavelet, &reply, "Third").await;
    wait_for_content(&server, &token, &wave, &wavelet, &again, "\nThree.").await;
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        4
    );
    server.stop().await;
}

// ---- Finished editing: a no-op from the client ends the quiet wait ----

/// A quiet period long enough to tell "answered when finished" from
/// "answered when the timer ran out".
const SLOW_DEBOUNCE: Duration = Duration::from_millis(5000);
/// Well under [`SLOW_DEBOUNCE`]: what "at once" may take at most.
const PROMPT: Duration = Duration::from_millis(2500);

async fn start_slow(fake: &FakeProvider) -> Server {
    let configure = with_agents(fake, vec![agent("helper", AgentTools::None)]);
    Server::start_with(move |config: &mut Config| {
        configure(config);
        config.ai.reply_debounce_ms = SLOW_DEBOUNCE.as_millis() as u64;
    })
    .await
}

/// Waits for the `count`th model request; returns how long that took.
async fn time_to_request(fake: &FakeProvider, count: usize) -> Duration {
    let started = std::time::Instant::now();
    assert!(
        fake.wait_for_requests(count, Duration::from_secs(15)).await,
        "no request {count}"
    );
    started.elapsed()
}

/// A wave of Alice (and others) with the helper added and its first
/// answer, "Hello.", given at once because Alice said she finished:
/// (wave, wavelet, root, greeting blip).
async fn greeted(
    server: &Server,
    fake: &FakeProvider,
    token: &str,
    participants: &[&str],
) -> (String, String, String, String) {
    fake.push(Reply::text(&["Hello."]));
    let (wave, wavelet, root) = server.create_wave(token, participants, "Questions").await;
    run(
        server,
        token,
        json!([add_participant("a", &wave, &wavelet, HELPER)]),
    )
    .await;
    server.no_op(ALICE, &wave, &wavelet).await;
    let took = time_to_request(fake, 1).await;
    assert!(took < PROMPT, "the first answer took {took:?}");
    let greeting = wait_for_text(server, token, &wave, &wavelet, HELPER, "Hello.").await;
    (wave, wavelet, root, greeting)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finishing_an_edit_answers_at_once_first_and_again() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    // Added to a wave with text: answered as soon as Alice finishes.
    let (wave, wavelet, _, greeting) = greeted(&server, &fake, &token, &[ALICE]).await;

    // A reply to the agent, then "finished": answered at once.
    fake.push(Reply::text(&["Mont Blanc is 4808 m high."]));
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &greeting,
            "\nHow high is Mont Blanc?"
        )]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fake.request_count(),
        1,
        "answered before the reply was finished"
    );
    server.no_op(ALICE, &wave, &wavelet).await;
    let took = time_to_request(&fake, 2).await;
    assert!(took < PROMPT, "the answer took {took:?}");
    assert!(user_message(&fake.requests()[1].body).contains("How high is Mont Blanc?"));
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "4808 m").await;

    // An edit of the answered reply, then "finished": redone at once, in
    // the same blip.
    fake.push(Reply::text(&["Lima."]));
    rewrite(
        &server,
        &token,
        &wave,
        &wavelet,
        &reply,
        "What is the capital of Peru?",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fake.request_count(),
        2,
        "redone before the edit was finished"
    );
    server.no_op(ALICE, &wave, &wavelet).await;
    let took = time_to_request(&fake, 3).await;
    assert!(took < PROMPT, "the redo took {took:?}");
    assert!(user_message(&fake.requests()[2].body).contains("capital of Peru"));
    wait_for_content(&server, &token, &wave, &wavelet, &answer, "\nLima.").await;
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        2
    );
    // The timers that went off early do not go off again later.
    tokio::time::sleep(SLOW_DEBOUNCE + Duration::from_millis(500)).await;
    assert_eq!(fake.request_count(), 3);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn someone_else_finishing_does_not_cut_the_wait() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    let bob = "bob@example.com";
    server.token("bob", &["read", "write"]).await;
    let (wave, wavelet, _, greeting) = greeted(&server, &fake, &token, &[ALICE, bob]).await;

    // Alice replies; Bob, who wrote nothing, finishes an edit session.
    fake.push(Reply::text(&["An answer."]));
    let replied = std::time::Instant::now();
    run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &greeting,
            "\nA question"
        )]),
    )
    .await;
    server.no_op(bob, &wave, &wavelet).await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(fake.request_count(), 1, "answered on someone else's no-op");
    // The quiet period still ends the wait.
    time_to_request(&fake, 2).await;
    let waited = replied.elapsed();
    assert!(
        waited >= SLOW_DEBOUNCE - Duration::from_millis(500),
        "{waited:?}"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_no_op_with_nothing_waiting_calls_no_model() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, _) = greeted(&server, &fake, &token, &[ALICE]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for _ in 0..3 {
        server.no_op(ALICE, &wave, &wavelet).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(fake.request_count(), 1);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        1
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writing_again_after_finishing_waits_for_quiet_again() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, greeting) = greeted(&server, &fake, &token, &[ALICE]).await;
    fake.push(Reply::text(&["First."]));
    let created = run(
        &server,
        &token,
        json!([continue_thread("r", &wave, &wavelet, &greeting, "\nOne")]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    server.no_op(ALICE, &wave, &wavelet).await;
    assert!(time_to_request(&fake, 2).await < PROMPT);
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "First.").await;
    // Alice writes again: her earlier "finished" no longer counts.
    fake.push(Reply::text(&["Second."]));
    let edited = std::time::Instant::now();
    run(
        &server,
        &token,
        json!([modify_append("m", &wave, &wavelet, &reply, " and two")]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(fake.request_count(), 2, "redone without a quiet period");
    time_to_request(&fake, 3).await;
    let waited = edited.elapsed();
    assert!(
        waited >= SLOW_DEBOUNCE - Duration::from_millis(500),
        "{waited:?}"
    );
    wait_for_content(&server, &token, &wave, &wavelet, &answer, "\nSecond.").await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finishing_a_whitespace_edit_calls_no_model() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, _, greeting) = greeted(&server, &fake, &token, &[ALICE]).await;
    fake.push(Reply::text(&["Noon."]));
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &greeting,
            "\nWhat time is it?"
        )]),
    )
    .await;
    let reply = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    server.no_op(ALICE, &wave, &wavelet).await;
    assert!(time_to_request(&fake, 2).await < PROMPT);
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Noon.").await;
    rewrite(
        &server,
        &token,
        &wave,
        &wavelet,
        &reply,
        "  What   time is\tit? ",
    )
    .await;
    server.no_op(ALICE, &wave, &wavelet).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(fake.request_count(), 2);
    assert_eq!(
        content_of(&server, &token, &wave, &wavelet, &answer).await,
        "\nNoon."
    );
    server.stop().await;
}

/// `document.modify` marking `start..end` of a blip's text as an
/// `@`-mention of `who`, as the web client does.
fn mention(id: &str, wave: &str, wavelet: &str, blip: &str, range: (i32, i32), who: &str) -> Value {
    json!({"id": id, "method": "document.modify", "params": {
        "waveId": wave, "waveletId": wavelet, "blipId": blip,
        "range": {"start": range.0, "end": range.1},
        "modifyAction": {"modifyHow": "ANNOTATE", "annotationKey": "mention/user", "values": [who]}
    }})
}

/// The reply threads of a blip, as fetched.
fn reply_threads_of(fetched: &Value, blip: &str) -> String {
    fetched["data"]["blips"][blip]["replyThreadIds"].to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mentioning_an_agent_adds_it_and_it_answers_the_mention() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Pack light."]));
    let server = start(&fake, vec![agent("helper", AgentTools::None)]).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server.create_wave(&token, &[ALICE], "Trip").await;
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &root,
            "\nhelper, what should we pack?"
        )]),
    )
    .await;
    let asking = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    // The web client adds whoever is mentioned and not in the wave yet,
    // then marks the name.
    run(
        &server,
        &token,
        json!([
            add_participant("a", &wave, &wavelet, HELPER),
            mention("m", &wave, &wavelet, &asking, (1, 7), HELPER)
        ]),
    )
    .await;
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "Pack light.").await;
    let user = user_message(&fake.requests()[0].body);
    assert!(user.contains("what should we pack?"), "{user}");
    assert!(
        user.contains(&format!("Blip [{asking}]")) && user.contains("mentions you (@helper)"),
        "{user}"
    );
    assert!(!user.contains("has just added you"), "{user}");
    // A reply under the mentioning blip, not a contribution elsewhere.
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let (thread, blips) = thread_holding(&fetched, &answer);
    assert!(
        reply_threads_of(&fetched, &asking).contains(&thread),
        "{fetched}"
    );
    assert_eq!(blips, vec![answer.clone()]);
    // Answered once: being added made no answer of its own.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 1);

    // An edit of the mentioning blip is redone like an edited reply, in
    // the same blip.
    fake.push(Reply::text(&["Pack light, and food."]));
    run(
        &server,
        &token,
        json!([modify_append("e", &wave, &wavelet, &asking, " And food?")]),
    )
    .await;
    assert!(fake.wait_for_requests(2, TIMEOUT).await);
    let user = user_message(&fake.requests()[1].body);
    assert!(
        user.contains("And food?") && user.contains("edited after"),
        "{user}"
    );
    wait_for_content(
        &server,
        &token,
        &wave,
        &wavelet,
        &answer,
        "\nPack light, and food.",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(fake.request_count(), 2);
    assert_eq!(
        blips_of(&server, &token, &wave, &wavelet, HELPER)
            .await
            .len(),
        1
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mention_is_answered_like_a_reply_and_finishing_answers_at_once() {
    let fake = FakeProvider::start().await;
    let server = start_slow(&fake).await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root, _) = greeted(&server, &fake, &token, &[ALICE]).await;

    // A new message in the main thread that mentions the agent: not a
    // reply to it, but addressed to it. Nothing while Alice writes.
    fake.push(Reply::text(&["The 12th works."]));
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "r",
            &wave,
            &wavelet,
            &root,
            "\nhelper which date works?"
        )]),
    )
    .await;
    let asking = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    run(
        &server,
        &token,
        json!([mention("m", &wave, &wavelet, &asking, (1, 7), HELPER)]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.request_count(), 1, "answered before Alice finished");
    // She finishes: answered at once.
    server.no_op(ALICE, &wave, &wavelet).await;
    let took = time_to_request(&fake, 2).await;
    assert!(took < PROMPT, "the answer took {took:?}");
    let user = user_message(&fake.requests()[1].body);
    assert!(user.contains("mentions you"), "{user}");
    let answer = wait_for_text(&server, &token, &wave, &wavelet, HELPER, "The 12th").await;
    let fetched = server.fetch(&token, &wave, &wavelet).await;
    let (thread, _) = thread_holding(&fetched, &answer);
    assert!(reply_threads_of(&fetched, &asking).contains(&thread));

    // A mention of somebody else is nobody's business but theirs.
    let created = run(
        &server,
        &token,
        json!([continue_thread(
            "s",
            &wave,
            &wavelet,
            &root,
            "\nbob are you in?"
        )]),
    )
    .await;
    let other = created[0]["data"]["newBlipId"].as_str().unwrap().to_owned();
    run(
        &server,
        &token,
        json!([mention(
            "n",
            &wave,
            &wavelet,
            &other,
            (1, 4),
            "bob@example.com"
        )]),
    )
    .await;
    server.no_op(ALICE, &wave, &wavelet).await;
    tokio::time::sleep(SLOW_DEBOUNCE + Duration::from_millis(500)).await;
    assert_eq!(fake.request_count(), 2);
    server.stop().await;
}
