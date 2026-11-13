// Messages of `box/profile/profiles.proto` (package `profile`), with their
// JSON codecs.
//
// Port of `crates/resonator-model/src/proto/profile.rs`; derived from Apache
// Wave, licensed under the Apache License, Version 2.0.

import { type MessageCodec, defineMessage, field } from "./json";

export interface ProfileRequest {
  readonly addresses: readonly string[];
}

export const ProfileRequest: MessageCodec<ProfileRequest> = defineMessage("ProfileRequest", [
  field(1, "addresses", "repeated", "string"),
]);

/** `ProfileResponse.FetchedProfile`. */
export interface FetchedProfile {
  readonly address: string;
  readonly name: string;
  readonly imageUrl: string;
  readonly profileUrl?: string;
}

export const FetchedProfile: MessageCodec<FetchedProfile> = defineMessage("FetchedProfile", [
  field(1, "address", "required", "string"),
  field(2, "name", "required", "string"),
  field(3, "imageUrl", "required", "string"),
  field(4, "profileUrl", "optional", "string"),
]);

export interface ProfileResponse {
  readonly profiles: readonly FetchedProfile[];
}

export const ProfileResponse: MessageCodec<ProfileResponse> = defineMessage("ProfileResponse", [
  field(1, "profiles", "repeated", FetchedProfile),
]);
