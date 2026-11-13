// Wavelet state: participants, documents and their metadata.
//
// Derived from `org.waveprotocol.wave.model.wave.data` (`WaveletDataImpl`,
// `BlipDataImpl`) of Apache Wave, licensed under the Apache License,
// Version 2.0. Listener plumbing is not ported.
//
// Documents are held as `BootstrapDocument`s. Unlike the original, which
// iterates documents in hash-map order, documents are kept ordered by id
// (UTF-16 code unit order, the native string order).

import { BootstrapDocument, type DocInitialization, OperationError } from "../docop";
import { type ParticipantId, type WaveId, type WaveletId, WaveletName } from "../id";
import type { HashedVersion } from "../version";
import { DataError } from "./error";

function indexOfParticipant(list: readonly ParticipantId[], participant: ParticipantId): number {
  return list.findIndex((p) => p.equals(participant));
}

function participantsEqual(a: readonly ParticipantId[], b: readonly ParticipantId[]): boolean {
  return a.length === b.length && a.every((p, i) => p.equals(b[i]!));
}

function list(participants: readonly ParticipantId[]): string {
  return `[${participants.map((p) => p.address).join(", ")}]`;
}

/**
 * A document in a wavelet with its metadata. Blips and data documents are
 * both represented this way. Created by {@link WaveletData.createDocument}.
 */
export class BlipData {
  readonly id: string;
  readonly author: ParticipantId;
  /** The document content; mutated in place with `content.consume(op)`. */
  readonly content: BootstrapDocument;
  /** Insertion-ordered set. */
  private contributorList: ParticipantId[];
  private modifiedTime: number;
  private modifiedVersion: number;

  constructor(
    id: string,
    author: ParticipantId,
    contributors: Iterable<ParticipantId>,
    content: BootstrapDocument,
    lastModifiedTime: number,
    lastModifiedVersion: number,
  ) {
    this.id = id;
    this.author = author;
    this.content = content;
    this.contributorList = [];
    this.modifiedTime = lastModifiedTime;
    this.modifiedVersion = lastModifiedVersion;
    for (const contributor of contributors) {
      this.addContributor(contributor);
    }
  }

  /** Contributors in the order they were added. */
  get contributors(): readonly ParticipantId[] {
    return this.contributorList;
  }

  /** Milliseconds since the epoch. */
  get lastModifiedTime(): number {
    return this.modifiedTime;
  }

  get lastModifiedVersion(): number {
    return this.modifiedVersion;
  }

  hasContributor(participant: ParticipantId): boolean {
    return indexOfParticipant(this.contributorList, participant) >= 0;
  }

  /** Adds a contributor unless already present. */
  addContributor(participant: ParticipantId): void {
    if (!this.hasContributor(participant)) {
      this.contributorList.push(participant);
    }
  }

  removeContributor(participant: ParticipantId): void {
    this.contributorList = this.contributorList.filter((p) => !p.equals(participant));
  }

  /** Returns the previous value. */
  setLastModifiedTime(newTime: number): number {
    const old = this.modifiedTime;
    this.modifiedTime = newTime;
    return old;
  }

  /** Returns the previous value. */
  setLastModifiedVersion(newVersion: number): number {
    const old = this.modifiedVersion;
    this.modifiedVersion = newVersion;
    return old;
  }

  /** An independent copy, including the content. */
  clone(): BlipData {
    return new BlipData(
      this.id,
      this.author,
      this.contributorList,
      this.content.clone(),
      this.modifiedTime,
      this.modifiedVersion,
    );
  }

  equals(other: BlipData): boolean {
    return (
      this.id === other.id &&
      this.author.equals(other.author) &&
      participantsEqual(this.contributorList, other.contributorList) &&
      this.content.equals(other.content) &&
      this.modifiedTime === other.modifiedTime &&
      this.modifiedVersion === other.modifiedVersion
    );
  }

