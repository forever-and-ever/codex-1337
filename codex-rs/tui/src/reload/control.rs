//! Private, bounded local control messages; never part of the app-server protocol.

use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_client::RemoteAuthToken;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

const MAX_MESSAGE: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) enum Endpoint {
    Unix(AbsolutePathBuf),
    WebSocket { url: String, token: Option<String> },
}

impl From<RemoteAppServerEndpoint> for Endpoint {
    fn from(endpoint: RemoteAppServerEndpoint) -> Self {
        match endpoint {
            RemoteAppServerEndpoint::UnixSocket { socket_path } => Self::Unix(socket_path),
            RemoteAppServerEndpoint::WebSocket {
                websocket_url,
                auth_token,
            } => Self::WebSocket {
                url: websocket_url,
                // Only the broker's transport copy crosses this wire. The UI keeps
                // the original endpoint, including any CLI provenance.
                token: auth_token.map(|token| token.as_str().to_owned()),
            },
        }
    }
}

impl From<Endpoint> for RemoteAppServerEndpoint {
    fn from(endpoint: Endpoint) -> Self {
        match endpoint {
            Endpoint::Unix(socket_path) => Self::UnixSocket { socket_path },
            Endpoint::WebSocket {
                url: websocket_url,
                token: auth_token,
            } => Self::WebSocket {
                websocket_url,
                auth_token: auth_token.map(RemoteAuthToken::from_secret),
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Envelope {
    pub(super) generation: u64,
    pub(super) request: Request,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Request {
    Hello {
        protocol: String,
        #[serde(default)]
        rejection_handoff: bool,
    },
    Connect(Endpoint),
    Reload(ReloadPayload),
    Ready,
    Suspend,
}

impl Request {
    pub(super) fn supervisor_owns_wait(&self) -> bool {
        matches!(self, Self::Reload(_) | Self::Suspend)
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct State {
    pub(super) checkpoint: Value,
    pub(super) rejected_candidate: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct IdleReload {
    pub(super) checkpoint: Value,
    pub(super) idle_candidate: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum ReloadPayload {
    Idle(IdleReload),
    Manual(Value),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum StatePayload {
    Handoff(State),
    Legacy(Value),
}

#[derive(Serialize, Deserialize)]
pub(super) enum Response {
    State(Option<StatePayload>),
    Ok,
    Error(String),
}

pub(super) struct Received {
    pub(super) envelope: Envelope,
    pub(super) reply: oneshot::Sender<Response>,
}

pub(super) async fn listen(
    listener: UnixListener,
    events: mpsc::Sender<Received>,
) -> io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let events = events.clone();
        tokio::spawn(async move {
            let Ok(Ok(envelope)) = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 10),
                read::<Envelope>(&mut stream),
            )
            .await
            else {
                return;
            };
            let supervisor_owns_wait = envelope.request.supervisor_owns_wait();
            let operation = async {
                let (reply, response) = oneshot::channel();
                events
                    .send(Received { envelope, reply })
                    .await
                    .map_err(io::Error::other)?;
                let response = response.await.map_err(io::Error::other)?;
                write(&mut stream, &response).await
            };
            if supervisor_owns_wait {
                // Only the supervisor can release a reload's input ownership or
                // resume job control. A second timer could release the writer early.
                let _ = operation.await;
            } else {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 10), operation)
                        .await;
            }
        });
    }
}

pub(super) async fn request(directory: &Path, envelope: Envelope) -> io::Result<Response> {
    let path = directory.join("control.sock");
    let mut stream = UnixStream::connect(&path).await.map_err(|error| {
        let path = path.display();
        io::Error::new(
            error.kind(),
            format!("Failed to connect to the live-reload host socket {path}: {error}"),
        )
    })?;
    write(&mut stream, &envelope).await?;
    read(&mut stream).await
}

/// Job control is synchronous: the renderer deliberately waits until the shell
/// resumes its host. This is not a reload wait and is never admitted during reload.
pub(super) fn suspend(directory: &Path, generation: u64) -> io::Result<()> {
    use std::io::Read;
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(directory.join("control.sock"))?;
    let bytes = serde_json::to_vec(&Envelope {
        generation,
        request: Request::Suspend,
    })?;
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&bytes)?;
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(io::Error::other("UI host message too large"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    match serde_json::from_slice(&bytes)? {
        Response::Ok => Ok(()),
        Response::Error(message) => Err(io::Error::other(message)),
        Response::State(_) => Err(io::Error::other("Unexpected suspend response")),
    }
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let len = stream.read_u32().await? as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UI host message too large",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

async fn write(stream: &mut UnixStream, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UI host message too large",
        ));
    }
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn rejection_handoff_is_opt_in_and_distinct_from_legacy_checkpoints() -> serde_json::Result<()>
    {
        let Request::Hello {
            rejection_handoff, ..
        } = serde_json::from_value::<Request>(
            json!({"Hello": {"protocol": super::super::PROTOCOL}}),
        )?
        else {
            panic!("expected Hello")
        };
        assert!(!rejection_handoff);

        // Bare v1 checkpoints must never be mistaken for either directional wrapper.
        let checkpoint = json!({"primary": null, "session_draft": null});
        let Request::Reload(ReloadPayload::Manual(legacy)) =
            serde_json::from_value::<Request>(json!({"Reload": checkpoint}))?
        else {
            panic!("expected manual reload")
        };
        assert_eq!(legacy, checkpoint);
        assert_eq!(
            serde_json::to_value(Response::State(Some(StatePayload::Legacy(legacy))))?,
            json!({"State": checkpoint}),
        );
        let Response::State(Some(StatePayload::Legacy(legacy))) =
            serde_json::from_value::<Response>(json!({"State": checkpoint}))?
        else {
            panic!("expected legacy state")
        };
        assert_eq!(legacy, checkpoint);

        let candidate = PathBuf::from("/install/distributions/rejected/bin/codex");
        let Request::Reload(ReloadPayload::Idle(attempt)) = serde_json::from_value::<Request>(
            json!({"Reload": {"checkpoint": checkpoint, "idle_candidate": candidate}}),
        )?
        else {
            panic!("expected idle reload")
        };
        assert_eq!(attempt.checkpoint, checkpoint);
        assert_eq!(attempt.idle_candidate, candidate);
        let response = serde_json::to_value(Response::State(Some(StatePayload::Handoff(State {
            checkpoint: attempt.checkpoint,
            rejected_candidate: Some(attempt.idle_candidate),
        }))))?;
        assert_eq!(
            response,
            json!({"State": {
                "checkpoint": checkpoint, "rejected_candidate": candidate,
            }})
        );
        let Response::State(Some(StatePayload::Handoff(response))) =
            serde_json::from_value::<Response>(response)?
        else {
            panic!("expected rejection handoff")
        };
        assert_eq!(response.rejected_candidate, Some(candidate));

        // An initial Hello can advertise support without claiming a retained draft.
        let Response::State(Some(StatePayload::Handoff(initial))) =
            serde_json::from_value::<Response>(json!({"State": {"checkpoint": null}}))?
        else {
            panic!("expected initial handoff")
        };
        assert!(initial.checkpoint.is_null());
        assert_eq!(initial.rejected_candidate, None);

        // Checkpoint transport must not erase v1/v2 input while adopting a new UI.
        // Exercise every retained slot together: only the reload boundary accepts
        // legacy records, and every later serialization is canonical v3.
        let text_element = codex_protocol::user_input::TextElement::new(
            codex_protocol::user_input::ByteRange { start: 0, end: 4 },
            Some("keep".into()),
        );
        let wire_text_element = codex_app_server_protocol::TextElement::from(text_element.clone());
        let message = json!({
            "text": "keep [Paste1] [Image #1]",
            "local_images": [{"placeholder": "[Image #1]", "path": "/legacy-image.png"}],
            "remote_image_urls": ["data:image/png;base64,cXVldWU="],
            "text_elements": [text_element],
            "mention_bindings": [{"sigil": "$", "mention": "proof", "path": "skill:///legacy-proof"}],
        });
        let mut composer = message.clone();
        composer["pending_pastes"] = json!([["[Paste1]", "pasted payload"]]);
        let history = json!({"Override": {"text": "/goal keep", "text_elements": []}});
        let legacy_input = json!({
            "questions": null, "pending_thread_settings": null, "composer": composer,
            "safety_buffering_prompt": message, "safety_buffering_source": "QuestionAnswer",
            "pending_steer": {
                "client_id": "steer-id",
                "items": [{"type": "text", "text": message["text"], "text_elements": [wire_text_element]}],
                "user_message": message, "history_record": history, "source": "QuestionAnswer",
                "compare_key": {"message": message["text"], "image_count": 2},
            },
            "rejected_steers_queue": [message], "rejected_steer_sources": ["QuestionAnswer"],
            "rejected_steer_history_records": [history],
            "queued_user_messages": [{
                "user_message": message, "action": "Literal",
                "delivery": {"Unconfirmed": "uncertain-id"},
                "pending_pastes": [["[Paste1]", "pasted payload"]], "source": "QuestionAnswer",
            }],
            "queued_user_message_history_records": [history],
            "recovered_queue": true, "reconnect_pending": true,
            "user_turn_pending_start": true, "submit_pending_steer_after_interrupt": true,
            "pending_user_message_client_id": "exact-id",
            "current_collaboration_mode": {
                "mode": "default",
                "settings": {"model": "test-model", "reasoning_effort": null, "developer_instructions": null},
            },
            "active_collaboration_mask": null, "plan_mode_reasoning_effort": null,
            "task_running": true, "agent_turn_running": true,
        });
        let thread_id = codex_protocol::ThreadId::new().to_string();
        let mut operation = legacy_input.clone();
        // Compact/review/shell after finalization can retain the previous stale ID.
        operation["safety_buffering_prompt"] = Value::Null;
        let mut prepared = legacy_input.clone();
        prepared["pending_user_message_client_id"] = Value::Null;
        let mut retained_retry = prepared.clone();
        retained_retry["user_turn_pending_start"] = json!(false);
        let legacy_checkpoint = json!({
            "primary": null, "overview": false, "draft": legacy_input,
            "unstarted": {"provider": "test-provider", "model": "test-model", "cwd": "/legacy", "input": retained_retry},
            "kill_buffer": {"text": "", "kind": "Characterwise"}, "terminal": null, "cwd": null,
            "threads": {thread_id.clone(): operation}, "detached": {thread_id.clone(): prepared},
        });
        let version = super::super::retained::checkpoint_version;
        assert_eq!(version(&legacy_checkpoint).unwrap(), 2);
        let mut v1 = legacy_checkpoint.clone();
        v1.as_object_mut().unwrap().remove("unstarted");
        assert_eq!(version(&v1).unwrap(), 1);
        let old: super::super::RetainedInput = serde_json::from_value(v1)?;
        assert_eq!(serde_json::to_value(old)?["draft"]["composer"], composer);

        let migrated: super::super::RetainedInput =
            serde_json::from_value(legacy_checkpoint.clone())?;
        let canonical = serde_json::to_value(migrated)?;
        for input in [
            &canonical["draft"],
            &canonical["threads"][&thread_id],
            &canonical["detached"][&thread_id],
            &canonical["unstarted"]["input"],
        ] {
            assert_eq!(input["composer"], composer);
            assert_eq!(
                input["input_queue"]["queued_user_messages"][0]["history_record"],
                history
            );
            assert_eq!(
                input["input_queue"]["queued_user_messages"][0]["delivery"],
                json!({"Unconfirmed": "uncertain-id"})
            );
            assert_eq!(
                input["input_queue"]["queued_user_messages"][0]["pending_pastes"],
                composer["pending_pastes"]
            );
            assert_eq!(
                input["input_queue"]["rejected_steers_queue"][0]["source"],
                "QuestionAnswer"
            );
            assert_eq!(
                input["input_queue"]["pending_steer"]["Pending"]["steer"]["client_id"],
                "steer-id"
            );
            assert_eq!(
                input["input_queue"]["pending_steer"]["Pending"]["interrupt"],
                "SubmitAsTurn"
            );
            assert_eq!(input["input_queue"]["recovered_queue"], true);
        }
        assert_eq!(
            canonical["draft"]["input_queue"]["turn"]["AwaitingStart"]["Submitted"]["client_id"],
            "exact-id"
        );
        assert_eq!(
            canonical["threads"][&thread_id]["input_queue"]["turn"],
            json!({"AwaitingStart": "Operation"})
        );
        assert_eq!(
            canonical["detached"][&thread_id]["input_queue"]["turn"]["AwaitingStart"]["PreparedRetry"]
                ["message"],
            message
        );
        assert_eq!(
            canonical["unstarted"]["input"]["input_queue"]["turn"]["RetainedRetry"]["message"],
            message
        );
        assert_eq!(version(&canonical).unwrap(), 3);
        let roundtrip: super::super::RetainedInput = serde_json::from_value(canonical.clone())?;
        assert_eq!(serde_json::to_value(roundtrip)?, canonical);
        for slot in ["draft", "threads", "detached", "unstarted"] {
            let mut mixed = legacy_checkpoint.clone();
            mixed[slot] = canonical[slot].clone();
            assert_eq!(version(&mixed).unwrap(), 3);
        }
        // Finalization leaves an unrelated retired ID; unavailable recovery leaves
        // the same ID already owned by an uncertain queue entry. Neither is a live
        // prompt, and normalizing either must preserve the entire recovered draft.
        for stale_id in ["retired-id", "uncertain-id"] {
            let mut finalized = legacy_checkpoint.clone();
            finalized["draft"]["safety_buffering_prompt"] = Value::Null;
            finalized["draft"]["user_turn_pending_start"] = json!(false);
            finalized["draft"]["pending_user_message_client_id"] = json!(stale_id);
            let migrated: super::super::RetainedInput = serde_json::from_value(finalized)?;
            let finalized = serde_json::to_value(migrated)?;
            assert_eq!(finalized["draft"]["input_queue"]["turn"], "Idle");
            assert_eq!(finalized["draft"]["composer"], composer);
            assert_eq!(
                finalized["draft"]["input_queue"]["queued_user_messages"],
                canonical["draft"]["input_queue"]["queued_user_messages"]
            );
        }
        let mut unknown = legacy_checkpoint.clone();
        unknown["threads"][&thread_id]["unknown_input_fact"] = json!(true);
        assert!(serde_json::from_value::<super::super::RetainedInput>(unknown).is_err());
        let mut orphaned = legacy_checkpoint;
        orphaned["draft"]["queued_user_message_history_records"]
            .as_array_mut()
            .unwrap()
            .push(history);
        assert!(serde_json::from_value::<super::super::RetainedInput>(orphaned).is_err());
        let mut unknown = canonical;
        unknown["unstarted"]["input"]["input_queue"]["unknown_input_fact"] = json!(true);
        assert!(serde_json::from_value::<super::super::RetainedInput>(unknown).is_err());
        Ok(())
    }
}
