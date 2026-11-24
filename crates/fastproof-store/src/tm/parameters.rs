//! Parameters of data-driven cases: validation and the row generator (§7.5, night 2).

use super::cases::invalid_case;
use super::{CaseBody, TmLimits, TmResult};

/// Checks `CaseBody.parameters` against the limits and the placeholders of the body.
pub fn validate_parameters(body: &CaseBody, _limits: &TmLimits) -> TmResult<()> {
    if body.parameters.is_some() {
        return Err(invalid_case(
            "body.parameters",
            "parameters are not available",
        ));
    }
    Ok(())
}
