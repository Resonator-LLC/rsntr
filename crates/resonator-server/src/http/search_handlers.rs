//! The search endpoints.
//!
//! `search` and `notification` follow `SearchServlet`,
//! `AbstractSearchServlet` and `NotificationServlet` of Apache Wave
//! (Apache License 2.0); `changes` is new.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use resonator_model::digest::Digest;
use resonator_model::id::{IdSerialiser, ModernIdSerialiser};
use resonator_model::proto::json::to_json;
use resonator_model::proto::search::SearchResponse;
use resonator_model::proto::search::search_response::Digest as ProtoDigest;
use serde_json::json;

use super::{AppState, Context, failure, internal_error, json};
use crate::search::{SearchProvider, SearchResult};

type Params = Query<HashMap<String, String>>;

/// Results asked for when the request does not say
/// (`AbstractSearchServlet.DEFAULT_NUMRESULTS`).
const DEFAULT_NUM_RESULTS: usize = 100;

/// The total reported when there may be more results than were returned
/// (`SearchService.UNKNOWN_SIZE`).
const UNKNOWN_SIZE: i32 = -1;

/// Longest and default wait of `/search/changes`, in seconds.
const MAX_CHANGES_WAIT_SECS: u64 = 60;
const DEFAULT_CHANGES_WAIT_SECS: u64 = 30;

/// The header carrying the caller's search generation; see
/// [`crate::search::SearchService::generation`].
pub(crate) const GENERATION_HEADER: &str = "x-resonator-search-generation";
/// The header carrying the number of waves the index has for the query.
pub(crate) const TOTAL_HEADER: &str = "x-resonator-search-total";

/// The wave id as the original's `ApiIdSerializer` writes it.
fn wave_id_text(digest: &Digest) -> String {
    ModernIdSerialiser
        .serialise_wave_id(&digest.wave_id)
        .unwrap_or_else(|_| format!("{}/{}", digest.wave_id.domain(), digest.wave_id.id()))
}

/// `SearchServlet.serializeDigest`: the first participant is the author.
fn proto_digest(digest: &Digest) -> ProtoDigest {
    let narrow = |count: usize| i32::try_from(count).unwrap_or(i32::MAX);
    let (author, participants) = match digest.participants.split_first() {
        Some((author, others)) => (author.clone(), others.to_vec()),
        // "This shouldn't be possible."
        None => ("nobody@example.com".to_owned(), Vec::new()),
    };
    ProtoDigest {
        title: Some(digest.title.clone()),
        snippet: Some(digest.snippet.clone()),
        wave_id: Some(wave_id_text(digest)),
        last_modified: Some(digest.last_modified),
        unread_count: Some(narrow(digest.unread_count)),
        blip_count: Some(narrow(digest.blip_count)),
        participants,
        author: Some(author),
    }
}

/// `SearchServlet.serializeSearchResult` with
/// `computeTotalResultsNumberGuess`: if as many results came back as were
/// asked for there may be more, and the total is unknown; otherwise it is
/// where the results end.
pub(crate) fn search_response(
    result: &SearchResult,
    index: usize,
    num_results: usize,
) -> SearchResponse {
    let total = if result.digests.len() >= num_results {
        UNKNOWN_SIZE
    } else {
        i32::try_from(index.saturating_add(result.digests.len())).unwrap_or(i32::MAX)
    };
    SearchResponse {
        query: Some(result.query.clone()),
        total_results: Some(total),
        digests: result.digests.iter().map(proto_digest).collect(),
    }
}

struct Request {
    query: String,
    index: usize,
    num_results: usize,
}

/// `AbstractSearchServlet.parseSearchRequest`. The original fails with a
/// server error on numbers that do not parse or are negative; here that is
/// a bad request, and `numResults` is capped.
fn parse_request(params: &HashMap<String, String>, max_results: usize) -> Result<Request, String> {
    let number = |name: &str, default: usize| match params.get(name) {
        None => Ok(default),
        Some(text) => text
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("{name} must be a non-negative number")),
    };
    Ok(Request {
        query: params.get("query").cloned().unwrap_or_default(),
        index: number("index", 0)?,
        num_results: number("numResults", DEFAULT_NUM_RESULTS)?.min(max_results),
    })
}

async fn run(
    state: &AppState,
    context: &Context,
    params: &HashMap<String, String>,
) -> Result<(Request, SearchResult, String), Response> {
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return Err(StatusCode::FORBIDDEN.into_response());
    };
    let request = parse_request(params, state.config.search_max_results)
        .map_err(|message| failure(StatusCode::BAD_REQUEST, message))?;
    // Read before searching, so that a change during the search shows as
    // a newer generation later.
    let generation = state.search.generation(&identity.participant);
    let result = state
        .search
        .search(
            &identity.participant,
            &request.query,
            request.index,
            request.num_results,
        )
        .await
        .map_err(internal_error)?;
    Ok((request, result, generation))
}

fn with_search_headers(mut response: Response, generation: &str, total: usize) -> Response {
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(generation) {
        headers.insert(HeaderName::from_static(GENERATION_HEADER), value);
    }
    headers.insert(
        HeaderName::from_static(TOTAL_HEADER),
        HeaderValue::from(total),
    );
    response
}

