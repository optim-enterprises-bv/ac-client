//! TP-469 Error Codes
//!
//! All error codes defined in TR-369 Appendix A

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum ErrorCode {
    // Message errors (7000-7199)
    MessageNotUnderstood = 7000,
    RequestDenied = 7001,
    InternalError = 7002,
    InvalidArguments = 7003,
    ResourcesExceeded = 7004,
    InvalidBoundAnchor = 7005,
    InvalidPath = 7006,
    InvalidInstanceIdentifier = 7007,
    InvalidUniqueKey = 7008,
    DuplicateUniqueKey = 7009,
    InvalidReference = 7010,
    InvalidNamespace = 7011,
    
    // GET/SET/ADD/DELETE errors (7200-7299)
    ParameterNotWritable = 7200,
    InvalidValueType = 7201,
    InvalidValue = 7202,
    ParameterNotFound = 7203,
    RequiredParameterMissing = 7204,
    InvalidPathSyntax = 7205,
    ObjectNotFound = 7206,
    ObjectNotCreatable = 7207,
    ObjectNotDeletable = 7208,
    ReadOnlyObject = 7209,
    
    // OPERATE errors (7800-7899)
    CommandFailure = 7800,
    CommandNotSupported = 7801,
    CommandInvalidArguments = 7802,
    CommandInvalidInputArgs = 7803,
    CommandNotAllowed = 7804,
    CommandResourceExceeded = 7805,
    CommandRetryLater = 7806,
    
    // Notification/Subscription errors (7900-7999)
    SubscriptionNotAllowed = 7900,
    SubscriptionAlreadyExists = 7901,
    SubscriptionLimitExceeded = 7902,
    
    // Version negotiation
    UnsupportedVersion = 7903,
    
    // Not supported (generic - different value from ResourcesExceeded)
    NotSupported = 7020,
}

impl ErrorCode {
    pub fn as_u32(&self) -> u32 {
        *self as u32
    }
    
    pub fn description(&self) -> &'static str {
        match self {
            ErrorCode::MessageNotUnderstood => "The message was not understood by the Agent",
            ErrorCode::RequestDenied => "The request was denied",
            ErrorCode::InternalError => "An internal error occurred",
            ErrorCode::InvalidArguments => "Invalid arguments were provided",
            ErrorCode::ResourcesExceeded => "Resources have been exceeded",
            ErrorCode::InvalidBoundAnchor => "Invalid bound anchor",
            ErrorCode::InvalidPath => "Invalid path",
            ErrorCode::InvalidInstanceIdentifier => "Invalid instance identifier",
            ErrorCode::InvalidUniqueKey => "Invalid unique key",
            ErrorCode::DuplicateUniqueKey => "Duplicate unique key",
            ErrorCode::InvalidReference => "Invalid reference",
            ErrorCode::InvalidNamespace => "Invalid namespace",
            ErrorCode::ParameterNotWritable => "Parameter is not writable",
            ErrorCode::InvalidValueType => "Invalid value type",
            ErrorCode::InvalidValue => "Invalid value",
            ErrorCode::ParameterNotFound => "Parameter not found",
            ErrorCode::RequiredParameterMissing => "Required parameter missing",
            ErrorCode::InvalidPathSyntax => "Invalid path syntax",
            ErrorCode::ObjectNotFound => "Object not found",
            ErrorCode::ObjectNotCreatable => "Object is not creatable",
            ErrorCode::ObjectNotDeletable => "Object is not deletable",
            ErrorCode::ReadOnlyObject => "Object is read-only",
            ErrorCode::CommandFailure => "Command failed",
            ErrorCode::CommandNotSupported => "Command not supported",
            ErrorCode::CommandInvalidArguments => "Invalid command arguments",
            ErrorCode::CommandInvalidInputArgs => "Invalid command input arguments",
            ErrorCode::CommandNotAllowed => "Command not allowed",
            ErrorCode::CommandResourceExceeded => "Command resource exceeded",
            ErrorCode::CommandRetryLater => "Command retry later",
            ErrorCode::SubscriptionNotAllowed => "Subscription not allowed",
            ErrorCode::SubscriptionAlreadyExists => "Subscription already exists",
            ErrorCode::SubscriptionLimitExceeded => "Subscription limit exceeded",
            ErrorCode::UnsupportedVersion => "Unsupported version",
            ErrorCode::NotSupported => "Not supported",
        }
    }
}
