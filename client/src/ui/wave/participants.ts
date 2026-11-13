// Turning what a user types into the "add participant" box into
// participant ids.
//
// A port of `ParticipantController.buildParticipantList` of the Apache Wave
// web client (`org.waveprotocol.wave.client.wavepanel.impl.edit`, Apache
// License 2.0).

import { InvalidParticipantAddress, ParticipantId } from "../../model/id";

/**
 * The participants named by a comma-separated list of addresses. A name
 * without a domain gets the local domain; `@` alone is the shared-domain
 * participant of the local domain. Throws `InvalidParticipantAddress` for
 * an address that is not valid (also for an empty entry).
 */
export function buildParticipantList(localDomain: string | null, addresses: string): ParticipantId[] {
  return addresses.split(",").map((entry) => {
    let address = entry.trim();
    if (localDomain !== null) {
      if (address !== "" && !address.includes("@")) {
        address = `${address}@${localDomain}`;
      } else if (address === "@") {
        address = `${address}${localDomain}`;
      }
    }
    return ParticipantId.of(address);
  });
}

export type ParticipantInput =
  | { readonly ok: true; readonly participants: ParticipantId[] }
  | { readonly ok: false; readonly message: string };

/** {@link buildParticipantList} with the failure as a message for the user. */
export function parseParticipantInput(localDomain: string | null, addresses: string): ParticipantInput {
  try {
    return { ok: true, participants: buildParticipantList(localDomain, addresses) };
  } catch (error) {
    if (error instanceof InvalidParticipantAddress) {
      return { ok: false, message: error.message };
    }
    throw error;
  }
}
