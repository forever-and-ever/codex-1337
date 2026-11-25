pub use crate::sse::responses::batch_response_events;
use codex_protocol::EndpointFilter;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterRouting(pub EndpointFilter);

impl Serialize for OpenRouterRouting {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Routing<'a> {
            sort: &'static str,
            #[serde(flatten)]
            filter: &'a EndpointFilter,
        }
        Routing {
            sort: "exacto",
            filter: &self.0,
        }
        .serialize(serializer)
    }
}
