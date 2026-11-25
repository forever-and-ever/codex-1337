//! `OpenRouter` UI actions retain their menu origin across server round trips.

use crate::model_catalog::ModelListing;
use codex_app_server_protocol::ModelProviderOption;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_protocol::EndpointFilter;
use codex_protocol::KeyName;
use codex_protocol::ModelSlug;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;

pub(crate) const VIEW: &str = "openrouter-models";
pub(crate) const ENDPOINTS_VIEW: &str = "openrouter-endpoints";
pub(crate) const BUSY_VIEW: &str = "openrouter-busy";
pub(crate) const ACCOUNT_SELECTION_VIEW: &str = "openrouter-account-selection";
pub(crate) const MODEL_FOOTER: &str = "enter select · ctrl+p endpoints · ctrl+d forget · esc back";

#[derive(Clone, Debug)]
pub(crate) enum ModelMenu {
    Current,
    New(ModelProviderOption),
}

impl ModelMenu {
    pub(crate) fn title(&self) -> &str {
        match self {
            Self::Current => "OpenRouter models",
            Self::New(_) => "New OpenRouter conversation",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Request {
    Models {
        menu: ModelMenu,
        selected: Option<ModelSlug>,
    },
    Save {
        menu: ModelMenu,
        slug: ModelSlug,
    },
    Forget {
        menu: ModelMenu,
        slug: ModelSlug,
    },
    Endpoints {
        menu: ModelMenu,
        slug: ModelSlug,
    },
    Route {
        menu: ModelMenu,
        slug: ModelSlug,
        filter: EndpointFilter,
    },
    AddKey {
        thread_id: ThreadId,
        name: KeyName,
        path: AbsolutePathBuf,
    },
    RemoveKey {
        thread_id: ThreadId,
        name: KeyName,
    },
}

impl Request {
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Models { menu, .. } | Self::Forget { menu, .. } => menu.title().to_owned(),
            Self::Save { .. } => "Add model…".into(),
            Self::Endpoints { slug, .. } | Self::Route { slug, .. } => {
                format!("Endpoints · {slug}")
            }
            Self::AddKey { .. } => "Add OpenRouter key…".into(),
            Self::RemoveKey { name, .. } => format!("Remove OpenRouter key · {name}"),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Reply {
    Models(ModelListing),
    Endpoints(OpenRouterModelEndpointsResponse),
    Saved(ModelSlug),
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusRefresh {
    Show,
    Now,
    AfterTurn,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StatusFailure {
    #[error("OpenRouter key {0} was removed; choose a key with /account")]
    Removed(KeyName),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug)]
pub(crate) enum Event {
    SelectTransport {
        thread_id: ThreadId,
        model: Option<String>,
        transport: codex_protocol::openrouter::OpenRouterTransport,
    },
    TransportSelected {
        thread_id: ThreadId,
        result: Result<codex_protocol::openrouter::OpenRouterTransport, String>,
    },
    Request(Request),
    Finished {
        id: uuid::Uuid,
        thread_id: Option<ThreadId>,
        request: Request,
        result: Result<Reply, String>,
    },
    AddModel {
        menu: ModelMenu,
    },
    KeyNamePrompt {
        thread_id: ThreadId,
    },
    KeyPathPrompt {
        thread_id: ThreadId,
        name: KeyName,
    },
    RemoveKeyConfirm {
        thread_id: ThreadId,
        name: KeyName,
    },
    RefreshStatus {
        thread_id: ThreadId,
        id: uuid::Uuid,
        mode: StatusRefresh,
    },
    StatusLoaded {
        thread_id: ThreadId,
        id: uuid::Uuid,
        mode: StatusRefresh,
        result: Result<OpenRouterKeyStatus, StatusFailure>,
    },
    ModelSubmitted {
        menu: ModelMenu,
        input: String,
    },
    KeyNameSubmitted {
        thread_id: ThreadId,
        input: String,
    },
    KeyFileSubmitted {
        thread_id: ThreadId,
        name: KeyName,
        input: String,
    },
    Error(String),
}

pub(crate) fn title_route<'a>(
    provider_id: &'a str,
    model: &'a str,
    title_model: &'a str,
    has_chatgpt_account: bool,
) -> Option<(&'a str, &'a str)> {
    if provider_id == codex_model_provider_info::OPENROUTER_PROVIDER_ID {
        // Automatic titles are Codex-initiated work, never billable OpenRouter requests.
        has_chatgpt_account.then_some((codex_model_provider_info::OPENAI_PROVIDER_ID, title_model))
    } else {
        Some((provider_id, model))
    }
}

pub(crate) fn error_message(error: &anyhow::Error) -> String {
    match error.downcast_ref::<codex_app_server_client::TypedRequestError>() {
        Some(codex_app_server_client::TypedRequestError::Server { source, .. }) => {
            source.message.clone()
        }
        _ => error.to_string(),
    }
}
