//! Shared schedule operation handlers used by transports.
//!
//! Every schedule resource is owned by exactly one namespace: the authorized
//! namespace is force-stamped into the schedule config before the engine
//! records `ScheduleCreated`, and every targeted operation verifies that
//! creation-recorded owner through the namespace guard before any engine
//! method can run. List results are filtered to the authorized namespace by
//! the same stamped attribute.

use aion_proto::{
    ProtoCreateScheduleRequest, ProtoCreateScheduleResponse, ProtoDeleteScheduleResponse,
    ProtoDescribeScheduleResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoPauseScheduleResponse, ProtoResumeScheduleResponse, ProtoScheduleIdRequest,
    ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse, WireError,
    convert::{ProtoScheduleId, decode_schedule_config, encode_schedule_state},
};

use crate::namespace::ScheduleTarget;
use crate::{CallerIdentity, NamespaceGuard, NamespaceOperation, ServerError};

/// Force the authorized namespace onto a schedule config so every triggered
/// execution is stamped with it; any caller-supplied value is overwritten to
/// prevent cross-tenant spoofing through the schedule wire envelope.
fn stamp_schedule_namespace(
    mut config: aion_core::ScheduleConfig,
    namespace: &str,
) -> aion_core::ScheduleConfig {
    config.search_attributes.insert(
        crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
        aion_core::SearchAttributeValue::String(namespace.to_owned()),
    );
    config
}

/// Whether a projected schedule state is stamped as owned by the namespace.
///
/// Unstamped or non-string-stamped schedules match no namespace and are
/// therefore invisible through every namespaced server API — there is no
/// default namespace and no migration path for schedules created outside the
/// server's stamping boundary.
fn schedule_in_namespace(state: &aion::schedule::ScheduleState, namespace: &str) -> bool {
    matches!(
        state
            .config
            .search_attributes
            .get(crate::namespace::NAMESPACE_ATTRIBUTE),
        Some(aion_core::SearchAttributeValue::String(owner)) if owner == namespace
    )
}

fn required_schedule_id(id: Option<ProtoScheduleId>) -> Result<aion_core::ScheduleId, WireError> {
    id.ok_or_else(|| WireError::invalid_input("schedule id is missing"))?
        .try_into()
}

fn required_schedule_config(
    config: Option<&aion_proto::WireEnvelope>,
) -> Result<aion_core::ScheduleConfig, WireError> {
    config
        .ok_or_else(|| WireError::invalid_input("schedule config is missing"))
        .and_then(decode_schedule_config)
}

/// Handles a decoded create-schedule request.
///
/// The schedule id is server-generated, so creation never targets an existing
/// resource; the authorized namespace is stamped into the recorded config and
/// becomes the schedule's immutable owner.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule config is missing or malformed, namespace
/// scoping fails, or the engine create/describe call fails.
pub async fn create_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoCreateScheduleRequest,
) -> Result<ProtoCreateScheduleResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::create_schedule(&request))
        .await
        .map_err(|error| error.to_wire_error())?;
    let config = stamp_schedule_namespace(
        required_schedule_config(request.config.as_ref())?,
        scoped.namespace(),
    );
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let schedule_id = engine
        .create_schedule(config)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoCreateScheduleResponse {
        schedule_id: Some(schedule_id.into()),
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded update-schedule request.
///
/// Ownership is verified against the creation-recorded namespace before the
/// engine is touched, and the verified namespace is re-stamped onto the
/// replacement config, so an update can never migrate a schedule between
/// tenants.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID or config is missing or malformed, namespace
/// scoping or ownership verification fails, or the engine update/describe call fails.
pub async fn update_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoUpdateScheduleRequest,
) -> Result<ProtoUpdateScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let target = ScheduleTarget::schedule(&schedule_id);
    let scoped = guard
        .scope(
            caller,
            &NamespaceOperation::update_schedule(&request, target),
        )
        .await
        .map_err(|error| error.to_wire_error())?;
    let config = stamp_schedule_namespace(
        required_schedule_config(request.config.as_ref())?,
        scoped.namespace(),
    );
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .update_schedule(&schedule_id, config)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoUpdateScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded pause-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// or ownership verification fails, or the engine pause/describe call fails.
pub async fn pause_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoPauseScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let target = ScheduleTarget::schedule(&schedule_id);
    let scoped = guard
        .scope(
            caller,
            &NamespaceOperation::pause_schedule(&request, target),
        )
        .await
        .map_err(|error| error.to_wire_error())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .pause_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoPauseScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded resume-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// or ownership verification fails, or the engine resume/describe call fails.
