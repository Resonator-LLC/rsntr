//! End-to-end tests of search and the inbox: the real server, waves built
//! with the model's editing layers, searched over HTTP and through the
//! library interface.
//!
//! The cases of `SimpleSearchProviderImplTest` of Apache Wave are ported
//! in `java_*`; the rest covers what this server adds (folders from the
//! supplement, full text, the persistent index, the change signal).

mod common;
#[path = "common/net.rs"]
mod net;
#[path = "common/waves.rs"]
mod waves;

use std::collections::BTreeSet;
use std::time::Duration;

use resonator_model::id::{ParticipantId, WaveId, WaveletName};
use resonator_model::proto::json::from_json;
use resonator_model::proto::search::SearchResponse;
use resonator_model::version::HashedVersionFactory;
use resonator_server::search::{SearchProvider, SearchResult};
use resonator_server::waveserver::{WaveServer, WaveletProvider};
use serde_json::Value;

use common::{DOMAIN, add_participant, delta, no_op, participant, remove_participant};
use net::TestServer;
use waves::{append_blip, archive, create_wave, follow, mark_read, mute, root_name, wave_id};

fn user(name: &str) -> ParticipantId {
    participant(&format!("{name}@{DOMAIN}"))
}

async fn search_raw(
    server: &TestServer,
    cookie: Option<&str>,
    query: &str,
    index: usize,
    num_results: usize,
) -> reqwest::Response {
    let mut request = server.http.get(server.url("/search/")).query(&[
        ("query", query.to_owned()),
        ("index", index.to_string()),
        ("numResults", num_results.to_string()),
    ]);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.send().await.expect("search request")
}

