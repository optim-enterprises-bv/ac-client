//! TP-469 Error Codes
//!
//! All error codes defined in TR-369 Appendix A

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum ErrorCode {
    // Message errors (7000-7199)
    InternalError = 7002,
    ResourcesExceeded = 7004,
    InvalidInstanceIdentifier = 7007,

    // GET/SET/ADD/DELETE errors (7200-7299)
    RequiredParameterMissing = 7204,
    ObjectNotFound = 7206,
    ObjectNotCreatable = 7207,
    ObjectNotDeletable = 7208,
}

impl ErrorCode {
    pub fn as_u32(&self) -> u32 {
        *self as u32
    }
}
