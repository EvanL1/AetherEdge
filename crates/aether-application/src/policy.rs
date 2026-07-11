//! Deny-by-default capability authorization and confirmation policy.

use crate::{ApplicationError, CapabilityDescriptor, ConfirmationPolicy, RequestContext};

/// Stateless safety policy applied before application use cases run.
#[derive(Debug, Default, Clone, Copy)]
pub struct SafetyPolicy;

impl SafetyPolicy {
    /// Authorizes one capability invocation.
    pub fn authorize(
        self,
        descriptor: CapabilityDescriptor,
        context: &RequestContext,
    ) -> Result<(), ApplicationError> {
        if !context
            .actor()
            .has_permission(descriptor.required_permission())
        {
            return Err(ApplicationError::PermissionDenied {
                capability: descriptor.name(),
                permission: descriptor.required_permission(),
            });
        }

        if matches!(descriptor.confirmation(), ConfirmationPolicy::Always) && !context.confirmed() {
            return Err(ApplicationError::ConfirmationRequired {
                capability: descriptor.name(),
            });
        }

        Ok(())
    }
}
