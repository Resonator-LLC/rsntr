// The network layer of the client: the HTTP endpoints of the wave server
// and the WebSocket RPC channel.
//
// Takes the place of the transport classes of
// `org.waveprotocol.box.webclient.client` in Apache Wave, licensed under the
// Apache License, Version 2.0. Everything here runs in the browser and in
// Node, with `fetch`, the WebSocket, timers, randomness and the network
// status injectable.

export { CookieJar, withCookieJar } from "./cookieJar";
export {
  AuthError,
  ClosedError,
  DisconnectedError,
  HttpError,
  InvalidResponseError,
  NetworkError,
  RpcFailedError,
  RpcFatalError,
  RpcProtocolError,
  type AuthErrorDetail,
  type CloseInfo,
  type RpcFatalDetail,
  type RpcProtocolErrorDetail,
} from "./errors";
export {
  CONTACT_LINK_PREFIX,
  HttpAttachmentService,
  HttpSearchService,
  HttpTransport,
  WaveHttpApi,
  sessionAuthToken,
  socketUrlFromAddress,
  socketUrlFromBaseUrl,
  type AttachmentService,
  type FetchResult,
  type RegisterResult,
  type SearchAnswer,
  SEARCH_ERROR_HEADER,
  type SearchService,
  type DirectoryEntry,
  type ProbeResult,
  type DeviceInfo,
  type SessionInfo,
  type SignInResult,
  type WaveHttpApiOptions,
} from "./http";
export {
  PAIR_LINK_PREFIX,
  PersonApi,
  PersonApiError,
  isPairLink,
  parseOverview,
  parsePairing,
  personErrorText,
  type Contact,
  type HostStatus,
  type JoinStage,
  type NewPairing,
  type Pairing,
  type PersonCoHost,
  type PersonDevice,
  type PersonLease,
  type PersonOverview,
} from "./person";
export {
  CODE_SHAPE_ERROR,
  codeGroups,
  codeHint,
  completeCodeWord,
  findCodeWord,
  looksLikePairLink,
  parsePairCode,
  suggestCodeWords,
  type PairCodeParts,
  type PairCodeReading,
} from "./pairCode";
export {
  RpcChannel,
  defaultNetworkStatus,
  type BackoffOptions,
  type CallHandle,
  type ConnectionState,
  type LogLevel,
  type Logger,
  type NetworkStatus,
  type RpcChannelEvents,
  type RpcChannelOptions,
  type SocketFactory,
  type SocketLike,
  type StateChange,
  type StreamHandle,
  type StreamHandlers,
  type TimerApi,
} from "./rpcChannel";
