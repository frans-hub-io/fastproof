//! Normalized test result model of FastProof, shared by ingest, storage and the API.
//!
//! Identity hashes are compatible with Allure 2.45+ and Allure 3 so that Allure history
//! can be imported; see [`identity`].

pub mod attachment;
pub mod flaky;
pub mod identity;
pub mod launch;
pub mod normalize;
pub mod report;
pub mod result;
pub mod retry;
pub mod status;

pub use attachment::{AttachmentId, AttachmentKind, AttachmentRef};
pub use flaky::{FlakyOptions, Transition, is_flaky, status_transition, test_flakiness};
pub use identity::{
    ExplicitId, IdSource, IdentityInput, SourceRef, TestIdentity, UNKNOWN_PARAMETER_VALUE,
};
pub use launch::{CiInfo, Executor, KeyValue, LaunchMeta, VcsInfo};
pub use report::{ReportAttachment, ReportError, ReportResult, ReportSource, ReportStep};
pub use result::{
    BoundedText, Category, Container, Description, Fixtures, GlobalError, Globals, Label, Link,
    Parameter, ParameterMode, ParsedResult, SourceFormat, StatusDetails, Step, StepKind,
};
pub use retry::{AttemptKey, RetryGroup, group_attempts, group_retries};
pub use status::{Stage, Status};
