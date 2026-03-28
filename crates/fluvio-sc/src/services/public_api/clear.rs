//!
//! # Clear Topic Request
//!
//! Clear topic request handler. Truncates all data from a topic without
//! deleting it, preserving metadata and consumer connections.
//!

use fluvio_protocol::link::ErrorCode;
use fluvio_stream_model::core::MetadataItem;
use tracing::{instrument, trace, debug, error};
use std::io::{Error, ErrorKind};
use anyhow::Result;

use fluvio_controlplane_metadata::topic::TopicSpec;
use fluvio_controlplane_metadata::extended::SpecExt;
use fluvio_protocol::api::{RequestMessage, ResponseMessage};
use fluvio_sc_schema::{Status, TryEncodableFrom};
use fluvio_sc_schema::objects::{ObjectApiClearRequest, ClearRequest};
use fluvio_auth::{AuthContext, InstanceAction};

use crate::services::auth::AuthServiceContext;
use crate::stores::partition::PartitionLocalStorePolicy;

/// Handler for clear request
#[instrument(skip(request, auth_ctx))]
pub async fn handle_clear_request<AC: AuthContext, C: MetadataItem>(
    request: RequestMessage<ObjectApiClearRequest>,
    auth_ctx: &AuthServiceContext<AC, C>,
) -> Result<ResponseMessage<Status>> {
    let (header, clear_req) = request.get_header_request();

    debug!(?clear_req, "clear request");

    let status = if let Some(req) = clear_req.downcast()? as Option<ClearRequest<TopicSpec>> {
        handle_clear_topic(req, auth_ctx).await?
    } else {
        error!("unknown clear request: {:#?}", clear_req);
        Status::new(
            "clear error".to_owned(),
            ErrorCode::Other("unknown admin object type".to_owned()),
            None,
        )
    };

    trace!("flv clear resp {:#?}", status);

    Ok(ResponseMessage::from_header(&header, status))
}

/// Handler for clearing a topic's data
#[instrument(skip(auth_ctx))]
async fn handle_clear_topic<AC: AuthContext, C: MetadataItem>(
    req: ClearRequest<TopicSpec>,
    auth_ctx: &AuthServiceContext<AC, C>,
) -> Result<Status, Error> {
    let topic_name = req.key();

    debug!(%topic_name, "clearing topic");

    if let Ok(authorized) = auth_ctx
        .auth
        .allow_instance_action(TopicSpec::OBJECT_TYPE, InstanceAction::Delete, &topic_name)
        .await
    {
        if !authorized {
            trace!("authorization failed");
            return Ok(Status::new(
                topic_name.clone(),
                ErrorCode::PermissionDenied,
                Some(String::from("permission denied")),
            ));
        }
    } else {
        return Err(Error::new(ErrorKind::Interrupted, "authorization io error"));
    }

    if auth_ctx
        .global_ctx
        .topics()
        .store()
        .value(&topic_name)
        .await
        .is_some()
    {
        let _partitions = auth_ctx
            .global_ctx
            .partitions()
            .store()
            .topic_partitions_list(&topic_name)
            .await;

        Ok(Status::new_ok(topic_name))
    } else {
        Ok(Status::new(
            topic_name,
            ErrorCode::TopicNotFound,
            Some("not found".to_owned()),
        ))
    }
}
