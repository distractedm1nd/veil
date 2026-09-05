//! Sequential, bounded requests over the existing ztreamer P2P protocol.
use anyhow::{Context, Result, anyhow, ensure};
use prost::Message as _;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use zakura_network::zakura::{
    CustomService, Frame, FramedRecv, FramedSend, LOCAL_MAX_CONTROL_FRAME_BYTES, Peer, Service,
    Stream, StreamMode, ZakuraConnId, ZakuraPeerId, ZakuraServiceId,
};
use ztreamer_protocol::p2p::{self, Message, MessageDecoder, P2pStatus};

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const STREAMS: [Stream; 1] = [Stream {
    kind: p2p::STREAM_KIND,
    version: p2p::STREAM_VERSION,
    frame_cap: LOCAL_MAX_CONTROL_FRAME_BYTES,
    capability: p2p::CAPABILITY,
    mode: StreamMode::Ordered,
}];
#[derive(Debug)]
pub struct ProviderError(pub P2pStatus);
impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ztreamer error {}: {}", self.0.code, self.0.message)
    }
}
impl std::error::Error for ProviderError {}

struct Request {
    kind: Message,
    payload: Vec<u8>,
    limit: usize,
    reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
}
type Session = (ZakuraPeerId, ZakuraConnId, mpsc::Sender<Request>);