async fn search_page(
    server: &TestServer,
    cookie: &str,
    query: &str,
    index: usize,
    num_results: usize,
) -> SearchResponse {
    let response = search_raw(server, Some(cookie), query, index, num_results).await;
    assert_eq!(response.status(), 200, "search '{query}'");
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    let text = response.text().await.expect("search body");
    from_json(&text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

async fn search(server: &TestServer, cookie: &str, query: &str) -> SearchResponse {
    search_page(server, cookie, query, 0, 100).await
}

/// The ids of the waves found, without the domain.
fn found(response: &SearchResponse) -> Vec<String> {
    response
        .digests
        .iter()
        .map(|digest| {
            let id = digest.wave_id.as_deref().expect("wave id");
            id.strip_prefix(&format!("{DOMAIN}/"))
                .unwrap_or_else(|| panic!("unexpected wave id {id}"))
                .to_owned()
        })
        .collect()
}

async fn found_for(server: &TestServer, cookie: &str, query: &str) -> Vec<String> {
    found(&search(server, cookie, query).await)
}

/// Distinct timestamps for what happens before and after.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(3)).await;
}

#[tokio::test]
async fn inbox_lists_waves_latest_first_with_unread_counts() {
    let server = TestServer::start().await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    let (alice, bob) = (user("alice"), user("bob"));
    let waves = server.app().server();

    let one = create_wave(waves, &alice, "w+one", &[&bob], "First wave").await;
    tick().await;
    create_wave(waves, &alice, "w+two", &[&bob], "Second wave").await;
    tick().await;
    create_wave(waves, &alice, "w+three", &[], "Third wave").await;

    // Nobody signed in: forbidden, as in the original.
    assert_eq!(
        search_raw(&server, None, "in:inbox", 0, 10).await.status(),
        403
    );

    let inbox = search(&server, &bob_cookie, "in:inbox").await;
    assert_eq!(found(&inbox), ["w+two", "w+one"]);
    assert_eq!(inbox.query.as_deref(), Some("in:inbox"));
    // Fewer results than asked for: the total is known.
    assert_eq!(inbox.total_results, Some(2));
    let digest = &inbox.digests[1];
    assert_eq!(digest.title.as_deref(), Some("First wave"));
    assert_eq!(digest.snippet.as_deref(), Some(""));
    assert_eq!(digest.wave_id.as_deref(), Some("example.com/w+one"));
    assert_eq!(digest.author.as_deref(), Some("alice@example.com"));
    assert_eq!(digest.participants, ["bob@example.com"]);
    assert_eq!(digest.blip_count, Some(1));
    // Never opened: everything is unread.
    assert_eq!(digest.unread_count, Some(1));
    assert!(digest.last_modified.unwrap() > 1_600_000_000_000);

    assert_eq!(
        found_for(&server, &alice_cookie, "in:inbox").await,
        ["w+three", "w+two", "w+one"]
    );

    // Bob reads the first wave.
    mark_read(waves, &bob, &one).await;
    let inbox = search(&server, &bob_cookie, "in:inbox").await;
    assert_eq!(found(&inbox), ["w+two", "w+one"]);
    assert_eq!(inbox.digests[1].unread_count, Some(0));
    assert_eq!(inbox.digests[0].unread_count, Some(1));
    // Alice has not.
    let alices = search(&server, &alice_cookie, "in:inbox").await;
    assert_eq!(alices.digests[2].unread_count, Some(1));

    // Alice replies: the wave moves up and has one unread blip for Bob.
    tick().await;
    append_blip(waves, &alice, &one, "A reply").await;
    let inbox = search(&server, &bob_cookie, "in:inbox").await;
    assert_eq!(found(&inbox), ["w+one", "w+two"]);
    assert_eq!(inbox.digests[0].unread_count, Some(1));
    assert_eq!(inbox.digests[0].blip_count, Some(2));
    assert_eq!(inbox.digests[0].title.as_deref(), Some("First wave"));

    mark_read(waves, &bob, &one).await;
    let inbox = search(&server, &bob_cookie, "in:inbox").await;
    assert_eq!(inbox.digests[0].unread_count, Some(0));

    // Bob is removed from the second wave: it leaves his inbox at once.
    let two = root_name(&wave_id("w+two"));
    let version = waves.get_snapshot(&two).await.unwrap().unwrap().snapshot;
    waves
        .submit_request(
            &two,
            delta(
                &alice,
                version.hashed_version(),
                vec![remove_participant(&alice, &bob)],
            ),
        )
        .await
        .unwrap();
    assert_eq!(found_for(&server, &bob_cookie, "in:inbox").await, ["w+one"]);
    assert_eq!(found_for(&server, &bob_cookie, "").await, ["w+one"]);
    server.shutdown().await;
}

/// A delta of a single no-op is what a client sends when a person ends
/// an edit session ("finished editing", for the AI agents). It is
/// accepted, makes no blip unread and changes no blip. As any delta, it
/// moves the wavelet's version and last-modified time: the time a digest
/// shows (from the blips) stays, but the inbox, sorted by the wavelet's
/// time, puts the wave first.
#[tokio::test]
async fn a_no_op_is_accepted_and_makes_nothing_unread() {
    let server = TestServer::start().await;
    let bob_cookie = server.user("bob").await;
    let (alice, bob) = (user("alice"), user("bob"));
    let waves = server.app().server();
    let one = create_wave(waves, &alice, "w+one", &[&bob], "First wave").await;
    tick().await;
    create_wave(waves, &alice, "w+two", &[&bob], "Second wave").await;
    mark_read(waves, &bob, &one).await;
    mark_read(waves, &bob, &wave_id("w+two")).await;
    let before = search(&server, &bob_cookie, "in:inbox").await;
    assert_eq!(found(&before), ["w+two", "w+one"]);
    let old = before.digests[1].clone();
    assert_eq!(old.unread_count, Some(0));

    tick().await;
    let name = root_name(&one);
    let snapshot = waves.get_snapshot(&name).await.unwrap().unwrap().snapshot;
    let blips: Vec<(String, u64, i64)> = snapshot
        .documents()
        .map(|d| {
            (
                d.id().to_owned(),
                d.last_modified_version(),
                d.last_modified_time(),
            )
        })
        .collect();
    let result = waves
        .submit_request_as(
            &alice,
            &name,
            delta(&alice, snapshot.hashed_version(), vec![no_op(&alice)]),
        )
        .await
        .expect("a no-op from a participant is accepted");
    assert_eq!(
        result.hashed_version_after_application.version(),
        snapshot.version() + 1
    );
    let after = waves.get_snapshot(&name).await.unwrap().unwrap().snapshot;
    assert_eq!(after.version(), snapshot.version() + 1);
    // No blip changed: not their versions, times or contributors.
    let blips_after: Vec<(String, u64, i64)> = after
        .documents()
        .map(|d| {
            (
                d.id().to_owned(),
                d.last_modified_version(),
                d.last_modified_time(),
            )
        })
        .collect();
    assert_eq!(blips, blips_after);
    // The wavelet's last-modified time moved.
    assert!(after.last_modified_time() > snapshot.last_modified_time());

    let inbox = search(&server, &bob_cookie, "in:inbox").await;
    // Nothing unread for Bob, the digest otherwise the same...
    let digest = inbox
        .digests
        .iter()
        .find(|d| d.wave_id.as_deref() == Some("example.com/w+one"))
        .unwrap();
    assert_eq!(digest.unread_count, Some(0));
    assert_eq!(digest.blip_count, old.blip_count);
    assert_eq!(digest.title, old.title);
    assert_eq!(digest.snippet, old.snippet);
    assert_eq!(digest.participants, old.participants);
    // ...but for its time, and the wave is now first.
    // The time the digest shows comes from the blips and stays; the
    // index sorts by the wavelet's time, so the wave is now first.
    assert_eq!(digest.last_modified, old.last_modified);
    assert_eq!(found(&inbox), ["w+one", "w+two"]);
    server.shutdown().await;
}

#[tokio::test]
async fn archive_mute_and_inbox_follow_the_supplement() {
    let server = TestServer::start().await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    let (alice, bob) = (user("alice"), user("bob"));
    let waves = server.app().server();
    let one = create_wave(waves, &alice, "w+one", &[&bob], "First wave").await;
    tick().await;
    let two = create_wave(waves, &alice, "w+two", &[&bob], "Second wave").await;

    let folders = |cookie: &str| {
        let (server, cookie) = (&server, cookie.to_owned());
        async move {
            (
                found_for(server, &cookie, "in:inbox").await,
                found_for(server, &cookie, "in:archive").await,
                found_for(server, &cookie, "in:mute").await,
            )
        }
    };
    let none: [&str; 0] = [];
    assert_eq!(
        folders(&bob_cookie).await,
        (vec!["w+two".into(), "w+one".into()], vec![], vec![])
    );

    // Bob archives the first wave: out of the inbox, into the archive.
    archive(waves, &bob, &one).await;
    let (inbox, archived, muted) = folders(&bob_cookie).await;
    assert_eq!(inbox, ["w+two"]);
    assert_eq!(archived, ["w+one"]);
    assert_eq!(muted, none);
    // Every wave is still found without a folder, and under a folder name
    // this server gives no meaning (as the original does for any name).
    assert_eq!(
        found_for(&server, &bob_cookie, "").await,
        ["w+two", "w+one"]
    );
    assert_eq!(
        found_for(&server, &bob_cookie, "in:whatever").await,
        ["w+two", "w+one"]
    );
    // Two folders at once: both must hold.
    assert_eq!(
        found_for(&server, &bob_cookie, "in:inbox in:archive").await,
        none
    );
    // Alice's inbox is her own.
    assert_eq!(folders(&alice_cookie).await.0, ["w+two", "w+one"]);

    // A change to the wave brings it back to the inbox.
    tick().await;
    append_blip(waves, &alice, &one, "News").await;
    let (inbox, archived, _) = folders(&bob_cookie).await;
    assert_eq!(inbox, ["w+one", "w+two"]);
    assert_eq!(archived, none);

    // Muted waves are in neither.
    mute(waves, &bob, &two).await;
    let (inbox, archived, muted) = folders(&bob_cookie).await;
    assert_eq!(inbox, ["w+one"]);
    assert_eq!(archived, none);
    assert_eq!(muted, ["w+two"]);
    append_blip(waves, &alice, &two, "Still muted").await;
    assert_eq!(folders(&bob_cookie).await.2, ["w+two"]);
    follow(waves, &bob, &two).await;
    let (inbox, _, muted) = folders(&bob_cookie).await;
    assert_eq!(inbox, ["w+two", "w+one"]);
    assert_eq!(muted, none);
    server.shutdown().await;
}

// ---- SimpleSearchProviderImplTest ----

/// `submitDeltaToNewWavelet`: a root wavelet with just participants.
async fn new_wavelet(
    waves: &WaveServer,
    wave: &str,
    creator: &ParticipantId,
    participants: &[&ParticipantId],
) {
    let name = common::wavelet_name(wave, "conv+root");
    let ops = participants
        .iter()
        .map(|who| add_participant(creator, who))
        .collect();
    let version = HashedVersionFactory::create_version_zero(&name);
    waves
        .submit_request(&name, delta(creator, &version, ops))
        .await
        .expect("submit");
    tick().await;
}

async fn library_search(
    server: &TestServer,
    who: &ParticipantId,
    query: &str,
    start_at: usize,
    num_results: usize,
) -> SearchResult {
    server
        .app()
        .state()
        .search
        .search(who, query, start_at, num_results)
        .await
        .expect("search")
}

#[tokio::test]
async fn java_inbox_and_all_queries() {
    let server = TestServer::start().await;
    let waves = server.app().server();
    let (user1, user2) = (user("user1"), user("user2"));
    let shared = participant("@example.com");
    // testSearchEmptyInboxReturnsNothing
    assert!(
        library_search(&server, &user1, "in:inbox", 0, 20)
            .await
            .digests
            .is_empty()
    );
    // testSearchInboxReturnsWaveWithExplicitParticipant
    new_wavelet(waves, "w+explicit", &user1, &[&user2]).await;
    let result = library_search(&server, &user2, "in:inbox", 0, 20).await;
    assert_eq!(result.digests.len(), 1);
    assert_eq!(result.digests[0].wave_id, wave_id("w+explicit"));
    // testSearchInboxDoesNotReturnWaveWithoutUser
    new_wavelet(waves, "w+other", &user1, &[&user1]).await;
    assert_eq!(
        library_search(&server, &user2, "in:inbox", 0, 20)
            .await
            .digests
            .len(),
        1
    );
    // testSearchWaveReturnsWaveWithImplicitParticipant
    new_wavelet(waves, "w+shared", &user1, &[&shared]).await;
    let all = library_search(&server, &user2, "", 0, 20).await;
    let ids: BTreeSet<&str> = all.digests.iter().map(|d| d.wave_id.id()).collect();
    assert_eq!(ids, BTreeSet::from(["w+explicit", "w+shared"]));
    // The inbox has explicit participation only.
    assert_eq!(
        library_search(&server, &user2, "in:inbox", 0, 20)
            .await
            .digests
            .len(),
        1
    );
    // testSearchAllReturnsWavesOnlyWithSharedDomainUser: w+other is
    // neither shared nor user2's.
    assert_eq!(all.digests.len(), 2);
    assert_eq!(all.total_results, 2);
    // `with:@` names the shared domain participant.
    let with_shared = library_search(&server, &user2, "with:@", 0, 20).await;
    assert_eq!(with_shared.digests.len(), 1);
    assert_eq!(with_shared.digests[0].wave_id, wave_id("w+shared"));
    server.shutdown().await;
}

#[tokio::test]
async fn java_limit_index_and_orderings() {
    let server = TestServer::start().await;
    let waves = server.app().server();
    let (user1, user2) = (user("user1"), user("user2"));
    for i in 0..10 {
        // user1 creates waves 1 and 2 with user2 on them; user2 the rest.
        if i == 1 || i == 2 {
            new_wavelet(waves, &i.to_string(), &user1, &[&user1, &user2]).await;
        } else {
            new_wavelet(waves, &i.to_string(), &user2, &[&user2]).await;
        }
    }
    let ids = |result: &SearchResult| -> Vec<String> {
        result
            .digests
            .iter()
            .map(|d| d.wave_id.id().to_owned())
            .collect()
    };
    // testSearchLimitEnforced
    assert_eq!(
        library_search(&server, &user2, "in:inbox", 0, 5)
            .await
            .digests
            .len(),
        5
    );
    // testSearchIndexWorks: every wave exactly once over ten searches.
    let mut seen = Vec::new();
    for index in 0..10 {
        let result = library_search(&server, &user2, "in:inbox", index, 1).await;
        assert_eq!(result.digests.len(), 1);
        seen.push(ids(&result).remove(0));
    }
    seen.sort();
    assert_eq!(seen, (0..10).map(|i| i.to_string()).collect::<Vec<_>>());
    // Beyond the end there is nothing.
    assert!(
        library_search(&server, &user2, "in:inbox", 10, 5)
            .await
            .digests
            .is_empty()
    );
    assert!(
        library_search(&server, &user2, "in:inbox", 11, 5)
            .await
            .digests
            .is_empty()
    );

    // testSearchOrderBy{Asc,Desc,CreatedAsc,CreatedDesc}Works
    let ascending: Vec<String> = (0..10).map(|i| i.to_string()).collect();
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();
    for (query, expected) in [
        ("in:inbox orderby:dateasc", &ascending),
        ("in:inbox orderby:datedesc", &descending),
        ("in:inbox", &descending),
        ("in:inbox orderby:createdasc", &ascending),
        ("in:inbox orderby:createddesc", &descending),
    ] {
        let result = library_search(&server, &user2, query, 0, 10).await;
        assert_eq!(&ids(&result), expected, "{query}");
    }
    // testSearchOrderByAuthorAscWithCompundingWorks: user1's two waves
    // first, each group latest created first.
    let result = library_search(
        &server,
        &user2,
        "in:inbox orderby:creatorasc orderby:createddesc",
        0,
        10,
    )
    .await;
    assert_eq!(
        ids(&result),
        ["2", "1", "9", "8", "7", "6", "5", "4", "3", "0"]
    );
    // testSearchOrderByAuthorDescWorks
    let result = library_search(&server, &user2, "in:inbox orderby:creatordesc", 0, 10).await;
    assert_eq!(ids(&result)[8..], ["1", "2"]);
    assert_eq!(result.digests.len(), 10);

    // testSearchFilterByWithWorks
    let with = |who: &ParticipantId| format!("in:inbox with:{}", who.address());
    assert_eq!(
        library_search(&server, &user2, &with(&user1), 0, 10)
            .await
            .digests
            .len(),
        2
    );
    assert_eq!(
        library_search(&server, &user2, &with(&user2), 0, 10)
            .await
            .digests
            .len(),
        10
    );
    // A name without a domain is of the searcher's domain.
    assert_eq!(
        library_search(&server, &user2, "in:inbox with:user1", 0, 10)
            .await
            .digests
            .len(),
        2
    );
    // testSearchFilterByCreatorWorks
    let creator = |who: &ParticipantId| format!("creator:{}", who.address());
    let by = |a: &str| format!("in:inbox {a}");
    assert_eq!(
        library_search(&server, &user2, &by(&creator(&user1)), 0, 10)
            .await
            .digests
            .len(),
        2
    );
    assert_eq!(
        library_search(&server, &user2, &by(&creator(&user2)), 0, 10)
            .await
            .digests
            .len(),
        8
    );
    let both = format!("in:inbox {} {}", creator(&user1), creator(&user2));
    assert!(
        library_search(&server, &user2, &both, 0, 10)
            .await
            .digests
            .is_empty()
    );
    // testInvalidWithSearchParam, testInvalidOrderByParam: an invalid
    // query finds nothing.
    for invalid in [
        "in:inbox with@^^^@:user2@example.com",
        "in:inbox orderby:createddescCCC",
        "in:inbox with:a@b@c",
        "in:inbox unknown:token",
        "in:inbox id:not/a/wave/id",
    ] {
        let result = library_search(&server, &user2, invalid, 0, 10).await;
        assert!(result.digests.is_empty(), "{invalid}");
        assert_eq!(result.query, invalid);
    }
    // `id:` picks waves.
    let result = library_search(&server, &user2, "id:3 id:example.com/7 id:missing", 0, 10).await;
    assert_eq!(ids(&result), ["7", "3"]);
    server.shutdown().await;
}

#[tokio::test]
async fn pages_cover_the_result_once_and_report_totals() {
    let server = TestServer::start().await;
    let cookie = server.user("alice").await;
    let alice = user("alice");
    let waves = server.app().server();
    for i in 0..25 {
        create_wave(
            waves,
            &alice,
            &format!("w+{i:02}"),
            &[],
            &format!("Wave {i}"),
        )
        .await;
        tick().await;
    }
    let mut all = Vec::new();
    for index in [0, 10, 20] {
        let response = search_raw(&server, Some(&cookie), "in:inbox", index, 10).await;
        assert_eq!(response.headers()["x-resonator-search-total"], "25");
        let page: SearchResponse = from_json(&response.text().await.unwrap()).unwrap();
        // A full page may have more behind it: the total is unknown, as
        // in the original.
        let expected_total = if index < 20 { -1 } else { 25 };
        assert_eq!(page.total_results, Some(expected_total), "index {index}");
        all.extend(found(&page));
    }
    let expected: Vec<String> = (0..25).rev().map(|i| format!("w+{i:02}")).collect();
    assert_eq!(all, expected);
    // The client's way of paging: a growing page from the start.
    assert_eq!(
        found(&search_page(&server, &cookie, "in:inbox", 0, 20).await),
        expected[..20]
    );
    // Past the end, and nothing asked for.
    let past = search_page(&server, &cookie, "in:inbox", 30, 10).await;
    assert!(past.digests.is_empty());
    assert_eq!(past.total_results, Some(30));
    let none = search_page(&server, &cookie, "in:inbox", 0, 0).await;
    assert!(none.digests.is_empty());
    // Defaults: the whole (short) inbox.
    let response = server.get("/search/", Some(&cookie)).await;
    let default: SearchResponse = from_json(&response.text().await.unwrap()).unwrap();
    assert_eq!(default.digests.len(), 25);
    assert_eq!(default.query.as_deref(), Some(""));
    // Bad numbers are bad requests.
    for bad in [
        "/search/?index=-1",
        "/search/?numResults=x",
        "/search/?index=1e3",
    ] {
        assert_eq!(server.get(bad, Some(&cookie)).await.status(), 400, "{bad}");
    }
    server.shutdown().await;
}

#[tokio::test]
async fn shared_domain_waves_are_found_by_everyone_of_the_domain() {
    let server = TestServer::start().await;
    let (alice_cookie, carol_cookie) = (server.user("alice").await, server.user("carol").await);
    let alice = user("alice");
    let shared = participant("@example.com");
    let waves = server.app().server();
    create_wave(waves, &alice, "w+public", &[&shared], "Public notice").await;
    tick().await;
    create_wave(waves, &alice, "w+private", &[], "Private note").await;

    // Carol is on neither wave. Without `in:` she finds the shared one.
    let all = search(&server, &carol_cookie, "").await;
    assert_eq!(found(&all), ["w+public"]);
    assert_eq!(all.digests[0].title.as_deref(), Some("Public notice"));
    assert_eq!(all.digests[0].participants, ["@example.com"]);
    // It is not in her inbox: that is for waves she is a participant of.
    assert!(
        found_for(&server, &carol_cookie, "in:inbox")
            .await
            .is_empty()
    );
    assert_eq!(
        found_for(&server, &carol_cookie, "with:@").await,
        ["w+public"]
    );
    assert_eq!(
        found_for(&server, &carol_cookie, "creator:alice").await,
        ["w+public"]
    );
    assert!(
        found_for(&server, &carol_cookie, "creator:carol")
            .await
            .is_empty()
    );
    assert_eq!(
        found_for(&server, &alice_cookie, "").await,
        ["w+private", "w+public"]
    );

    // Carol's read state of a shared wave counts.
    assert_eq!(all.digests[0].unread_count, Some(1));
    mark_read(waves, &user("carol"), &wave_id("w+public")).await;
    let all = search(&server, &carol_cookie, "").await;
    assert_eq!(all.digests[0].unread_count, Some(0));

    // The wave stops being shared: Carol no longer finds it.
    let name = root_name(&wave_id("w+public"));
    let version = waves.get_snapshot(&name).await.unwrap().unwrap().snapshot;
    waves
        .submit_request(
            &name,
            delta(
                &alice,
                version.hashed_version(),
                vec![remove_participant(&alice, &shared)],
            ),
        )
        .await
        .unwrap();
    assert!(found_for(&server, &carol_cookie, "").await.is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn full_text_finds_words_in_any_script_and_only_what_one_may_read() {
    let server = TestServer::start().await;
    let (alice_cookie, bob_cookie, carol_cookie) = (
        server.user("alice").await,
        server.user("bob").await,
        server.user("carol").await,
    );
    let (alice, bob) = (user("alice"), user("bob"));
    let waves = server.app().server();
    let search_service = &server.app().state().search;
    assert!({
        search_service.flush().await;
        search_service.full_text_available()
    });

    create_wave(waves, &alice, "w+fox", &[&bob], "The quick brown fox").await;
    create_wave(
        waves,
        &alice,
        "w+cafe",
        &[&bob],
        "Gr\u{f6}\u{df}e und Caf\u{e9} na\u{ef}ve",
    )
    .await;
    create_wave(waves, &alice, "w+cjk", &[&bob], "\u{4e2d}\u{6587}\u{641c}\u{7d22}\u{6d4b}\u{8bd5}\u{3001}\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}").await;
    create_wave(
        waves,
        &alice,
        "w+ru",
        &[&bob],
        "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}, \u{43c}\u{438}\u{440}!",
    )
    .await;
    create_wave(
        waves,
        &alice,
        "w+party",
        &[&bob],
        "Party \u{1F389} tonight\u{1F600}!",
    )
    .await;
    create_wave(
        waves,
        &alice,
        "w+secret",
        &[],
        "The quick secret plan \u{1F389} \u{4e2d}\u{6587}",
    )
    .await;
    append_blip(waves, &alice, &wave_id("w+fox"), "jumps over the lazy dog").await;
    search_service.flush().await;

    let cases: &[(&str, &[&str])] = &[
        ("quick", &["w+fox"]),
        ("QUICK", &["w+fox"]),
        // A later blip of the wave.
        ("lazy", &["w+fox"]),
        // All terms must match.
        ("fox dog", &["w+fox"]),
        ("fox cat", &[]),
        // Prefixes and phrases.
        ("qui*", &["w+fox"]),
        ("qui", &[]),
        ("\"brown fox\"", &["w+fox"]),
        ("\"fox brown\"", &[]),
        // Diacritics do not matter.
        ("cafe", &["w+cafe"]),
        ("CAF\u{c9}", &["w+cafe"]),
        ("naive", &["w+cafe"]),
        ("gr\u{f6}\u{df}e", &["w+cafe"]),
        // Scripts without spaces: any run of characters.
        ("\u{641c}\u{7d22}", &["w+cjk"]),
        ("\u{4e2d}\u{6587}", &["w+cjk"]),
        ("\u{65e5}\u{672c}\u{8a9e}", &["w+cjk"]),
        ("\u{30c6}\u{30ad}\u{30b9}\u{30c8}", &["w+cjk"]),
        ("\u{6587}\u{4e2d}", &[]),
        ("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}", &["w+ru"]),
        ("\u{41c}\u{418}\u{420}", &["w+ru"]),
        // Emoji are words.
        ("\u{1F389}", &["w+party"]),
        ("\u{1F600}", &["w+party"]),
        ("party \u{1F389}", &["w+party"]),
        ("\u{1F680}", &[]),
        // Free text with structured tokens.
        ("in:inbox quick", &["w+fox"]),
        ("quick creator:alice with:bob", &["w+fox"]),
        ("quick creator:bob", &[]),
        ("in:archive quick", &[]),
        // Operators of the index's own query language are just text.
        ("quick OR nothing", &[]),
        ("NOT", &[]),
        ("title:quick", &[]),
        ("***", &[]),
    ];
    for (query, expected) in cases {
        let mut ids = found_for(&server, &bob_cookie, query).await;
        ids.sort();
        assert_eq!(&ids, expected, "bob searching '{query}'");
    }
    // Alice also finds her private wave.
    let mut ids = found_for(&server, &alice_cookie, "quick").await;
    ids.sort();
    assert_eq!(ids, ["w+fox", "w+secret"]);
    let mut ids = found_for(&server, &alice_cookie, "\u{1F389}").await;
    ids.sort();
    assert_eq!(ids, ["w+party", "w+secret"]);
    // Carol is on none of the waves and finds none, whatever she types.
    for query in [
        "quick",
        "secret",
        "\u{1F389}",
        "\u{4e2d}\u{6587}",
        "plan",
        "the*",
        "",
    ] {
        assert!(
            found_for(&server, &carol_cookie, query).await.is_empty(),
            "carol searching '{query}'"
        );
    }
    // Bob cannot find the secret wave by its words.
    assert!(found_for(&server, &bob_cookie, "secret").await.is_empty());
    assert!(found_for(&server, &bob_cookie, "plan").await.is_empty());

    // Text is indexed as it changes.
    assert!(found_for(&server, &bob_cookie, "zebra").await.is_empty());
    append_blip(waves, &bob, &wave_id("w+ru"), "A zebra appears").await;
    search_service.flush().await;
    assert_eq!(found_for(&server, &bob_cookie, "zebra").await, ["w+ru"]);
    // Without the explicit flush it follows within the debounce interval.
    append_blip(waves, &bob, &wave_id("w+ru"), "then an okapi").await;
    let mut tries = 0;
    while found_for(&server, &bob_cookie, "okapi").await.is_empty() {
        tries += 1;
        assert!(tries < 200, "text was never indexed");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    server.shutdown().await;
}

/// Opens the database the way no server does, to tamper with the index.
fn tamper(server: &TestServer, sql: &str) {
    let conn = rusqlite::Connection::open(server.dir.path().join("resonator.db")).expect("open");
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    conn.execute_batch(sql).expect("tamper");
}

#[tokio::test]
async fn a_wrong_index_never_shows_a_wave_one_may_not_read() {
    let server = TestServer::start().await;
    let (alice_cookie, mallory_cookie) = (server.user("alice").await, server.user("mallory").await);
    let alice = user("alice");
    let waves = server.app().server();
    create_wave(waves, &alice, "w+secret", &[], "Secret plan").await;
    server.app().state().search.flush().await;
    assert_eq!(
        found_for(&server, &alice_cookie, "plan").await,
        ["w+secret"]
    );
    assert!(found_for(&server, &mallory_cookie, "").await.is_empty());

    // The index claims Mallory is on every wavelet.
    tamper(
        &server,
        "INSERT OR IGNORE INTO search_participants (participant, wavelet) \
         SELECT 'mallory@example.com', id FROM search_wavelets;",
    );
    for query in [
        "",
        "in:inbox",
        "plan",
        "creator:alice",
        "id:w+secret",
        "with:alice",
    ] {
        let response = search_raw(&server, Some(&mallory_cookie), query, 0, 10).await;
        // The index did offer the wave ...
        assert_eq!(
            response.headers()["x-resonator-search-total"],
            "1",
            "'{query}'"
        );
        // ... and the look at the wavelet itself refused it.
        let page: SearchResponse = from_json(&response.text().await.unwrap()).unwrap();
        assert!(page.digests.is_empty(), "mallory searching '{query}'");
    }
    // The same for another user's user-data wavelet.
    mark_read(waves, &alice, &wave_id("w+secret")).await;
    assert!(found_for(&server, &mallory_cookie, "").await.is_empty());
    assert_eq!(found_for(&server, &alice_cookie, "").await, ["w+secret"]);
    server.shutdown().await;
}

#[tokio::test]
async fn the_index_survives_a_restart_and_catches_up_with_what_it_missed() {
    let server = TestServer::start().await;
    let (alice, bob) = (user("alice"), user("bob"));
    server.user("alice").await;
    server.user("bob").await;
    {
        let waves = server.app().server();
        create_wave(waves, &alice, "w+one", &[&bob], "First wave about apples").await;
        tick().await;
        create_wave(waves, &alice, "w+two", &[], "Second wave about pears").await;
        archive(waves, &alice, &wave_id("w+one")).await;
        server.app().state().search.flush().await;
    }
    let dir = server.stop().await;

    // A restart: the index is there, nothing is behind.
    let server = TestServer::start_in(dir, |_| {}).await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    assert_eq!(
        found_for(&server, &alice_cookie, "in:inbox").await,
        ["w+two"]
    );
    assert_eq!(
        found_for(&server, &alice_cookie, "in:archive").await,
        ["w+one"]
    );
    assert_eq!(found_for(&server, &bob_cookie, "apples").await, ["w+one"]);
    assert_eq!(
        server
            .app()
            .state()
            .search
            .indexed_wavelet_count()
            .await
            .unwrap(),
        3
    );
    let dir = server.stop().await;

    // Deltas are applied while no indexer runs: the wave server alone on
    // the database, as after a crash between a delta and its indexing.
    {
        let config = TestServer::config_in(&dir);
        let waves = resonator_server::app::start(&config)
            .await
            .expect("wave server");
        tick().await;
        // A new wave, a new blip, a participant added and one removed, and
        // an archived wave coming back through a change.
        create_wave(
            &waves,
            &bob,
            "w+three",
            &[&alice],
            "Third wave about cherries",
        )
        .await;
        tick().await;
        append_blip(&waves, &alice, &wave_id("w+one"), "now with quinces").await;
        tick().await;
        let two = root_name(&wave_id("w+two"));
        let version = waves.get_snapshot(&two).await.unwrap().unwrap().snapshot;
        waves
            .submit_request(
                &two,
                delta(
                    &alice,
                    version.hashed_version(),
                    vec![add_participant(&alice, &bob)],
                ),
            )
            .await
            .unwrap();
        resonator_server::app::shutdown(waves).await;
    }

    let server = TestServer::start_in(dir, |_| {}).await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    server.app().state().search.flush().await;
    assert_eq!(
        found_for(&server, &alice_cookie, "in:inbox").await,
        ["w+two", "w+one", "w+three"]
    );
    assert!(
        found_for(&server, &alice_cookie, "in:archive")
            .await
            .is_empty()
    );
    assert_eq!(
        found_for(&server, &bob_cookie, "in:inbox").await,
        ["w+two", "w+one", "w+three"]
    );
    assert_eq!(found_for(&server, &bob_cookie, "quinces").await, ["w+one"]);
    assert_eq!(
        found_for(&server, &bob_cookie, "cherries").await,
        ["w+three"]
    );
    assert_eq!(found_for(&server, &bob_cookie, "pears").await, ["w+two"]);
    let dir = server.stop().await;

    // The index is lost or damaged altogether: rebuilt from the wavelets.
    {
        let conn = rusqlite::Connection::open(dir.path().join("resonator.db")).unwrap();
        conn.execute_batch(
            "DELETE FROM search_wavelets WHERE wavelet_id = 'conv+root' AND wave_id = 'w+one'; \
             UPDATE search_wavelets SET version = version + 5 WHERE wave_id = 'w+two'; \
             UPDATE search_wavelets SET text_version = 0 WHERE wave_id = 'w+three'; \
             DELETE FROM search_text; \
             INSERT INTO search_wavelets (wave_domain, wave_id, wavelet_domain, wavelet_id, \
               version, creator, created, last_modified, kind, is_root, has_conversation) \
               VALUES ('example.com', 'w+ghost', 'example.com', 'conv+root', 3, \
               'alice@example.com', 1, 1, 1, 1, 1); \
             INSERT INTO search_participants (participant, wavelet) \
               SELECT 'alice@example.com', id FROM search_wavelets WHERE wave_id = 'w+ghost';",
        )
        .unwrap();
    }
    let server = TestServer::start_in(dir, |_| {}).await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    server.app().state().search.flush().await;
    assert_eq!(
        found_for(&server, &alice_cookie, "in:inbox").await,
        ["w+two", "w+one", "w+three"]
    );
    for (query, wave) in [
        ("quinces", "w+one"),
        ("cherries", "w+three"),
        ("pears", "w+two"),
    ] {
        assert_eq!(
            found_for(&server, &bob_cookie, query).await,
            [wave],
            "{query}"
        );
    }
    assert_eq!(
        server
            .app()
            .state()
            .search
            .indexed_wavelet_count()
            .await
            .unwrap(),
        4
    );
    server.shutdown().await;
}

#[tokio::test]
async fn search_without_full_text_still_serves_the_inbox() {
    let server = TestServer::start_with(|config| config.search_fulltext_enabled = false).await;
    let cookie = server.user("alice").await;
    let alice = user("alice");
    create_wave(server.app().server(), &alice, "w+one", &[], "Words to find").await;
    let search_service = &server.app().state().search;
    search_service.flush().await;
    assert!(!search_service.full_text_available());
    assert_eq!(found_for(&server, &cookie, "in:inbox").await, ["w+one"]);
    assert_eq!(
        found_for(&server, &cookie, "creator:alice").await,
        ["w+one"]
    );
    // Free text finds nothing rather than everything.
    assert!(found_for(&server, &cookie, "words").await.is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn changes_signal_tells_a_client_when_to_search_again() {
    let server = TestServer::start().await;
    let (alice_cookie, bob_cookie) = (server.user("alice").await, server.user("bob").await);
    let (alice, bob) = (user("alice"), user("bob"));
    let waves = server.app().server().clone();
    create_wave(&waves, &alice, "w+one", &[&bob], "First wave").await;
    // Indexing the text of a wave is a change too; let it happen first.
    server.app().state().search.flush().await;

    let changes = |cookie: &str, query: &str| {
        let request = server
            .http
            .get(server.url(&format!("/search/changes{query}")))
            .header("Cookie", cookie.to_owned());
        async move {
            let response = request.send().await.expect("changes request");
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.expect("changes body")
        }
    };
    // A search says which generation it saw.
    let response = search_raw(&server, Some(&bob_cookie), "in:inbox", 0, 10).await;
    let generation = response.headers()["x-resonator-search-generation"]
        .to_str()
        .unwrap()
        .to_owned();
    // Without `since`: the current generation, at once.
    let now = changes(&bob_cookie, "").await;
    assert_eq!(now["generation"], generation.as_str());
    assert_eq!(now["changed"], false);
    // Nothing happens: the wait times out unchanged.
    let started = std::time::Instant::now();
    let idle = changes(&bob_cookie, &format!("?since={generation}&timeout=1")).await;
    assert_eq!(idle["changed"], false);
    assert_eq!(idle["generation"], generation.as_str());
    assert!(started.elapsed() >= Duration::from_millis(900));

    // A change to a wave Bob is on wakes his waiting request ...
    let waiting = tokio::spawn(changes(
        &bob_cookie,
        &format!("?since={generation}&timeout=30"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    append_blip(&waves, &alice, &wave_id("w+one"), "A reply").await;
    let woken = waiting.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(woken["changed"], true);
    let newer = woken["generation"].as_str().unwrap().to_owned();
    assert_ne!(newer, generation);
    // ... and a stale generation is answered at once.
    let again = changes(&bob_cookie, &format!("?since={generation}&timeout=30")).await;
    assert_eq!(again["changed"], true);

    // A wave Bob is not on does not concern him; it does concern Alice.
    server.app().state().search.flush().await;
    let newer = changes(&bob_cookie, "").await["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let alice_generation = changes(&alice_cookie, "").await["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    create_wave(&waves, &alice, "w+private", &[], "Private").await;
    server.app().state().search.flush().await;
    let bob_now = changes(&bob_cookie, "").await["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(bob_now, newer);
    let alice_now = changes(
        &alice_cookie,
        &format!("?since={alice_generation}&timeout=5"),
    )
    .await;
    assert_eq!(alice_now["changed"], true);
    // Marking a wave read changes one's own results.
    mark_read(&waves, &bob, &wave_id("w+one")).await;
    let after_read = changes(&bob_cookie, &format!("?since={newer}&timeout=5")).await;
    assert_eq!(after_read["changed"], true);

    assert_eq!(server.get("/search/changes", None).await.status(), 403);
    assert_eq!(
        server
            .get("/search/changes?since=x&timeout=soon", Some(&bob_cookie))
            .await
            .status(),
        400
    );
    // A waiting request does not hold up shutdown.
    let since = changes(&bob_cookie, "").await["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = tokio::spawn(changes(&bob_cookie, &format!("?since={since}&timeout=60")));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    server.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(8));
    let _ = waiting.await;
}

#[tokio::test]
async fn notification_lists_digests_and_scopes_are_enforced() {
    let server = TestServer::start().await;
    let cookie = server.user("alice").await;
    let alice = user("alice");
    create_wave(server.app().server(), &alice, "w+one", &[], "First wave").await;

    let response = server
        .get("/notification/?query=in:inbox", Some(&cookie))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    let digests: Value = response.json().await.unwrap();
    let digests = digests.as_array().unwrap();
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0]["title"], "First wave");
    assert_eq!(digests[0]["waveId"], "example.com/w+one");
    assert_eq!(digests[0]["unreadCount"], 1);
    assert_eq!(digests[0]["blipCount"], 1);
    assert_eq!(
        digests[0]["participants"],
        serde_json::json!(["alice@example.com"])
    );
    assert!(digests[0]["lastModified"].as_i64().unwrap() > 0);
    assert!(digests[0]["created"].as_i64().unwrap() > 0);
    assert_eq!(server.get("/notification/", None).await.status(), 403);

    // A token needs the read scope to search.
    let (read, _) = server.create_token(&cookie, "reader", &["read"]).await;
    let (write, _) = server.create_token(&cookie, "writer", &["write"]).await;
    for path in [
        "/search/?query=in:inbox",
        "/notification/",
        "/search/changes",
    ] {
        assert_eq!(server.get_bearer(path, &read).await.status(), 200, "{path}");
        assert_eq!(
            server.get_bearer(path, &write).await.status(),
            403,
            "{path}"
        );
    }
    let found: SearchResponse = from_json(
        &server
            .get_bearer("/search/?query=in:inbox", &read)
            .await
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found.digests.len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn welcome_wave_is_delivered_to_new_accounts() {
    let server =
        TestServer::start_with(|config| config.welcome_wave_id = "w+welcome".to_owned()).await;
    let admin_cookie = server.user("admin").await;
    let admin = user("admin");
    let waves = server.app().server();
    // The address of the welcome robot is taken by the robot's account.
    let robot = user("welcome-bot");
    let accounts = server.app().state().auth.accounts().clone();
    for _ in 0..400 {
        if accounts.get_account(&robot).await.unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        accounts
            .get_account(&robot)
            .await
            .unwrap()
            .unwrap()
            .is_agent()
    );
    assert_ne!(
        server
            .register("welcome-bot", "some-password")
            .await
            .status(),
        200
    );
    // No template yet: registration works, no wave appears.
    let early_cookie = server.user("early").await;
    // The template, with an inline reply whose anchor must not be copied.
    let template = create_wave(waves, &admin, "w+welcome", &[], "Welcome to the wave!").await;
    waves::edit_root(waves, &admin, &template, |conversation| {
        let blip = conversation
            .read()
            .unwrap()
            .root_thread()
            .first_blip()
            .unwrap()
            .id()
            .to_owned();
        conversation
            .add_inline_reply_thread(&blip, 3)
            .expect("inline reply");
    })
    .await;

    let cookie = server.user("newcomer").await;
    let mut inbox = search(&server, &cookie, "in:inbox").await;
    for _ in 0..200 {
        if !inbox.digests.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        inbox = search(&server, &cookie, "in:inbox").await;
    }
    assert_eq!(inbox.digests.len(), 1, "the welcome wave arrives");
    let digest = &inbox.digests[0];
    assert_eq!(digest.title.as_deref(), Some("Welcome to the wave!"));
    assert_eq!(digest.author.as_deref(), Some("welcome-bot@example.com"));
    assert_eq!(digest.participants, ["newcomer@example.com"]);
    assert_eq!(digest.blip_count, Some(1));
    assert_eq!(digest.unread_count, Some(1));
    // A wave of its own, not the template.
    let id = digest.wave_id.as_deref().unwrap();
    assert_ne!(id, "example.com/w+welcome");
    let copy = WaveId::of(DOMAIN, id.strip_prefix("example.com/").unwrap()).unwrap();
    let root = waves::wavelet(waves, &root_name(&copy)).await;
    assert_eq!(root.creator().address(), "welcome-bot@example.com");
    let blips: Vec<&str> = root
        .document_ids()
        .filter(|id| id.starts_with("b+"))
        .collect();
    assert_eq!(blips.len(), 1);
    assert_eq!(
        common::doc_xml(&root, blips[0]),
        "<body><?a \"conv/title\"=\"\"?><line/>Welcome to the wave!<?a \"conv/title\"?></body>"
    );
    // The newcomer can read it; the admin, who is not on it, cannot find it.
    let name: WaveletName = root_name(&copy);
    assert!(
        server
            .app()
            .state()
            .frontend
            .check_access(&user("newcomer"), &name)
            .await
            .unwrap()
    );
    assert_eq!(found_for(&server, &admin_cookie, "").await, ["w+welcome"]);
    // Whoever registered before there was a template got nothing.
    assert!(found_for(&server, &early_cookie, "").await.is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn registration_succeeds_when_the_welcome_wave_cannot_be_made() {
    // The template does not exist.
    let server =
        TestServer::start_with(|config| config.welcome_wave_id = "w+missing".to_owned()).await;
    let cookie = server.user("alice").await;
    assert!(found_for(&server, &cookie, "").await.is_empty());
    server.shutdown().await;
    // The id is not a wave id at all.
    let server =
        TestServer::start_with(|config| config.welcome_wave_id = "not a wave id".to_owned()).await;
    let cookie = server.user("alice").await;
    assert!(found_for(&server, &cookie, "").await.is_empty());
    server.shutdown().await;
}