pub async fn resume_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoResumeScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let target = ScheduleTarget::schedule(&schedule_id);
    let scoped = guard
        .scope(
            caller,
            &NamespaceOperation::resume_schedule(&request, target),
        )
        .await
        .map_err(|error| error.to_wire_error())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .resume_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoResumeScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded delete-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// or ownership verification fails, or the engine delete call fails.
pub async fn delete_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoDeleteScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let target = ScheduleTarget::schedule(&schedule_id);
    let scoped = guard
        .scope(
            caller,
            &NamespaceOperation::delete_schedule(&request, target),
        )
        .await
        .map_err(|error| error.to_wire_error())?;
    scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .delete_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    Ok(ProtoDeleteScheduleResponse {})
}

/// Handles a decoded list-schedules request.
///
/// The engine's projected schedule states are filtered to those stamped as
/// owned by the authorized namespace before encoding, so a shared engine never
/// leaks another tenant's schedules.
///
/// # Errors
///
/// Returns a stable [`WireError`] when namespace scoping fails, the engine list call fails, or
/// schedule states cannot be encoded.
pub async fn list_schedules(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoListSchedulesRequest,
) -> Result<ProtoListSchedulesResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::list_schedules(&request))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let schedules = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .list_schedules()
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?
        .into_iter()
        .filter(|state| schedule_in_namespace(state, &namespace))
        .map(|state| encode_schedule_state(namespace.clone(), None, &state))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProtoListSchedulesResponse { schedules })
}

