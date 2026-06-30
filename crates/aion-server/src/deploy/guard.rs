//! Adapter-boundary deploy authorization.
//!
//! Deploy is not a data operation: loading a package registers code into the
//! shared BEAM VM and re-points routing for a workflow *type* that is
//! startable from every namespace. Namespace grants therefore authorize the
//! wrong thing — the deploy grant is deployment-wide, carried by the `deploy`
//! token claim (or the `x-aion-deploy` development header), and this guard
//! decides it before any handler logic runs.

use std::sync::Arc;

use aion::Engine;

use crate::error::ServerError;
use crate::namespace::resolver::{CallerIdentity, GrantSource, NamespaceResolver};

/// Adapter-boundary guard for the operator deploy API, the sibling of
/// [`crate::namespace::NamespaceGuard`] for engine-global operations.
#[derive(Clone)]
pub struct DeployGuard {
    resolver: NamespaceResolver,
}

impl DeployGuard {
    /// Build a guard from the shared namespace resolver (the engine owner).
    #[must_use]
    pub const fn new(resolver: NamespaceResolver) -> Self {
        Self { resolver }
    }

    /// Authorize a caller for the deploy surface before any handler logic.
    ///
    /// # Errors
    ///
    /// Returns a `deploy_denied` wire error when the transport already denied
    /// the caller (bad/missing credentials) or when the caller lacks the
    /// deploy grant; the denial hint names the knob that actually carries the
    /// grant, mirroring the namespace-denial pattern.
    pub fn authorize(&self, caller: &CallerIdentity) -> Result<(), ServerError> {
        if let Some(reason) = caller.denial_reason() {
            return Err(ServerError::deploy_denied(reason));
        }
        if caller.deploy_granted() {
            return Ok(());
        }
        Err(deploy_denied(caller))
    }

    /// Borrow the engine handle for an authorized deploy operation.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] only for guards constructed without an
    /// engine for unit tests.
    pub fn engine(&self) -> Result<&Arc<Engine>, ServerError> {
        self.resolver.engine()
    }
}

fn deploy_denied(caller: &CallerIdentity) -> ServerError {
    let subject = caller.subject();
    let hint = match caller.grant_source() {
        GrantSource::NamespacesHeader => {
            format!("set x-aion-deploy: true for subject `{subject}`")
        }
        GrantSource::TokenClaim => {
            format!("mint a token whose deploy claim is true for subject `{subject}`")
        }
        // An operator always holds the deploy grant (`deploy_granted` is true),
        // so this arm is never reached; keep the match exhaustive.
        GrantSource::Operator => {
            format!("subject `{subject}` is the operator and already holds the deploy grant")
        }
    };
    ServerError::deploy_denied(format!(
        "subject `{subject}` is not authorized to deploy; {hint}"
    ))
}

#[cfg(test)]
mod tests {
    use aion_proto::WireErrorCode;

    use super::DeployGuard;
    use crate::config::NamespaceMode;
    use crate::namespace::{
        CallerIdentity, NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
    };

    fn guard() -> DeployGuard {
        DeployGuard::new(NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        ))
    }

    #[test]
    fn granted_caller_is_authorized() -> Result<(), Box<dyn std::error::Error>> {
        let header_caller = CallerIdentity::new("ci", [String::from("tenant-a")]).with_deploy(true);
        let token_caller =
            CallerIdentity::from_token_claims("ci", [String::from("tenant-a")]).with_deploy(true);

        guard().authorize(&header_caller)?;
        guard().authorize(&token_caller)?;
        Ok(())
    }

    /// The denial hint must point at the knob that actually carries the
    /// deploy grant: the development `x-aion-deploy` header for
    /// header-sourced identities, the token's deploy claim for identities
    /// produced by the JWT path (mirrors
    /// `denial_hint_names_the_grant_source`).
    #[test]
    fn denial_hint_names_the_grant_source() -> Result<(), Box<dyn std::error::Error>> {
        let header_caller = CallerIdentity::new("ci", [String::from("tenant-a")]);
        let header_denial = guard()
            .authorize(&header_caller)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected header-sourced caller to be denied")?;
        assert_eq!(header_denial.code, WireErrorCode::DeployDenied);
        assert!(
            header_denial
                .message
                .contains("subject `ci` is not authorized to deploy"),
            "denial must name the subject: {}",
            header_denial.message
        );
        assert!(
            header_denial.message.contains("x-aion-deploy"),
            "header-path denial must hint the dev header: {}",
            header_denial.message
        );
        assert!(
            !header_denial.message.contains("deploy claim"),
            "header-path denial must not hint the token claim: {}",
            header_denial.message
        );

        let token_caller = CallerIdentity::from_token_claims("ci", [String::from("tenant-a")]);
        let token_denial = guard()
            .authorize(&token_caller)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected token-sourced caller to be denied")?;
        assert_eq!(token_denial.code, WireErrorCode::DeployDenied);
        assert!(
            token_denial.message.contains("deploy claim"),
            "JWT-path denial must hint the token's deploy claim: {}",
            token_denial.message
        );
        assert!(
            !token_denial.message.contains("x-aion-deploy"),
            "JWT-path denial must not hint the dev header: {}",
            token_denial.message
        );
        Ok(())
    }

    /// A transport-level credential failure stays a deploy denial carrying
    /// the transport's specific reason.
    #[test]
    fn transport_denied_caller_is_deploy_denied_with_reason()
    -> Result<(), Box<dyn std::error::Error>> {
        let denied = CallerIdentity::denied("ci", "invalid bearer token").with_deploy(true);

        let error = guard()
            .authorize(&denied)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected transport-denied caller to be refused")?;
        assert_eq!(error.code, WireErrorCode::DeployDenied);
        assert!(
            error.message.contains("invalid bearer token"),
            "denial must carry the transport reason: {}",
            error.message
        );
        Ok(())
    }

    /// Namespace grants must not leak into the deploy decision: a caller
    /// with every namespace but no deploy grant is denied.
    #[test]
    fn namespace_grants_do_not_imply_deploy() {
        let caller = CallerIdentity::new("ci", [String::from("tenant-a"), String::from("b")]);

        let result = guard().authorize(&caller);
        assert_eq!(
            result.err().map(|error| error.to_wire_error().code),
            Some(WireErrorCode::DeployDenied)
        );
    }
}