  toString(): string {
    return (
      `Blip state = [id:${this.id}] [author: ${this.author}] ` +
      `[contributors: ${list(this.contributorList)}] ` +
      `[lastModifiedVersion:${this.modifiedVersion}] [lastModifiedTime:${this.modifiedTime}]`
    );
  }
}

/** The state of a wavelet. */
export class WaveletData {
  readonly waveId: WaveId;
  readonly waveletId: WaveletId;
  readonly creator: ParticipantId;
  /** Milliseconds since the epoch. */
  readonly creationTime: number;
  private currentVersion: number;
  private currentHashedVersion: HashedVersion;
  private modifiedTime: number;
  /** Insertion-ordered set. */
  private participantList: ParticipantId[] = [];
  private readonly documentMap = new Map<string, BlipData>();
  /** The keys of `documentMap` in UTF-16 order. */
  private sortedIds: string[] = [];

  /**
   * Creates a wavelet without participants or documents. `version` and
   * `hashedVersion` are stored independently, as in the original.
   */
  constructor(
    waveletId: WaveletId,
    creator: ParticipantId,
    creationTime: number,
    version: number,
    hashedVersion: HashedVersion,
    lastModifiedTime: number,
    waveId: WaveId,
  ) {
    this.waveId = waveId;
    this.waveletId = waveletId;
    this.creator = creator;
    this.creationTime = creationTime;
    this.currentVersion = version;
    this.currentHashedVersion = hashedVersion;
    this.modifiedTime = lastModifiedTime;
  }

  /**
   * An empty wavelet at `version`, last modified at its creation time
   * (`WaveletDataUtil.createEmptyWavelet`).
   */
  static empty(
    name: WaveletName,
    creator: ParticipantId,
    version: HashedVersion,
    creationTime: number,
  ): WaveletData {
    return new WaveletData(
      name.waveletId,
      creator,
      creationTime,
      version.version,
      version,
      creationTime,
      name.waveId,
    );
  }

  get waveletName(): WaveletName {
    return WaveletName.of(this.waveId, this.waveletId);
  }

  get version(): number {
    return this.currentVersion;
  }

  get hashedVersion(): HashedVersion {
    return this.currentHashedVersion;
  }

  /** Milliseconds since the epoch. */
  get lastModifiedTime(): number {
    return this.modifiedTime;
  }

  /** Participants in the order they were added. */
  get participants(): readonly ParticipantId[] {
    return this.participantList;
  }

  hasParticipant(participant: ParticipantId): boolean {
    return indexOfParticipant(this.participantList, participant) >= 0;
  }

  /** The position of a participant, or -1. */
  indexOfParticipant(participant: ParticipantId): number {
    return indexOfParticipant(this.participantList, participant);
  }

  /** Appends a participant. Returns false if already present. */
  addParticipant(participant: ParticipantId): boolean {
    if (this.hasParticipant(participant)) {
      return false;
    }
    this.participantList.push(participant);
    return true;
  }

  /**
   * Inserts a participant at `position`. Returns false if already present;
   * throws {@link DataError} if `position` is beyond the end.
   */
  addParticipantAt(participant: ParticipantId, position: number): boolean {
    if (this.hasParticipant(participant)) {
      return false;
    }
    const size = this.participantList.length;
    if (!Number.isInteger(position) || position < 0 || position > size) {
      throw new DataError({ kind: "positionOutOfBounds", position, size });
    }
    this.participantList.splice(position, 0, participant);
    return true;
  }

  /** Returns false if the participant was not present. */
  removeParticipant(participant: ParticipantId): boolean {
    const index = indexOfParticipant(this.participantList, participant);
    if (index < 0) {
      return false;
    }
    this.participantList.splice(index, 1);
    return true;
  }

  /** Returns the previous value. */
  setVersion(newVersion: number): number {
    const old = this.currentVersion;
    this.currentVersion = newVersion;
    return old;
  }

  /** Returns the previous value. */
  setHashedVersion(newHashedVersion: HashedVersion): HashedVersion {
    const old = this.currentHashedVersion;
    this.currentHashedVersion = newHashedVersion;
    return old;
  }

