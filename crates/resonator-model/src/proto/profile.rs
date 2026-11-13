//! Messages of `box/profile/profiles.proto` (package `profile`).

use super::wave_message;

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProfileRequest {
    #[prost(string, repeated, tag = "1")]
    pub addresses: Vec<String>,
}
wave_message!(ProfileRequest {});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProfileResponse {
    #[prost(message, repeated, tag = "1")]
    pub profiles: Vec<profile_response::FetchedProfile>,
}
wave_message!(ProfileResponse {
    repeated: [profiles],
});

pub mod profile_response {
    use super::super::wave_message;

    #[derive(Clone, PartialEq, Eq, prost::Message)]
    pub struct FetchedProfile {
        /// Required.
        #[prost(string, optional, tag = "1")]
        pub address: Option<String>,
        /// Required.
        #[prost(string, optional, tag = "2")]
        pub name: Option<String>,
        /// Required.
        #[prost(string, optional, tag = "3")]
        pub image_url: Option<String>,
        #[prost(string, optional, tag = "4")]
        pub profile_url: Option<String>,
    }
    wave_message!(FetchedProfile {
        required: [address, name, image_url],
    });
}