/// Handles a decoded describe-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// or ownership verification fails, or the engine describe call fails.
pub async fn describe_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoDescribeScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let target = ScheduleTarget::schedule(&schedule_id);
    let scoped = guard
        .scope(
            caller,
            &NamespaceOperation::describe_schedule(&request, target),
        )
        .await
        .map_err(|error| error.to_wire_error())?;
    let state = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoDescribeScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use aion::{Engine, EngineBuilder, schedule::ScheduleState};
    use aion_core::{
        CatchUpPolicy, OverlapPolicy, Payload, ScheduleConfig, ScheduleId, SearchAttributeValue,
        TriggerSpec,
    };
    use aion_proto::{
        WireErrorCode,
        convert::{decode_schedule_state, encode_schedule_config},
    };
    use aion_store::{EventStore, InMemoryStore, visibility::VisibilityStore};
    use serde_json::json;

    use super::*;
    use crate::namespace::schedule_source::HistoryScheduleNamespaceSource;
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    const TENANT_A: &str = "tenant-a";
    const TENANT_B: &str = "tenant-b";

    struct TestContext {
        guard: NamespaceGuard,
        tenant_a: CallerIdentity,
        tenant_b: CallerIdentity,
        engine: Arc<Engine>,
    }

    async fn context() -> Result<TestContext, aion::EngineError> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .visibility_store_arc(visibility_store)
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(Arc::clone(&engine)),
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(HistoryScheduleNamespaceSource::new(Arc::clone(&engine))),
        );
        Ok(TestContext {
            guard: NamespaceGuard::new(resolver),
            tenant_a: CallerIdentity::new("alice", [TENANT_A.to_owned()]),
            tenant_b: CallerIdentity::new("bob", [TENANT_B.to_owned()]),
            engine,
        })
    }

    fn schedule_config(
        attributes: HashMap<String, SearchAttributeValue>,
    ) -> Result<ScheduleConfig, aion_core::PayloadError> {
        Ok(ScheduleConfig {
            trigger: TriggerSpec::Interval {
                period: Duration::from_secs(3600),
            },
            overlap_policy: OverlapPolicy::Skip,
            catch_up_policy: CatchUpPolicy::Skip,
            workflow_type: "fixture".to_owned(),
            input: Payload::from_json(&json!({ "fixture": true }))?,
            search_attributes: attributes,
        })
    }

    fn spoofed_attributes(namespace: &str) -> HashMap<String, SearchAttributeValue> {
        HashMap::from([(
            crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
            SearchAttributeValue::String(namespace.to_owned()),
        )])
    }

    fn create_request(
        namespace: &str,
        config: &ScheduleConfig,
    ) -> Result<ProtoCreateScheduleRequest, WireError> {
        Ok(ProtoCreateScheduleRequest {
            namespace: namespace.to_owned(),
            config: Some(encode_schedule_config(namespace, None, config)?),
        })
    }

    fn id_request(namespace: &str, schedule_id: &ScheduleId) -> ProtoScheduleIdRequest {
        ProtoScheduleIdRequest {
            namespace: namespace.to_owned(),
            schedule_id: Some(schedule_id.clone().into()),
        }
    }

    async fn create_in(
        context: &TestContext,
        caller: &CallerIdentity,
        namespace: &str,
    ) -> Result<ScheduleId, Box<dyn std::error::Error>> {
        let response = create_schedule(
            &context.guard,
            caller,
            create_request(namespace, &schedule_config(HashMap::new())?)?,
        )
        .await?;
        let schedule_id: ScheduleId = response
            .schedule_id
            .ok_or("create response missing schedule id")?
            .try_into()?;
        Ok(schedule_id)
    }

    fn state_namespace(state: &ScheduleState) -> Option<&SearchAttributeValue> {
        state
            .config
            .search_attributes
            .get(crate::namespace::NAMESPACE_ATTRIBUTE)
    }

    #[tokio::test]
    async fn create_overwrites_caller_supplied_namespace_stamp()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let request = create_request(TENANT_A, &schedule_config(spoofed_attributes(TENANT_B))?)?;

        let response = create_schedule(&context.guard, &context.tenant_a, request).await?;

        let state: ScheduleState =
            decode_schedule_state(response.state.as_ref().ok_or("state missing")?)?;
        assert_eq!(
            state_namespace(&state),
            Some(&SearchAttributeValue::String(TENANT_A.to_owned()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn owner_round_trip_succeeds_for_every_schedule_operation()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let schedule_id = create_in(&context, &context.tenant_a, TENANT_A).await?;

        describe_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;
        pause_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;
        resume_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;
        update_schedule(
            &context.guard,
            &context.tenant_a,
            ProtoUpdateScheduleRequest {
                namespace: TENANT_A.to_owned(),
                schedule_id: Some(schedule_id.clone().into()),
                config: Some(encode_schedule_config(
                    TENANT_A,
                    None,
                    &schedule_config(HashMap::new())?,
                )?),
            },
        )
        .await?;
        let listed = list_schedules(
            &context.guard,
            &context.tenant_a,
            ProtoListSchedulesRequest {
                namespace: TENANT_A.to_owned(),
            },
        )
        .await?;
        assert_eq!(listed.schedules.len(), 1);
        delete_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn cross_namespace_probes_are_indistinguishable_from_nonexistent()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let owned = create_in(&context, &context.tenant_a, TENANT_A).await?;
        let nonexistent = ScheduleId::new_v4();

        let probe = |schedule_id: ScheduleId| {
            let guard = context.guard.clone();
            let caller = context.tenant_b.clone();
            async move {
                let update = update_schedule(
                    &guard,
                    &caller,
                    ProtoUpdateScheduleRequest {
                        namespace: TENANT_B.to_owned(),
                        schedule_id: Some(schedule_id.clone().into()),
                        config: Some(encode_schedule_config(
                            TENANT_B,
                            None,
                            &schedule_config(HashMap::new())?,
                        )?),
                    },
                )
                .await
                .err()
                .ok_or("expected update rejection")?;
                let pause = pause_schedule(&guard, &caller, id_request(TENANT_B, &schedule_id))
                    .await
                    .err()
                    .ok_or("expected pause rejection")?;
                let resume = resume_schedule(&guard, &caller, id_request(TENANT_B, &schedule_id))
                    .await
                    .err()
                    .ok_or("expected resume rejection")?;
                let delete = delete_schedule(&guard, &caller, id_request(TENANT_B, &schedule_id))
                    .await
                    .err()
                    .ok_or("expected delete rejection")?;
                let describe =
                    describe_schedule(&guard, &caller, id_request(TENANT_B, &schedule_id))
                        .await
                        .err()
                        .ok_or("expected describe rejection")?;
                Ok::<_, Box<dyn std::error::Error>>([update, pause, resume, delete, describe])
            }
        };

        let foreign = probe(owned).await?;
        let absent = probe(nonexistent).await?;

        // Foreign-owned and nonexistent schedules must be byte-for-byte
        // identical NotFound errors across every targeted operation.
        for (foreign_error, absent_error) in foreign.iter().zip(absent.iter()) {
            assert_eq!(foreign_error.code, WireErrorCode::NotFound);
            assert_eq!(foreign_error, absent_error);
            assert_eq!(
                foreign_error.message,
                format!("schedule not found in namespace {TENANT_B}")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn list_schedules_is_filtered_to_the_authorized_namespace()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        // An unstamped schedule created outside the server's stamping boundary
        // (embedded engine use) is durably recorded but owned by no namespace:
        // it must appear in no tenant's list.
        let unstamped_id = context
            .engine
            .create_schedule(schedule_config(HashMap::new())?)
            .await?;
        let owned_a = create_in(&context, &context.tenant_a, TENANT_A).await?;
        let owned_b = create_in(&context, &context.tenant_b, TENANT_B).await?;

        let listed_a = list_schedules(
            &context.guard,
            &context.tenant_a,
            ProtoListSchedulesRequest {
                namespace: TENANT_A.to_owned(),
            },
        )
        .await?;
        let listed_b = list_schedules(
            &context.guard,
            &context.tenant_b,
            ProtoListSchedulesRequest {
                namespace: TENANT_B.to_owned(),
            },
        )
        .await?;

        let ids = |response: &ProtoListSchedulesResponse| -> Result<Vec<ScheduleId>, WireError> {
            response
                .schedules
                .iter()
                .map(|envelope| {
                    decode_schedule_state::<ScheduleState>(envelope).map(|state| state.schedule_id)
                })
                .collect()
        };
        assert_eq!(ids(&listed_a)?, vec![owned_a]);
        assert_eq!(ids(&listed_b)?, vec![owned_b]);
        assert!(!ids(&listed_a)?.contains(&unstamped_id));
        assert!(!ids(&listed_b)?.contains(&unstamped_id));
        Ok(())
    }

    #[tokio::test]
    async fn update_cannot_migrate_a_schedule_between_tenants()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let schedule_id = create_in(&context, &context.tenant_a, TENANT_A).await?;

        // The owner submits a replacement config spoof-stamped for tenant-b;
        // the handler re-stamps the verified owner namespace over it.
        update_schedule(
            &context.guard,
            &context.tenant_a,
            ProtoUpdateScheduleRequest {
                namespace: TENANT_A.to_owned(),
                schedule_id: Some(schedule_id.clone().into()),
                config: Some(encode_schedule_config(
                    TENANT_A,
                    None,
                    &schedule_config(spoofed_attributes(TENANT_B))?,
                )?),
            },
        )
        .await?;

        let described = describe_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;
        let state: ScheduleState =
            decode_schedule_state(described.state.as_ref().ok_or("state missing")?)?;
        assert_eq!(
            state_namespace(&state),
            Some(&SearchAttributeValue::String(TENANT_A.to_owned()))
        );

        let foreign_describe = describe_schedule(
            &context.guard,
            &context.tenant_b,
            id_request(TENANT_B, &schedule_id),
        )
        .await
        .err()
        .ok_or("expected foreign describe rejection")?;
        assert_eq!(foreign_describe.code, WireErrorCode::NotFound);

        let listed_b = list_schedules(
            &context.guard,
            &context.tenant_b,
            ProtoListSchedulesRequest {
                namespace: TENANT_B.to_owned(),
            },
        )
        .await?;
        assert!(listed_b.schedules.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn deleted_schedule_is_engine_not_found_for_owner_and_guard_not_found_for_foreign()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let schedule_id = create_in(&context, &context.tenant_a, TENANT_A).await?;
        delete_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await?;

        // Owner probes pass the ownership gate (deletion does not erase the
        // recorded owner) and surface the engine's typed ScheduleNotFound.
        let owner_describe = describe_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await
        .err()
        .ok_or("expected owner describe rejection")?;
        assert_eq!(owner_describe.code, WireErrorCode::NotFound);
        assert_eq!(
            owner_describe.error_type.as_deref(),
            Some("ScheduleNotFound")
        );

        let owner_redelete = delete_schedule(
            &context.guard,
            &context.tenant_a,
            id_request(TENANT_A, &schedule_id),
        )
        .await
        .err()
        .ok_or("expected owner re-delete rejection")?;
        assert_eq!(owner_redelete.code, WireErrorCode::NotFound);
        assert_eq!(
            owner_redelete.error_type.as_deref(),
            Some("ScheduleNotFound")
        );

        // A foreign probe of the deleted schedule must stay the guard's
        // anti-existence-leak NotFound — never the engine's typed error, which
        // would leak that the schedule ever existed.
        let foreign_describe = describe_schedule(
            &context.guard,
            &context.tenant_b,
            id_request(TENANT_B, &schedule_id),
        )
        .await
        .err()
        .ok_or("expected foreign describe rejection")?;
        assert_eq!(foreign_describe.code, WireErrorCode::NotFound);
        assert_eq!(foreign_describe.error_type, None);
        assert_eq!(
            foreign_describe.message,
            format!("schedule not found in namespace {TENANT_B}")
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_update_does_not_decode_config_before_namespace_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let guard = NamespaceGuard::new(resolver);
        let caller = CallerIdentity::new("alice", [TENANT_B.to_owned()]);
        let request = ProtoUpdateScheduleRequest {
            namespace: TENANT_A.to_owned(),
            schedule_id: Some(ScheduleId::new_v4().into()),
            config: Some(aion_proto::WireEnvelope {
                namespace: TENANT_A.to_owned(),
                request_id: None,
                payload: Some(aion_proto::convert::ProtoPayload {
                    content_type: "application/octet-stream".to_owned(),
                    bytes: Vec::new(),
                }),
            }),
        };

        let error = update_schedule(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    #[tokio::test]
    async fn foreign_targeted_update_does_not_decode_config_after_ownership_miss()
    -> Result<(), Box<dyn std::error::Error>> {
        let schedule_ownership = StaticScheduleNamespaces::default();
        let foreign_id = ScheduleId::new_v4();
        schedule_ownership.record(foreign_id.clone(), TENANT_B)?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            schedule_ownership,
        );
        let guard = NamespaceGuard::new(resolver);
        let caller = CallerIdentity::new("alice", [TENANT_A.to_owned()]);
        let request = ProtoUpdateScheduleRequest {
            namespace: TENANT_A.to_owned(),
            schedule_id: Some(foreign_id.into()),
            config: Some(aion_proto::WireEnvelope {
                namespace: TENANT_A.to_owned(),
                request_id: None,
                payload: Some(aion_proto::convert::ProtoPayload {
                    content_type: "application/octet-stream".to_owned(),
                    bytes: Vec::new(),
                }),
            }),
        };

        let error = update_schedule(&guard, &caller, request).await;

        // The malformed config envelope is never decoded: the ownership miss
        // rejects first with the anti-existence-leak NotFound.
        let error = error.err().ok_or("expected update rejection")?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(
            error.message,
            format!("schedule not found in namespace {TENANT_A}")
        );
        Ok(())
    }
}