  /** Returns the previous value. */
  setLastModifiedTime(newTime: number): number {
    const old = this.modifiedTime;
    this.modifiedTime = newTime;
    return old;
  }

  /**
   * Creates a document. Throws {@link DataError} if the id is taken or the
   * content is invalid. Duplicate contributors are dropped.
   */
  createDocument(
    docId: string,
    author: ParticipantId,
    contributors: Iterable<ParticipantId>,
    content: DocInitialization,
    lastModifiedTime: number,
    lastModifiedVersion: number,
  ): BlipData {
    if (this.documentMap.has(docId)) {
      throw new DataError({ kind: "duplicateDocument", docId });
    }
    const document = new BootstrapDocument();
    try {
      document.consume(content);
    } catch (e) {
      if (e instanceof OperationError) {
        throw new DataError({ kind: "content", error: e });
      }
      throw e;
    }
    const blip = new BlipData(
      docId,
      author,
      contributors,
      document,
      lastModifiedTime,
      lastModifiedVersion,
    );
    this.insertDocument(blip);
    return blip;
  }

  private insertDocument(blip: BlipData): void {
    this.documentMap.set(blip.id, blip);
    let low = 0;
    let high = this.sortedIds.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (this.sortedIds[mid]! < blip.id) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    this.sortedIds.splice(low, 0, blip.id);
  }

  /** The document with the given id, or null. */
  document(docId: string): BlipData | null {
    return this.documentMap.get(docId) ?? null;
  }

  hasDocument(docId: string): boolean {
    return this.documentMap.has(docId);
  }

  /** Document ids in UTF-16 order. */
  documentIds(): readonly string[] {
    return this.sortedIds;
  }

  /** Documents ordered by id. */
  documents(): BlipData[] {
    return this.sortedIds.map((id) => this.documentMap.get(id)!);
  }

  /**
   * Whether `user` may access this wavelet: an explicit participant, or
   * anyone if the shared domain participant is one
   * (`WaveletDataUtil.checkAccessPermission` for an existing wavelet).
   */
  checkAccessPermission(
    user: ParticipantId,
    sharedDomainParticipant: ParticipantId | null = null,
  ): boolean {
    return (
      this.hasParticipant(user) ||
      (sharedDomainParticipant !== null && this.hasParticipant(sharedDomainParticipant))
    );
  }

  /** An independent deep copy. */
  clone(): WaveletData {
    const copy = new WaveletData(
      this.waveletId,
      this.creator,
      this.creationTime,
      this.currentVersion,
      this.currentHashedVersion,
      this.modifiedTime,
      this.waveId,
    );
    copy.participantList = [...this.participantList];
    for (const [id, blip] of this.documentMap) {
      copy.documentMap.set(id, blip.clone());
    }
    copy.sortedIds = [...this.sortedIds];
    return copy;
  }

  equals(other: WaveletData): boolean {
    return (
      this.waveId.equals(other.waveId) &&
      this.waveletId.equals(other.waveletId) &&
      this.creator.equals(other.creator) &&
      this.creationTime === other.creationTime &&
      this.currentVersion === other.currentVersion &&
      this.currentHashedVersion.equals(other.currentHashedVersion) &&
      this.modifiedTime === other.modifiedTime &&
      participantsEqual(this.participantList, other.participantList) &&
      this.sortedIds.length === other.sortedIds.length &&
      this.sortedIds.every((id) => {
        const theirs = other.documentMap.get(id);
        return theirs !== undefined && this.documentMap.get(id)!.equals(theirs);
      })
    );
  }

  toString(): string {
    let s =
      `WaveletDataImpl: ${this.waveId}/${this.waveletId} \n[version:${this.currentVersion}] ` +
      `\n[creator: ${this.creator}] \n[participants: ${list(this.participantList)}] ` +
      `\n[creation time: ${this.creationTime}] \n[lastModifiedTime:${this.modifiedTime}]\n[documents:`;
    for (const blip of this.documents()) {
      s += ` \n  [${blip.id}: ${blip}]`;
    }
    return `${s} \n]`;
  }
}