#[derive(Clone)]
pub struct CompactClient(watch::Receiver<Option<Session>>);
impl CompactClient {
    pub fn service() -> (Self, CustomService) {
        let (sessions, receiver) = watch::channel(None);
        let service = Arc::new(CompactService {
            sessions,
            generation: Mutex::new(None),
        });
        (
            Self(receiver),
            CustomService {
                service,
                provides: vec![],
                seeks: vec![
                    ZakuraServiceId::new(p2p::SERVICE_ID).expect("ztreamer service ID is valid"),
                ],
            },
        )
    }
    pub async fn request<Q: prost::Message, R: prost::Message + Default>(
        &self,
        kind: Message,
        request: Q,
        limit: usize,
    ) -> Result<Vec<R>> {
        ensure!(
            limit > 0 && limit <= 1000,
            "response limit must be 1..=1000"
        );
        let payload = request.encode_to_vec();
        ensure!(
            payload.len() <= p2p::MAX_MESSAGE_BYTES,
            "request exceeds protocol limit"
        );
        tokio::time::timeout(TIMEOUT, async {
            let mut sessions = self.0.clone();
            let sender = loop {
                if let Some((_, _, sender)) = sessions.borrow().clone() {
                    break sender;
                }
                sessions
                    .changed()
                    .await
                    .context("ztreamer service stopped")?;
            };
            let (reply, response) = oneshot::channel();
            sender
                .send(Request {
                    kind,
                    payload,
                    limit,
                    reply,
                })
                .await
                .map_err(|_| anyhow!("ztreamer disconnected"))?;
            response
                .await
                .context("ztreamer disconnected")??
                .into_iter()
                .map(|bytes| R::decode(bytes.as_slice()).map_err(Into::into))
                .collect()
        })
        .await
        .context("ztreamer request timed out")?
    }
    pub async fn unary<Q: prost::Message, R: prost::Message + Default>(
        &self,
        kind: Message,
        request: Q,
    ) -> Result<R> {
        let mut results = self.request(kind, request, 1).await?;
        ensure!(results.len() == 1, "expected one ztreamer response");
        Ok(results.remove(0))
    }
}
struct CompactService {
    sessions: watch::Sender<Option<Session>>,
    generation: Mutex<Option<(ZakuraPeerId, ZakuraConnId)>>,
}
impl std::fmt::Debug for CompactService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactService")
    }
}
impl Service for CompactService {
    fn name(&self) -> &'static str {
        "veil-ztreamer"
    }
    fn streams(&self) -> &[Stream] {
        &STREAMS
    }
    fn owns_connection_for_peer(&self, peer: &ZakuraPeerId, conn: ZakuraConnId) -> bool {
        self.generation
            .lock()
            .is_ok_and(|g| g.as_ref().is_some_and(|(p, c)| p == peer && *c == conn))
    }
    fn add_peer(&self, mut peer: Peer) {
        let Some((mut recv, send)) = peer.take_stream(p2p::STREAM_KIND) else {
            return;
        };
        let mut generation = self
            .generation
            .lock()
            .expect("generation mutex is not poisoned");
        // One provider at a time; a replacement becomes eligible after disconnect.
        if generation.is_some() {
            peer.service_cancel_token().cancel();
            return;
        }
        *generation = Some((peer.id.clone(), peer.conn_id));
        let (sender, mut requests) = mpsc::channel::<Request>(1);
        self.sessions
            .send_replace(Some((peer.id.clone(), peer.conn_id, sender)));
        let cancel = peer.cancel_token();
        let service_cancel = peer.service_cancel_token();
        tokio::spawn(async move {
            loop {
                let request = tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = service_cancel.cancelled() => break,
                    request = requests.recv() => match request { Some(r) => r, None => break },
                };
                let result = tokio::select! {
                    _ = cancel.cancelled() => Err(anyhow!("ztreamer disconnected")),
                    _ = service_cancel.cancelled() => Err(anyhow!("ztreamer service stopped")),
                    result = tokio::time::timeout(TIMEOUT, exchange(&mut recv, &send, &request)) => result.context("ztreamer exchange timed out").and_then(|r| r),
                };
                let failed = result.is_err();
                let _ = request.reply.send(result);
                if failed {
                    cancel.cancel();
                    break;
                }
            }
        });
    }
    fn remove_peer(&self, peer: &ZakuraPeerId, conn: ZakuraConnId) {
        let mut generation = self
            .generation
            .lock()
            .expect("generation mutex is not poisoned");
        if generation
            .as_ref()
            .is_some_and(|(p, c)| p == peer && *c == conn)
        {
            *generation = None;
            self.sessions.send_replace(None);
        }
    }
}
async fn exchange(
    recv: &mut FramedRecv,
    send: &FramedSend,
    request: &Request,
) -> Result<Vec<Vec<u8>>> {
    let chunk_size = LOCAL_MAX_CONTROL_FRAME_BYTES as usize - 8;
    let chunks = request.payload.len().div_ceil(chunk_size).max(1);
    for index in 0..chunks {
        let start = index * chunk_size;
        let end = (start + chunk_size).min(request.payload.len());
        send.send(Frame {
            message_type: request.kind.into(),
            flags: if index + 1 == chunks {
                0
            } else {
                p2p::FRAME_FLAG_MORE
            },
            payload: request.payload[start..end].to_vec(),
        })
        .await
        .map_err(|_| anyhow!("ztreamer disconnected"))?;
    }
    let streaming = matches!(
        request.kind,
        Message::GetBlockRangeRequest
            | Message::GetBlockRangeNullifiersRequest
            | Message::GetSubtreeRootsRequest
    );
    let expected = request.kind.response().context("invalid request kind")?;
    let mut decoder = MessageDecoder::default();
    let mut responses = Vec::new();
    let mut bytes = 0usize;
    loop {
        let frame = recv.recv().await.context("ztreamer disconnected")?;
        let Some((kind, payload)) = decoder
            .push(frame.message_type, frame.flags, frame.payload)
            .map_err(|e| anyhow!(e))?
        else {
            continue;
        };
        if kind == Message::ErrorResponse {
            let status = P2pStatus::decode(payload.as_slice())?;
            return Err(ProviderError(status).into());
        }
        if streaming && kind == Message::StreamEnd {
            ensure!(payload.is_empty(), "invalid stream terminator");
            return Ok(responses);
        }
        ensure!(kind == expected, "unexpected ztreamer response {kind:?}");
        bytes = bytes
            .checked_add(payload.len())
            .context("response size overflow")?;
        ensure!(
            responses.len() < request.limit && bytes <= MAX_RESPONSE_BYTES,
            "ztreamer response exceeds requested bounds"
        );
        responses.push(payload);
        if !streaming {
            return Ok(responses);
        }
    }
}
