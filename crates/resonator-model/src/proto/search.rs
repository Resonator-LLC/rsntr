//! Messages of `box/search/search.proto` (package `search`).

use super::wave_message;

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SearchRequest {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub query: Option<String>,
    /// Required.
    #[prost(int32, optional, tag = "2")]
    pub index: Option<i32>,
    /// Required.
    #[prost(int32, optional, tag = "3")]
    pub num_results: Option<i32>,
}
wave_message!(SearchRequest {
    required: [query, index, num_results],
});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SearchResponse {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub query: Option<String>,
    /// Required. The number of results if known, else -1.
    #[prost(int32, optional, tag = "2")]
    pub total_results: Option<i32>,
    #[prost(message, repeated, tag = "3")]
    pub digests: Vec<search_response::Digest>,
}
wave_message!(SearchResponse {
    required: [query, total_results],
    repeated: [digests],
});

pub mod search_response {
    use super::super::wave_message;

    #[derive(Clone, PartialEq, Eq, prost::Message)]
    pub struct Digest {
        /// Required.
        #[prost(string, optional, tag = "1")]
        pub title: Option<String>,
        /// Required.
        #[prost(string, optional, tag = "2")]
        pub snippet: Option<String>,
        /// Required. The serialised wave id.
        #[prost(string, optional, tag = "3")]
        pub wave_id: Option<String>,
        /// Required. Milliseconds since the epoch.
        #[prost(int64, optional, tag = "4")]
        pub last_modified: Option<i64>,
        /// Required.
        #[prost(int32, optional, tag = "5")]
        pub unread_count: Option<i32>,
        /// Required.
        #[prost(int32, optional, tag = "6")]
        pub blip_count: Option<i32>,
        /// The participants other than the author.
        #[prost(string, repeated, tag = "7")]
        pub participants: Vec<String>,
        /// Required. The first participant.
        #[prost(string, optional, tag = "8")]
        pub author: Option<String>,
        /// The searcher's own labels on the wave (ensembles). Not in the
        /// original.
        #[prost(int32, repeated, tag = "9")]
        pub labels: Vec<i32>,
    }
    wave_message!(Digest {
        required: [
            title,
            snippet,
            wave_id,
            last_modified,
            unread_count,
            blip_count,
            author
        ],
    });
}