/// `GET /search/?query=…&index=…&numResults=…`: the digests of the waves
/// matching the query, as a `SearchResponse` in the protocol's JSON form.
/// 403 unless signed in. The `X-Resonator-Search-Generation` header carries the
/// token to hand to `/search/changes`.
pub(super) async fn search(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
) -> Response {
    let (request, result, generation) = match run(&state, &context, &params).await {
        Ok(done) => done,
        Err(response) => return response,
    };
    let response = search_response(&result, request.index, request.num_results);
    match to_json(&response) {
        Ok(text) => with_search_headers(
            (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                text,
            )
                .into_response(),
            &generation,
            result.total_results,
        ),
        Err(error) => internal_error(error),
    }
}

/// `GET /notification/?query=…&index=…&numResults=…`: the same search, as
/// the plain JSON list of digests that `NotificationServlet` writes with
/// Gson (for notifiers outside the client). The original labels it
/// `text/html`; here it is `application/json`.
pub(super) async fn notification(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
) -> Response {
    let (_, result, generation) = match run(&state, &context, &params).await {
        Ok(done) => done,
        Err(response) => return response,
    };
    let digests: Vec<serde_json::Value> = result
        .digests
        .iter()
        .map(|digest| {
            json!({
                "title": digest.title,
                "snippet": digest.snippet,
                "waveId": wave_id_text(digest),
                "lastModified": digest.last_modified,
                "created": digest.created,
                "unreadCount": digest.unread_count,
                "blipCount": digest.blip_count,
                "participants": digest.participants,
            })
        })
        .collect();
    with_search_headers(
        json(StatusCode::OK, serde_json::Value::Array(digests)),
        &generation,
        result.total_results,
    )
}

/// `GET /search/changes?since=<generation>&timeout=<seconds>`: answers
/// when the caller's search generation is no longer `since`, or after the
/// timeout (30 seconds unless given, 60 at most), with
/// `{"generation": "...", "changed": true|false}`. Without `since` it
/// answers at once. A client holds this request open instead of polling
/// `/search/` and searches again when `changed` is true.
pub(super) async fn changes(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
) -> Response {
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let participant = &identity.participant;
    let Some(since) = params.get("since") else {
        let generation = state.search.generation(participant);
        return json(
            StatusCode::OK,
            json!({"generation": generation, "changed": false}),
        );
    };
    let timeout = match params.get("timeout").map(|text| text.trim().parse::<u64>()) {
        None => DEFAULT_CHANGES_WAIT_SECS,
        Some(Ok(seconds)) => seconds.min(MAX_CHANGES_WAIT_SECS),
        Some(Err(_)) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "timeout must be a number of seconds",
            );
        }
    };
    let generation = tokio::select! {
        generation = state.search.changes(participant, since, Duration::from_secs(timeout)) => generation,
        () = state.shutting_down() => state.search.generation(participant),
    };
    let changed = generation != *since;
    json(
        StatusCode::OK,
        json!({"generation": generation, "changed": changed}),
    )
}

#[cfg(test)]
mod tests {
    use resonator_model::id::WaveId;

    use super::*;

    fn digest(id: &str, participants: &[&str]) -> Digest {
        Digest {
            title: "Title".to_owned(),
            snippet: "snippet".to_owned(),
            wave_id: WaveId::of("example.com", id).unwrap(),
            participants: participants.iter().map(|p| (*p).to_owned()).collect(),
            last_modified: 1234,
            created: 1000,
            unread_count: 2,
            blip_count: 3,
        }
    }

    fn result(count: usize) -> SearchResult {
        SearchResult {
            query: "in:inbox".to_owned(),
            digests: (0..count)
                .map(|i| digest(&format!("w+{i}"), &["a@example.com", "b@example.com"]))
                .collect(),
            total_results: count,
        }
    }

    #[test]
    fn the_total_is_a_guess_as_in_the_original() {
        // As many results as asked for: there may be more.
        assert_eq!(search_response(&result(5), 0, 5).total_results, Some(-1));
        assert_eq!(search_response(&result(5), 10, 3).total_results, Some(-1));
        // Fewer: the results end here.
        assert_eq!(search_response(&result(2), 10, 5).total_results, Some(12));
        assert_eq!(search_response(&result(0), 0, 5).total_results, Some(0));
        // Asking for nothing always leaves the total unknown.
        assert_eq!(search_response(&result(0), 0, 0).total_results, Some(-1));
    }

    #[test]
    fn the_first_participant_is_the_author() {
        let response = search_response(&result(1), 0, 10);
        assert_eq!(
            to_json(&response).unwrap(),
            "{\"1\":\"in:inbox\",\"2\":1,\"3\":[{\"1\":\"Title\",\"2\":\"snippet\",\
             \"3\":\"example.com/w+0\",\"4\":[1234,0],\"5\":2,\"6\":3,\
             \"7\":[\"b@example.com\"],\"8\":\"a@example.com\"}]}"
        );
        let nobody = proto_digest(&digest("w+x", &[]));
        assert_eq!(nobody.author.as_deref(), Some("nobody@example.com"));
        assert!(nobody.participants.is_empty());
    }

    #[test]
    fn requests_are_parsed_with_the_original_defaults() {
        let params = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let request = parse_request(&params(&[]), 1000).unwrap();
        assert_eq!(
            (request.query.as_str(), request.index, request.num_results),
            ("", 0, 100)
        );
        let request = parse_request(
            &params(&[
                ("query", "in:inbox"),
                ("index", "20"),
                ("numResults", "5000"),
            ]),
            1000,
        )
        .unwrap();
        assert_eq!(
            (request.query.as_str(), request.index, request.num_results),
            ("in:inbox", 20, 1000)
        );
        for bad in [("index", "-1"), ("numResults", "many"), ("index", "1.5")] {
            assert!(parse_request(&params(&[bad]), 1000).is_err(), "{bad:?}");
        }
    }
}
